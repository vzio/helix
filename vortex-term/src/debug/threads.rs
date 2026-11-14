//! Spectating threads and scheduling them by hand: which thread playback follows, cycling it,
//! freezing and thawing threads, running one thread alone, and continuing or pausing a single
//! thread, so a suspected race can be forced (step A to the check, run B through, continue A).
//!
//! Only what the adapter allows is offered: running one thread alone needs DAP `singleThread`
//! (`supportsSingleThreadExecutionRequests`), except between sessions (js-debug runs each worker
//! in its own session), which never run each other's threads.

use vortex_dap::{registry::DebugAdapterId, StepKind, ThreadId};
use vortex_view::{
    debug::{spectator::ThreadKey, Section},
    Editor,
};

use super::{load, playback, rewind};
use crate::job;

/// The program runs again after a request for `key` (unless it stopped since: the stop's event
/// can come before the request's answer).
pub fn mark_resumed(editor: &mut Editor, key: ThreadKey, generation: u64) {
    if editor.debug.generation != generation {
        return;
    }
    let Some(client) = editor.debug_adapters.get_client_mut(key.session) else {
        return;
    };
    if client.thread_id == Some(key.thread) || client.thread_id.is_none() {
        client.resume_application();
    }
    client
        .thread_states
        .insert(key.thread, "running".to_owned());
    if editor.debug_adapters.get_active_client().map(|c| c.id()) == Some(key.session) {
        editor.debug.resumed();
    }
}

/// Why freezing threads or running one alone isn't offered in `session`, if it isn't: the
/// adapter would run every thread of the session on each step.
pub fn scheduling_unavailable(editor: &Editor, session: DebugAdapterId) -> Option<&'static str> {
    if playback::supports_single_thread(editor, session) {
        return None;
    }
    if editor.debug.threads_of(session).len() <= 1 {
        // a session of one thread: its neighbours are other sessions, which it never runs
        return None;
    }
    Some("This debug adapter runs every thread on each step (no singleThread support)")
}

/// Spectates `key`: playback follows it, and when it is stopped the panel and the editor show
/// it.
pub fn spectate(editor: &mut Editor, key: ThreadKey) {
    rewind::to_live(editor);
    if editor.debug.spectator.spectated != Some(key) && editor.debug.spectator.is_playing() {
        playback::pause(editor);
    }
    editor.debug.spectator.spectated = Some(key);
    let name = editor.debug.thread_name(key);
    if playback::is_stopped(editor, key) {
        // the first frame whose code is here: a thread waiting in libc shows where it waits
        let index = editor
            .debug_adapters
            .get_client(key.session)
            .and_then(|client| client.stack_frames.get(&key.thread))
            .and_then(|frames| {
                frames.iter().position(|frame| {
                    frame
                        .source
                        .as_ref()
                        .and_then(|source| source.path.as_ref())
                        .is_some_and(|path| path.exists())
                })
            })
            .unwrap_or(0);
        load::select_frame(editor, key, index);
        crate::debug::mode::say(editor, format!("Spectating thread {} {name}", key.thread));
    } else {
        crate::debug::mode::say(
            editor,
            format!("Spectating thread {} {name} (running)", key.thread),
        );
    }
    super::engine::sync_watch(editor);
}

/// Spectates the next (`by` = 1) or previous (-1) thread, across sessions.
pub fn cycle(editor: &mut Editor, by: isize) {
    let all: Vec<ThreadKey> = editor
        .debug
        .all_threads()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    if all.is_empty() {
        editor.set_error("No threads");
        return;
    }
    let current = playback::spectated(editor).and_then(|key| all.iter().position(|k| *k == key));
    let index = match current {
        Some(index) => (index as isize + by).rem_euclid(all.len() as isize) as usize,
        None => 0,
    };
    spectate(editor, all[index]);
}

/// The thread under the cursor in the call stack, else the spectated one.
pub fn selected_or_spectated(editor: &Editor) -> Option<ThreadKey> {
    if editor.debug.is_focused() && editor.debug.section == Section::Stack {
        if let Some(key) = super::panel::current_thread(editor) {
            return Some(key);
        }
    }
    playback::spectated(editor)
}

/// Freezes or thaws `key`. A frozen thread never runs: steps and continues of the others run
/// them alone, and it can't be stepped or continued itself. A running thread is paused as it
/// freezes.
pub fn toggle_freeze(editor: &mut Editor, key: ThreadKey) {
    if let Some(reason) = scheduling_unavailable(editor, key.session) {
        editor.set_error(format!("{reason}: threads can't be frozen"));
        return;
    }
    let frozen = editor.debug.spectator.toggle_frozen(key);
    // the client's fast path continues other threads' hits alone while a thread is frozen
    super::engine::sync_watch(editor);
    let name = editor.debug.thread_name(key);
    if frozen {
        if !playback::is_stopped(editor, key) {
            pause(editor, key);
        }
        crate::debug::mode::say(
            editor,
            format!(
                "Thread {} {name} is frozen: it won't run until thawed",
                key.thread
            ),
        );
    } else {
        crate::debug::mode::say(editor, format!("Thread {} {name} is thawed", key.thread));
    }
}

/// Turns "only the spectated thread runs" on or off, for steps, continues and playback.
pub fn toggle_solo(editor: &mut Editor) {
    let session = playback::spectated(editor)
        .map(|key| key.session)
        .or_else(|| editor.debug_adapters.get_active_client().map(|c| c.id()));
    if let Some(reason) = session.and_then(|session| scheduling_unavailable(editor, session)) {
        editor.set_error(format!("{reason}: other threads run during steps"));
        return;
    }
    let spectator = &mut editor.debug.spectator;
    spectator.solo = !spectator.solo;
    crate::debug::mode::say(
        editor,
        if editor.debug.spectator.solo {
            "Only the spectated thread runs during steps and continues"
        } else {
            "Every thread that isn't frozen runs during steps and continues"
        },
    );
}

/// `p`: continues `key` if it is stopped, pauses it if it runs.
pub fn pause_or_continue(editor: &mut Editor, key: ThreadKey) {
    rewind::to_live(editor);
    if playback::is_stopped(editor, key) {
        continue_thread(editor, key);
    } else {
        pause(editor, key);
    }
}

/// Pauses one thread (the adapter may stop the others with it).
pub fn pause(editor: &mut Editor, key: ThreadKey) {
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        return;
    };
    let request = client.requester().pause(key.thread);
    tokio::spawn(async move {
        if let Err(err) = request.await {
            job::dispatch(move |editor, _| editor.set_error(format!("Failed to pause: {err}")))
                .await;
        }
    });
}

/// Continues `key`: alone when it must (solo, or a neighbour is frozen). The answer says
/// whether the adapter continued the other threads too.
pub fn continue_thread(editor: &mut Editor, key: ThreadKey) {
    if editor.debug.spectator.is_frozen(key) {
        editor.set_error("This thread is frozen: thaw it to continue it");
        return;
    }
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        return;
    };
    let alone = playback::single_thread(editor, key);
    let request = client.requester().continue_with(key.thread, alone);
    let generation = editor.debug.generation;
    tokio::spawn(async move {
        let result = request.await;
        job::dispatch(move |editor, _| match result {
            Ok(all) => {
                let stopped_since = editor.debug.generation != generation;
                mark_resumed(editor, key, generation);
                if all != Some(false) && !stopped_since {
                    // every thread of the session runs
                    if let Some(client) = editor.debug_adapters.get_client_mut(key.session) {
                        for state in client.thread_states.values_mut() {
                            *state = "running".to_owned();
                        }
                    }
                }
                resume_others(editor, key);
            }
            Err(err) => editor.set_error(format!("Failed to continue: {err}")),
        })
        .await;
    });
}

/// In a session whose threads stop one by one, a step or continue of `key` sent alone because
/// of a frozen neighbour leaves the threads that aren't frozen stopped: they continue too.
fn resume_others(editor: &mut Editor, key: ThreadKey) {
    let spectator = &editor.debug.spectator;
    if spectator.solo || !spectator.non_stop.contains(&key.session) || spectator.frozen.is_empty() {
        return;
    }
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        return;
    };
    let requester = client.requester();
    let others: Vec<ThreadId> = editor
        .debug
        .threads_of(key.session)
        .iter()
        .map(|thread| thread.id)
        .filter(|thread| {
            *thread != key.thread
                && !editor.debug.spectator.is_frozen(ThreadKey {
                    session: key.session,
                    thread: *thread,
                })
                && client.thread_states.get(thread).map(String::as_str) != Some("running")
        })
        .collect();
    for thread in others {
        let request = requester.continue_with(thread, true);
        tokio::spawn(async move {
            if let Err(err) = request.await {
                log::debug!("continue of thread {thread}: {err}");
            }
        });
    }
}

/// A step from the editor (debug mode's `.`, `i`, `o`) of the current thread, honouring freeze and solo.
pub fn step(editor: &mut Editor, kind: StepKind) {
    rewind::to_live(editor);
    if editor.debug.spectator.is_playing() {
        playback::pause(editor);
    }
    let Some(client) = editor.debug_adapters.get_active_client() else {
        editor.set_error("No debug session");
        return;
    };
    let Some(thread) = client.thread_id else {
        editor.set_error("The current thread is not stopped");
        return;
    };
    let key = ThreadKey {
        session: client.id(),
        thread,
    };
    if editor.debug.spectator.is_frozen(key) {
        editor.set_error("This thread is frozen: thaw it to step it");
        return;
    }
    if super::engine::manual_step(editor, key, kind) {
        // the hidden breakpoints stop it at its next line, and the step ends where it would
        return;
    }
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        return;
    };
    let requester = client.requester();
    let alone = playback::single_thread(editor, key);
    let granularity = playback::granularity(editor, key.session);
    let generation = editor.debug.generation;
    let request = requester.step(kind, thread, granularity, alone);
    tokio::spawn(async move {
        let result = request.await;
        job::dispatch(move |editor, _| match result {
            Ok(()) => {
                mark_resumed(editor, key, generation);
                resume_others(editor, key);
            }
            Err(err) => editor.set_error(format!("Failed to step: {err}")),
        })
        .await;
    });
}

/// The adapter's `thread` event: a short notice, and the thread list follows.
pub fn event(editor: &mut Editor, session: DebugAdapterId, reason: &str, thread: ThreadId) {
    let key = ThreadKey { session, thread };
    match reason {
        "started" => {
            editor.debug.engine.record.thread_started(key);
            let mut threads = editor.debug.threads_of(session).to_vec();
            if !threads.iter().any(|t| t.id == thread) {
                threads.push(vortex_dap::Thread {
                    id: thread,
                    name: format!("Thread {thread}"),
                });
                editor.debug.set_threads(session, threads);
            }
            log::info!("thread {thread} joined");
        }
        "exited" => {
            editor.debug.engine.record.thread_exited(key);
            let mut threads = editor.debug.threads_of(session).to_vec();
            threads.retain(|t| t.id != thread);
            editor.debug.set_threads(session, threads);
            if let Some(client) = editor.debug_adapters.get_client_mut(session) {
                client.thread_states.remove(&thread);
                client.stack_frames.remove(&thread);
            }
            let spectator = &mut editor.debug.spectator;
            spectator.frozen.remove(&key);
            if spectator.spectated == Some(key) {
                spectator.spectated = None;
                if spectator.is_playing() {
                    spectator.pause();
                }
                log::info!("thread {thread} left: it was spectated");
            } else {
                log::info!("thread {thread} left");
            }
        }
        _ => {}
    }
}
