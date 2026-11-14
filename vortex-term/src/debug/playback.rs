//! Spectator mode's playback: automatic stepping at a chosen speed (step, wait for the stop,
//! draw it, wait out the speed's interval, repeat), single steps, fast forward to the first
//! change of a watched value, and run to cursor.
//!
//! A playback step is a `stepIn`, then `stepOut`s while the frame it lands in isn't code the
//! mode follows (std, lib or own), all counted as one step. Breakpoints, exceptions, a pause and
//! the program's end pause playback. Nothing waits: requests run in background tasks and come
//! back through the job queue, and a pause bumps the playback token so a scheduled step from
//! before is dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use vortex_dap::{
    registry::DebugAdapterId, Requester, StackFrame, StepKind, SteppingGranularity, ThreadId,
};
use vortex_view::{
    debug::{
        engine::LINE_REASON,
        spectator::{
            Change, Granularity, Mode, OriginRules, RunTo, StepInFlight, ThreadKey, BLOCKED_AFTER,
            MAX_STEP_OUTS,
        },
        LineKind, Status, Tree,
    },
    handlers::dap::jump_to_stack_frame,
    Editor,
};

use super::{breakpoints, cpu, load, panel, record, rewind, threads};
use crate::job;

/// How a stop is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    /// Jump to it and load its values. `follow`: playing, so the view only scrolls when the line
    /// leaves it, instead of centering.
    Full { follow: bool },
    /// Not at all: a stop on the way (a step out of library code, a fast forward step).
    Quiet,
}

/// The thread playback follows: the spectated one while its session lives, else the current
/// thread of the session the panel follows.
pub fn spectated(editor: &Editor) -> Option<ThreadKey> {
    let spectator = &editor.debug.spectator;
    if let Some(key) = spectator
        .spectated
        .filter(|key| editor.debug_adapters.get_client(key.session).is_some())
    {
        return Some(key);
    }
    let client = editor.debug_adapters.get_active_client()?;
    Some(ThreadKey {
        session: client.id(),
        thread: client.thread_id?,
    })
}

/// Whether `key` is stopped, as far as the editor knows.
pub fn is_stopped(editor: &Editor, key: ThreadKey) -> bool {
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        return false;
    };
    match client.thread_states.get(&key.thread) {
        Some(state) => state != "running",
        None => {
            client.thread_id == Some(key.thread)
                || (matches!(editor.debug.status, Status::Stopped { .. })
                    && editor.debug_adapters.get_active_client().map(|c| c.id())
                        == Some(key.session))
        }
    }
}

/// Whether the session's adapter runs one thread alone on request (`singleThread`).
pub fn supports_single_thread(editor: &Editor, session: DebugAdapterId) -> bool {
    editor
        .debug_adapters
        .get_client(session)
        .and_then(|client| client.caps.as_ref())
        .and_then(|caps| caps.supports_single_thread_execution_requests)
        .unwrap_or_default()
}

/// Whether the session's adapter offers a step granularity.
pub fn supports_granularity(editor: &Editor, session: DebugAdapterId) -> bool {
    editor
        .debug_adapters
        .get_client(session)
        .and_then(|client| client.caps.as_ref())
        .and_then(|caps| caps.supports_stepping_granularity)
        .unwrap_or_default()
}

/// The granularity to send: the chosen one where the adapter offers a choice.
pub fn granularity(editor: &Editor, session: DebugAdapterId) -> Option<SteppingGranularity> {
    if !supports_granularity(editor, session) {
        return None;
    }
    Some(match editor.debug.spectator.granularity {
        Granularity::Line => SteppingGranularity::Line,
        Granularity::Statement => SteppingGranularity::Statement,
        Granularity::Instruction => SteppingGranularity::Instruction,
    })
}

/// Whether a step or continue of `key` is sent with `singleThread`.
pub fn single_thread(editor: &Editor, key: ThreadKey) -> bool {
    editor.debug.spectator.runs_alone(key) && supports_single_thread(editor, key.session)
}

/// The rules that classify frames for the session's language.
pub fn origin_rules(editor: &Editor, session: DebugAdapterId) -> OriginRules {
    let config = editor
        .debug_adapters
        .get_client(session)
        .and_then(|client| client.config.as_ref());
    // the folders the configuration names, else the workspace and the folders the launch
    // names (`cwd`, the program's)
    if let Some(own) = config.and_then(|config| config.own_paths.clone()) {
        return OriginRules::new(
            own.into_iter().filter(|path| path.is_absolute()).collect(),
            config.and_then(|config| config.std_paths.clone()),
            config.and_then(|config| config.lib_paths.clone()),
        );
    }
    let mut roots = vec![vortex_loader::find_workspace().0];
    let args = editor
        .debug_adapters
        .get_client(session)
        .and_then(|client| client.starting_request_args());
    for key in ["cwd", "program"] {
        let Some(path) = args.and_then(|args| args.get(key)?.as_str()) else {
            continue;
        };
        let path = std::path::Path::new(path);
        let dir = if key == "program" {
            path.parent()
        } else {
            Some(path)
        };
        if let Some(dir) = dir.filter(|dir| dir.is_absolute() && dir.parent().is_some()) {
            if !roots.iter().any(|root| dir.starts_with(root)) {
                roots.push(dir.to_owned());
            }
        }
    }
    OriginRules::new(
        roots,
        config.and_then(|config| config.std_paths.clone()),
        config.and_then(|config| config.lib_paths.clone()),
    )
}

/// The thread id as the OS knows it, when the adapter uses OS thread ids (lldb does).
fn os_thread(thread: ThreadId) -> Option<u64> {
    thread.to_string().parse().ok()
}

fn cpu_now(editor: &Editor, thread: ThreadId) -> Option<Duration> {
    let pid = editor.debug.spectator.pid?;
    cpu::cpu_time(pid, os_thread(thread)).map(|(time, _)| time)
}

/// Space: plays, or pauses.
pub fn toggle(editor: &mut Editor) {
    if editor.debug.spectator.is_playing() {
        pause(editor);
    } else {
        play(editor, Mode::Playing);
    }
}

/// Starts playing (or fast forwarding) the spectated thread. A running thread is paused first;
/// playback starts at its stop.
pub fn play(editor: &mut Editor, mode: Mode) {
    rewind::to_live(editor);
    if editor.debug_adapters.get_active_client().is_none() {
        editor.set_error("No debug session to play");
        return;
    }
    let Some(key) = spectated(editor) else {
        editor.set_error("No thread to follow: the program hasn't stopped yet");
        return;
    };
    if editor.debug.spectator.is_frozen(key) {
        editor.set_error("The spectated thread is frozen: thaw it to play it");
        return;
    }
    if mode == Mode::FastForward {
        let baseline = watched_values(editor);
        if baseline.is_empty() {
            editor.set_error("Nothing to fast forward to: watch an expression or mark a variable");
            return;
        }
        editor.debug.spectator.baseline = baseline;
    }
    editor.debug.spectator.spectated = Some(key);
    super::engine::sync_watch(editor);
    editor.debug.spectator.play(mode);
    if editor.debug.spectator.step.is_some() {
        // the step in flight goes on, and playback with it
        return;
    }
    if is_stopped(editor, key) {
        send_step(editor, false);
    } else {
        log::info!("pausing the thread to play it");
        pause_thread(editor, key);
    }
}

/// Pauses playback. A step that is blocked gets a `pause`, so the thread stops where it waits.
pub fn pause(editor: &mut Editor) {
    let was_fast = editor.debug.spectator.mode == Mode::FastForward;
    editor.debug.spectator.pause();
    let now = Instant::now();
    if let Some(key) = editor
        .debug
        .spectator
        .blocked(now)
        .map(|(step, _)| step.key)
    {
        pause_thread(editor, key);
        return;
    }
    if was_fast && editor.debug.spectator.step.is_none() {
        // the stops of the fast forward weren't shown
        show_current(editor, false);
    }
}

/// `n`: one playback step (following std, lib or own); pauses playback first.
pub fn step_once(editor: &mut Editor) {
    rewind::to_live(editor);
    if editor.debug.spectator.is_playing() {
        pause(editor);
        return;
    }
    if editor.debug.spectator.step.is_some() {
        editor.set_error("A step is still running");
        return;
    }
    let Some(key) = spectated(editor) else {
        editor.set_error("No thread to step");
        return;
    };
    if !is_stopped(editor, key) {
        editor.set_error("The thread runs: pause it first");
        return;
    }
    send_step(editor, true);
}

pub(crate) fn pause_thread(editor: &mut Editor, key: ThreadKey) {
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

/// Playback's next line of the spectated thread (once the engine's hidden breakpoints are back,
/// when it waited for them).
pub fn advance(editor: &mut Editor) {
    send_step(editor, false);
}

/// Sends the next playback step of the spectated thread: a `continue` to its next line where
/// the engine's hidden breakpoints are in place, else a step.
fn send_step(editor: &mut Editor, single: bool) {
    let Some(key) = spectated(editor) else {
        editor.debug.spectator.pause();
        return;
    };
    if editor.debug.spectator.is_frozen(key) {
        editor.debug.spectator.pause();
        editor.set_error("The spectated thread is frozen");
        return;
    }
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        editor.debug.spectator.pause();
        return;
    };
    let frames = client
        .stack_frames
        .get(&key.thread)
        .cloned()
        .unwrap_or_default();
    let requester = client.requester();
    let top = frames.first().cloned();
    let how = super::engine::advance_kind(editor, key, top.as_ref());
    if how == super::engine::Advance::Wait {
        // the hidden breakpoints are on their way back: playback goes on once they are
        editor.debug.engine.pending_advance = true;
        return;
    }
    let continued = how == super::engine::Advance::Continue;
    let from = top.as_ref().and_then(|frame| {
        let path = frame.source.as_ref()?.path.clone()?;
        Some((path, frame.line.saturating_sub(1)))
    });
    let single_thread = single_thread(editor, key);
    let granularity = granularity(editor, key.session);
    let cpu_before = cpu_now(editor, key.thread);
    let alive = Arc::new(AtomicBool::new(true));
    let now = Instant::now();
    let generation = editor.debug.generation;
    let spectator = &mut editor.debug.spectator;
    spectator.shown_frame = top.clone();
    spectator.last_sent = Some(now);
    spectator.step = Some(StepInFlight {
        key,
        sent: now,
        requests: 1,
        request_sent: now,
        times: Vec::new(),
        from,
        function: top.map(|frame| frame.name).unwrap_or_default(),
        depth: frames.len(),
        cpu_before,
        single,
        continued,
        alive: alive.clone(),
    });
    log::info!("playback: advance (continued: {continued})");
    if continued {
        send_continue(requester, key, single_thread, generation);
    } else {
        send_request(
            requester,
            key,
            StepKind::In,
            granularity,
            single_thread,
            generation,
        );
    }
    blocked_timer(alive);
}

/// Continues the spectated thread to its next line (the engine's hidden breakpoints stop it
/// there).
fn send_continue(requester: Requester, key: ThreadKey, single_thread: bool, generation: u64) {
    let request = requester.continue_with(key.thread, single_thread);
    tokio::spawn(async move {
        let result = request.await;
        job::dispatch(move |editor, _| match result {
            Ok(all) => {
                let stopped_since = editor.debug.generation != generation;
                threads::mark_resumed(editor, key, generation);
                if all != Some(false) && !stopped_since {
                    if let Some(client) = editor.debug_adapters.get_client_mut(key.session) {
                        for state in client.thread_states.values_mut() {
                            *state = "running".to_owned();
                        }
                    }
                }
            }
            Err(err) => {
                editor.debug.spectator.finish_step();
                editor.debug.spectator.pause();
                editor.set_error(format!("Playback stopped: the continue failed: {err}"));
            }
        })
        .await;
    });
}

/// Sends one step request; the program shows as running once the adapter answers (unless it
/// stopped already).
fn send_request(
    requester: Requester,
    key: ThreadKey,
    kind: StepKind,
    granularity: Option<SteppingGranularity>,
    single_thread: bool,
    generation: u64,
) {
    let request = requester.step(kind, key.thread, granularity, single_thread);
    tokio::spawn(async move {
        let result = request.await;
        job::dispatch(move |editor, _| match result {
            Ok(()) => threads::mark_resumed(editor, key, generation),
            Err(err) => {
                editor.debug.spectator.finish_step();
                editor.debug.spectator.pause();
                editor.set_error(format!("Playback stopped: the step failed: {err}"));
            }
        })
        .await;
    });
}

/// While a step runs past [`BLOCKED_AFTER`], redraws a few times a second so its timer moves.
fn blocked_timer(alive: Arc<AtomicBool>) {
    tokio::spawn(async move {
        tokio::time::sleep(BLOCKED_AFTER).await;
        while alive.load(Ordering::Relaxed) {
            job::dispatch(|_, _| {}).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });
}

/// A session stopped, and the stack of the thread is loaded: what playback makes of it, and how
/// to show it. Every shown stop is recorded for rewind.
pub fn on_stopped(
    editor: &mut Editor,
    id: DebugAdapterId,
    thread: ThreadId,
    reason: &str,
    frames: &[StackFrame],
) -> Show {
    let key = ThreadKey {
        session: id,
        thread,
    };
    super::engine::on_frames(editor, key, frames);
    finish_run_to(editor, frames);
    let line_hit = reason == LINE_REASON;
    if line_hit
        && editor.debug.spectator.step.is_none()
        && !super::engine::manual_step_ends(editor, key, frames)
    {
        // a manual step goes on past this line
        return Show::Quiet;
    }
    let arrived = editor
        .debug
        .spectator
        .stop_arrived
        .take()
        .unwrap_or_else(Instant::now);
    if let Some(step) = editor
        .debug
        .spectator
        .step
        .as_mut()
        .filter(|step| step.key.session == id)
    {
        step.times
            .push(arrived.saturating_duration_since(step.request_sent));
    }
    let in_flight = editor
        .debug
        .spectator
        .step
        .as_ref()
        .filter(|step| step.key.session == id)
        .cloned();

    let Some(step) = in_flight else {
        // a breakpoint, a manual step, or the pause asked for to start playing
        editor.debug.spectator.spectated = Some(key);
        editor.debug.engine.others_since = 0;
        let done = record::capture(editor, id, key, reason, frames.to_vec());
        let spectator = &editor.debug.spectator;
        if spectator.is_playing() {
            if line_hit || matches!(reason, "pause" | "step" | "entry" | "goto") {
                schedule(editor, done);
            } else {
                editor.debug.spectator.pause();
                crate::debug::mode::say(editor, format!("Playback paused: {reason}"));
            }
        }
        return Show::Full { follow: false };
    };

    if !(reason == "step" || line_hit) || step.key != key {
        // something else stopped it: a breakpoint, an exception, a pause (the step of this thread
        // still took its time: a step that ends on a breakpoint is timed)
        if let Some(step) = editor.debug.spectator.finish_step() {
            if step.key == key {
                time_step(editor, &step, frames);
            }
        }
        editor.debug.spectator.spectated = Some(key);
        if editor.debug.spectator.is_playing() {
            editor.debug.spectator.pause();
            crate::debug::mode::say(editor, format!("Playback paused: {reason}"));
        }
        record::capture(editor, id, key, reason, frames.to_vec());
        return Show::Full { follow: false };
    }

    // outside the code the mode follows: step out, as part of the same step
    let allowed = frames.first().is_none_or(|top| {
        let origin = origin_rules(editor, id).classify(top);
        editor.debug.spectator.follow.allows(origin)
    });
    if !step.continued && !allowed && frames.len() > 1 && step.requests <= MAX_STEP_OUTS {
        let Some(client) = editor.debug_adapters.get_client(id) else {
            return Show::Quiet;
        };
        let requester = client.requester();
        if let Some(step) = editor.debug.spectator.step.as_mut() {
            step.requests += 1;
            step.request_sent = Instant::now();
        }
        let single_thread = single_thread(editor, key);
        let granularity = granularity(editor, id);
        let generation = editor.debug.generation;
        send_request(
            requester,
            key,
            StepKind::Out,
            granularity,
            single_thread,
            generation,
        );
        return Show::Quiet;
    }

    // the step is done
    let step = editor.debug.spectator.finish_step().unwrap_or(step);
    time_step(editor, &step, frames);
    editor.debug.spectator.steps += 1;
    editor.debug.spectator.shown_frame = frames.first().cloned();
    let done = record::capture(editor, id, key, reason, frames.to_vec());

    // reached the line playback runs to
    if let Some(target) = editor.debug.spectator.run_to.clone() {
        if at_line(frames, &target) {
            editor.debug.spectator.run_to = None;
            editor.debug.spectator.pause();
            crate::debug::mode::say(
                editor,
                format!("Reached the cursor (line {})", target.line + 1),
            );
            return Show::Full { follow: true };
        }
    }

    match editor.debug.spectator.mode {
        Mode::Paused => Show::Full { follow: true },
        Mode::Playing => {
            schedule(editor, done);
            Show::Full { follow: true }
        }
        Mode::FastForward => {
            fast_forward_check(editor, key, frames.first().map(|frame| frame.id), done);
            Show::Quiet
        }
    }
}

/// Gives the line a step left its time (and CPU time), and keeps it on the lineline with its
/// frame.
fn time_step(editor: &mut Editor, step: &StepInFlight, frames: &[StackFrame]) {
    let mut plain = frames.len() == step.depth
        && frames.first().map(|frame| frame.name.as_str()) == Some(step.function.as_str());
    let cpu = step
        .cpu_before
        .zip(cpu_now(editor, step.key.thread))
        .map(|(before, after)| after.saturating_sub(before));
    let mut first = step.times.first().copied().unwrap_or_default();
    // in a session that stops every thread at a hit, each hit of another thread on the way
    // cost a round trip
    let others = std::mem::take(&mut editor.debug.engine.others_since);
    if step.continued && others > 0 {
        plain = false;
        if !editor.debug.spectator.non_stop.contains(&step.key.session) {
            let overhead = editor.debug.spectator.timing.overhead().unwrap_or_default();
            first = first.saturating_sub(overhead * others);
        }
    }
    let outs = step.times.get(1..).unwrap_or_default();
    let corrected = editor.debug.spectator.timing.correct(first, outs);
    editor
        .debug
        .spectator
        .timing
        .record(step.from.clone(), first, outs, plain, cpu);
    let at = editor
        .debug
        .engine
        .last_stop
        .filter(|(stopped, _)| *stopped == step.key)
        .map(|(_, at)| at);
    if let (Some(at), Some((path, line))) = (at, step.from.as_ref()) {
        editor.debug.engine.record.line_time(
            at,
            step.key,
            path,
            *line,
            &step.function,
            step.depth,
            corrected,
            cpu,
        );
    }
}

fn at_line(frames: &[StackFrame], target: &RunTo) -> bool {
    frames.first().is_some_and(|frame| {
        frame.line.saturating_sub(1) == target.line
            && frame
                .source
                .as_ref()
                .and_then(|source| source.path.as_deref())
                == Some(target.path.as_path())
    })
}

/// Sends the next playback step once the stop is recorded and the speed's interval since the
/// last step is over.
fn schedule(editor: &mut Editor, recorded: Option<oneshot::Receiver<()>>) {
    let spectator = &editor.debug.spectator;
    let token = spectator.token;
    let deadline = match (spectator.mode, spectator.last_sent) {
        (Mode::Playing, Some(sent)) => sent + spectator.interval(),
        _ => Instant::now(),
    };
    tokio::spawn(async move {
        if let Some(recorded) = recorded {
            let _ = recorded.await;
        }
        tokio::time::sleep_until(deadline.into()).await;
        job::dispatch(move |editor, _| {
            let spectator = &editor.debug.spectator;
            if spectator.token == token && spectator.is_playing() && spectator.step.is_none() {
                send_step(editor, false);
            }
        })
        .await;
    });
}

/// The values fast forward compares, by their name in the panel: the watches (`watch: x + 1`)
/// and the marked variables (`a.b.c`), as shown now.
fn watched_values(editor: &Editor) -> HashMap<String, String> {
    let state = &editor.debug;
    let mut values = HashMap::new();
    for watch in &state.watches {
        let value = watch
            .node
            .as_ref()
            .filter(|node| !node.error)
            .map(|node| node.value.clone())
            .unwrap_or_default();
        values.insert(format!("watch: {}", watch.expression), value);
    }
    for names in &state.spectator.marked {
        let path = vortex_view::debug::VarPath {
            tree: Tree::Scopes,
            names: names.clone(),
        };
        let value = state
            .node(&path)
            .map(|node| node.value.clone())
            .unwrap_or_default();
        values.insert(names[1..].join("."), value);
    }
    values
}

/// A fast forward step stopped: loads the watched values (after the recording, which needs the
/// stop's references first), compares them with the baseline, and goes on or pauses on the
/// first change.
fn fast_forward_check(
    editor: &mut Editor,
    key: ThreadKey,
    frame_id: Option<usize>,
    recorded: Option<oneshot::Receiver<()>>,
) {
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        return;
    };
    let requester = client.requester();
    let token = editor.debug.spectator.token;
    let watches: Vec<String> = editor
        .debug
        .watches
        .iter()
        .map(|watch| watch.expression.clone())
        .collect();
    let marked: Vec<Vec<String>> = editor.debug.spectator.marked.iter().cloned().collect();
    tokio::spawn(async move {
        if let Some(recorded) = recorded {
            let _ = recorded.await;
        }
        let mut values = HashMap::new();
        if let Some(frame_id) = frame_id {
            for expression in watches {
                let value = match requester
                    .evaluate(expression.clone(), Some(frame_id), Some("watch"))
                    .await
                {
                    Ok(response) => response.result,
                    Err(_) => String::new(),
                };
                values.insert(format!("watch: {expression}"), value);
            }
            for names in marked {
                let value = load::fetch_path(&requester, frame_id, &names)
                    .await
                    .unwrap_or_default();
                values.insert(names[1..].join("."), value);
            }
        }
        job::dispatch(move |editor, _| {
            if editor.debug.spectator.token != token
                || editor.debug.spectator.mode != Mode::FastForward
            {
                return;
            }
            let mut changes = Vec::new();
            for (name, new) in values {
                match editor.debug.spectator.baseline.get(&name) {
                    Some(old) if *old != new => changes.push(Change {
                        name,
                        old: old.clone(),
                        new,
                    }),
                    Some(_) => {}
                    None => {
                        editor.debug.spectator.baseline.insert(name, new);
                    }
                }
            }
            if changes.is_empty() {
                send_step(editor, false);
                return;
            }
            changes.sort_by(|a, b| a.name.cmp(&b.name));
            let steps = editor.debug.spectator.steps;
            let text = changes
                .iter()
                .map(|change| {
                    let name = change.name.trim_start_matches("watch: ");
                    format!("{name}: {} -> {}", change.old, change.new)
                })
                .collect::<Vec<_>>()
                .join(", ");
            editor.debug.console.line(
                LineKind::Info,
                format!("Fast forward stopped after {steps} steps: {text}"),
            );
            crate::debug::mode::say(editor, format!("Changed after {steps} steps: {text}"));
            editor.debug.spectator.changes = changes;
            editor.debug.spectator.pause();
            show_current(editor, false);
        })
        .await;
    });
}

/// Shows the current stop: jumps to its frame and loads its values.
pub fn show_current(editor: &mut Editor, follow: bool) {
    if let Some(frame) = editor.debug_adapters.current_stack_frame().cloned() {
        show_frame(editor, &frame, follow);
    }
    load::reload_frame(editor);
}

/// Jumps to `frame` (`playing`: a playback step). In debug mode the camera follows it, keeping
/// its line in the middle third of the view, and doesn't move in view mode. Outside debug mode a
/// pause jumps there, centered, and playback doesn't move the view: you are editing.
pub fn show_frame(editor: &mut Editor, frame: &StackFrame, playing: bool) {
    if editor.debug.viewing() || (!editor.debug.mode_on() && playing) {
        return;
    }
    // a memory view keeps its place: the source moves in another view
    vortex_view::handlers::dap::in_source_view(editor, |editor| {
        if editor.debug.following() {
            super::mode::camera(editor, frame);
        } else {
            jump_to_stack_frame(editor, frame);
        }
    })
}

/// Run to cursor: while playing, plays until the cursor's line; else continues to it through
/// a temporary breakpoint.
pub fn run_to_cursor(editor: &mut Editor) {
    rewind::to_live(editor);
    let Some((path, line)) = panel::cursor_line(editor) else {
        editor.set_error("Run to cursor needs a file");
        return;
    };
    if editor.debug.spectator.is_playing() {
        editor.debug.spectator.run_to = Some(RunTo {
            path,
            line,
            breakpoint: false,
        });
        log::info!("playing to line {}", line + 1);
        return;
    }
    let Some(key) = spectated(editor) else {
        editor.set_error("No thread to run");
        return;
    };
    if editor.debug.spectator.is_frozen(key) {
        editor.set_error("The spectated thread is frozen: thaw it first");
        return;
    }
    if !is_stopped(editor, key) {
        editor.set_error("The thread runs already");
        return;
    }
    // where the engine has a hidden breakpoint on the line, its hits are checked against it;
    // else a temporary breakpoint
    let covered = editor.debug.engine.is_ready(key.session)
        && editor.debug.engine.sessions[&key.session]
            .files
            .get(&path)
            .is_some_and(|file| file.lines.contains(&line) && !file.strays.contains(&line));
    editor.debug.spectator.run_to = Some(RunTo {
        path: path.clone(),
        line,
        breakpoint: !covered,
    });
    log::info!("running to line {}", line + 1);
    if covered {
        threads::continue_thread(editor, key);
        return;
    }
    breakpoints::sync_then(editor, &path, key.session, move |editor| {
        threads::continue_thread(editor, key);
    });
}

/// Any stop ends a run to cursor: its temporary breakpoint goes.
fn finish_run_to(editor: &mut Editor, frames: &[StackFrame]) {
    if editor.debug.spectator.is_playing() {
        // playing to a line: checked at each step
        return;
    }
    let Some(target) = editor.debug.spectator.run_to.take() else {
        return;
    };
    if target.breakpoint {
        breakpoints::sync(editor, &target.path);
    }
    if at_line(frames, &target) {
        crate::debug::mode::say(
            editor,
            format!("Reached the cursor (line {})", target.line + 1),
        );
    }
}

/// The run to cursor target, when it is a temporary breakpoint in `path`.
pub fn run_to_breakpoint(editor: &Editor, path: &std::path::Path) -> Option<usize> {
    let spectator = &editor.debug.spectator;
    if spectator.is_playing() {
        return None;
    }
    spectator
        .run_to
        .as_ref()
        .filter(|target| target.breakpoint && target.path == path)
        .map(|target| target.line)
}

/// The session ended or the program went away: playback stops.
pub fn session_ended(editor: &mut Editor) {
    if editor.debug.spectator.is_playing() {
        editor.debug.spectator.pause();
    }
    editor.debug.spectator.finish_step();
}
