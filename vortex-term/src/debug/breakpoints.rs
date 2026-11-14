//! Breakpoints and the debug sessions: every session gets every file's breakpoints (but the
//! disabled ones), and each session's answers and events update its own ids and verification.

use std::path::{Path, PathBuf};

use vortex_dap::{self as dap, registry::DebugAdapterId, SourceBreakpoint};
use vortex_view::{
    editor::{Breakpoint, SessionBreakpoint},
    Editor,
};

use crate::job;

/// Breakpoints are set in debug mode only: outside it, says so and returns `true`.
pub fn outside_debug_mode(editor: &mut Editor) -> bool {
    if editor.debug.mode_on() {
        return false;
    }
    editor.set_error("breakpoints are set in debug mode");
    true
}

/// A breakpoint as sent: the line it had in the editor (its identity until the answer comes)
/// and what the adapter got.
pub type Sent = (usize, SourceBreakpoint);

fn source_breakpoint(breakpoint: &Breakpoint) -> SourceBreakpoint {
    SourceBreakpoint {
        line: breakpoint.line + 1,
        column: breakpoint.column,
        condition: breakpoint.condition.clone(),
        hit_condition: breakpoint.hit_condition.clone(),
        log_message: breakpoint.log_message.clone(),
    }
}

fn file_to_send(breakpoints: &[Breakpoint]) -> Vec<Sent> {
    breakpoints
        .iter()
        .filter(|breakpoint| !breakpoint.disabled)
        .map(|breakpoint| (breakpoint.line, source_breakpoint(breakpoint)))
        .collect()
}

/// What `path` sends: its breakpoints but the disabled ones, and run to cursor's temporary one
/// when it goes to a line of this file without a breakpoint.
pub fn path_to_send(editor: &Editor, path: &Path) -> Vec<Sent> {
    let list = editor
        .breakpoints
        .get(path)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut sent = file_to_send(list);
    if let Some(line) = super::playback::run_to_breakpoint(editor, path) {
        if !list.iter().any(|b| b.line == line) {
            sent.push((
                line,
                SourceBreakpoint {
                    line: line + 1,
                    column: None,
                    condition: None,
                    hit_condition: None,
                    log_message: None,
                },
            ));
        }
    }
    sent
}

/// Every file's breakpoints, for a session's configuration.
pub fn to_send(editor: &Editor) -> Vec<(PathBuf, Vec<Sent>)> {
    editor
        .breakpoints
        .keys()
        .map(|path| (path.clone(), path_to_send(editor, path)))
        .collect()
}

/// Checks that the active session supports what the breakpoints of `path` use.
pub fn check_support(editor: &Editor, path: &Path) -> anyhow::Result<()> {
    let Some(caps) = editor
        .debug_adapters
        .get_active_client()
        .and_then(|client| client.caps.as_ref())
    else {
        return Ok(());
    };
    let breakpoints = editor
        .breakpoints
        .get(path)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let uses = |f: fn(&Breakpoint) -> bool| breakpoints.iter().any(|b| !b.disabled && f(b));
    if uses(|b| b.condition.is_some()) && !caps.supports_conditional_breakpoints.unwrap_or_default()
    {
        anyhow::bail!("The debug adapter does not support conditional breakpoints");
    }
    if uses(|b| b.hit_condition.is_some())
        && !caps
            .supports_hit_conditional_breakpoints
            .unwrap_or_default()
    {
        anyhow::bail!("The debug adapter does not support hit counts");
    }
    if uses(|b| b.log_message.is_some()) && !caps.supports_log_points.unwrap_or_default() {
        anyhow::bail!("The debug adapter does not support logpoints");
    }
    Ok(())
}

/// Sends the breakpoints of `path` to every configured session, with the session's hidden
/// breakpoints of that file (the engine's) after yours. The answers come back through the job
/// queue.
pub fn sync(editor: &mut Editor, path: &Path) {
    let sessions: Vec<_> = editor
        .debug_adapters
        .iter()
        .filter(|(_, client)| client.configured)
        .map(|(id, _)| id)
        .collect();
    for id in sessions {
        sync_then(editor, path, id, |_| {});
    }
}

/// Sends the breakpoints of `path` (yours, then the hidden ones) to `session` alone, then calls
/// `then` once it answered (run to cursor: its temporary breakpoint must be set before the
/// program continues).
pub fn sync_then(
    editor: &mut Editor,
    path: &Path,
    session: DebugAdapterId,
    then: impl FnOnce(&mut Editor) + Send + 'static,
) {
    let sent = path_to_send(editor, path);
    let hidden = super::engine::hidden_lines(editor, session, path, &sent);
    let Some(client) = editor.debug_adapters.get_client(session) else {
        return;
    };
    let requester = client.requester();
    let path = path.to_owned();
    tokio::spawn(async move {
        let breakpoints = sent
            .iter()
            .map(|(_, b)| b.clone())
            .chain(hidden.iter().map(|line| SourceBreakpoint {
                line: line + 1,
                column: None,
                condition: None,
                hit_condition: None,
                log_message: None,
            }))
            .collect();
        let result = requester.set_breakpoints(path.clone(), breakpoints).await;
        job::dispatch(move |editor, _| {
            let failed = result.is_err();
            apply_with_hidden(editor, session, &path, &sent, &hidden, result);
            if !failed {
                then(editor);
            }
        })
        .await;
    });
}

/// An answer to a send of yours and the hidden breakpoints: yours get the first answers, the
/// engine the rest.
fn apply_with_hidden(
    editor: &mut Editor,
    session: DebugAdapterId,
    path: &Path,
    sent: &[Sent],
    hidden: &[usize],
    result: dap::Result<Option<Vec<dap::Breakpoint>>>,
) {
    match result {
        Ok(answers) => {
            let answers = answers.unwrap_or_default();
            let (yours, theirs) = answers.split_at(sent.len().min(answers.len()));
            apply(editor, session, path, sent, Ok(Some(yours.to_vec())));
            super::engine::apply_hidden(editor, session, path, hidden, theirs);
        }
        Err(err) => apply(editor, session, path, sent, Err(err)),
    }
}

/// A session's answer to `setBreakpoints` for `path`: its ids and verification, matched to the
/// breakpoints by the lines they had when sent.
pub fn apply(
    editor: &mut Editor,
    session: DebugAdapterId,
    path: &Path,
    sent: &[Sent],
    result: dap::Result<Option<Vec<dap::Breakpoint>>>,
) {
    if editor.debug_adapters.get_client(session).is_none() {
        return;
    }
    let answers = match result {
        Ok(answers) => answers.unwrap_or_default(),
        Err(err) => {
            editor.set_error(format!("Failed to set breakpoints: {err}"));
            return;
        }
    };
    let Some(list) = editor.breakpoints.get_mut(path) else {
        return;
    };
    for ((line, _), answer) in sent.iter().zip(answers) {
        let Some(index) = list.iter().position(|b| b.line == *line) else {
            // removed since
            continue;
        };
        let moved = answer
            .line
            .map(|line| line.saturating_sub(1))
            .filter(|moved| moved != line && !list.iter().any(|b| b.line == *moved));
        let breakpoint = &mut list[index];
        if let Some(moved) = moved {
            breakpoint.line = moved;
        }
        if answer.message.is_some() {
            breakpoint.message = answer.message.clone();
        }
        record(breakpoint, session, answer.id, answer.verified);
    }
}

fn record(breakpoint: &mut Breakpoint, session: DebugAdapterId, id: Option<usize>, verified: bool) {
    match breakpoint
        .sessions
        .iter_mut()
        .find(|s| s.session == session)
    {
        Some(entry) => {
            entry.id = id;
            entry.verified = verified;
        }
        None => breakpoint.sessions.push(SessionBreakpoint {
            session,
            id,
            verified,
        }),
    }
    breakpoint.id = id.or(breakpoint.id);
    breakpoint.refresh_verified();
}

/// The adapter's `breakpoint` event: a breakpoint of session `session` was verified or moved
/// (`changed`), created by the adapter (`new`), or removed.
/// Returns whether one of yours changed (the engine's hidden breakpoints don't show: an adapter
/// that verifies them one by one sends thousands of these).
pub fn event(
    editor: &mut Editor,
    session: DebugAdapterId,
    reason: &str,
    answer: dap::Breakpoint,
) -> bool {
    if super::engine::breakpoint_event(editor, session, reason, &answer) {
        return false;
    }
    let find = |editor: &mut Editor, id: usize| -> Option<(PathBuf, usize)> {
        editor.breakpoints.iter().find_map(|(path, list)| {
            let index = list.iter().position(|b| {
                b.sessions
                    .iter()
                    .any(|s| s.session == session && s.id == Some(id))
            })?;
            Some((path.clone(), index))
        })
    };
    match reason {
        "changed" => {
            let Some(id) = answer.id else {
                return false;
            };
            let Some((path, index)) = find(editor, id) else {
                return false;
            };
            let list = editor.breakpoints.get_mut(&path).unwrap();
            let moved = answer
                .line
                .map(|line| line.saturating_sub(1))
                .filter(|moved| {
                    *moved != list[index].line && !list.iter().any(|b| b.line == *moved)
                });
            let breakpoint = &mut list[index];
            if let Some(moved) = moved {
                breakpoint.line = moved;
            }
            if answer.message.is_some() {
                breakpoint.message = answer.message.clone();
            }
            record(breakpoint, session, Some(id), answer.verified);
        }
        "new" => {
            let (Some(path), Some(line)) = (
                answer
                    .source
                    .as_ref()
                    .and_then(|source| source.path.clone()),
                answer.line,
            ) else {
                return false;
            };
            let list = editor.breakpoints.entry(path).or_default();
            let line = line.saturating_sub(1);
            let index = match list.iter().position(|b| b.line == line) {
                Some(index) => index,
                None => {
                    list.push(Breakpoint {
                        line,
                        column: answer.column,
                        message: answer.message.clone(),
                        ..Default::default()
                    });
                    list.len() - 1
                }
            };
            record(&mut list[index], session, answer.id, answer.verified);
        }
        "removed" => {
            let Some(id) = answer.id else {
                return false;
            };
            if let Some((path, index)) = find(editor, id) {
                let breakpoint = &mut editor.breakpoints.get_mut(&path).unwrap()[index];
                breakpoint
                    .sessions
                    .retain(|s| !(s.session == session && s.id == Some(id)));
                breakpoint.refresh_verified();
            }
        }
        reason => log::debug!("unknown breakpoint event reason '{reason}'"),
    }
    true
}

/// Sets or removes the breakpoint on `line` of `path`, and tells the sessions.
pub fn toggle(editor: &mut Editor, path: PathBuf, line: usize) {
    let list = editor.breakpoints.entry(path.clone()).or_default();
    match list.iter().position(|b| b.line == line) {
        Some(index) => {
            list.remove(index);
        }
        None => list.push(Breakpoint {
            line,
            ..Default::default()
        }),
    }
    if list.is_empty() {
        // sent once more as an empty list, so sessions drop it
        sync(editor, &path);
        editor.breakpoints.remove(&path);
    } else {
        sync(editor, &path);
    }
}

/// Changes the breakpoint on `line` of `path` with `edit`, checks the session supports it, and
/// tells the sessions. Returns false when there is no breakpoint there.
pub fn edit(
    editor: &mut Editor,
    path: &Path,
    line: usize,
    edit: impl FnOnce(&mut Breakpoint),
) -> anyhow::Result<bool> {
    let Some(breakpoint) = editor
        .breakpoints
        .get_mut(path)
        .and_then(|list| list.iter_mut().find(|b| b.line == line))
    else {
        return Ok(false);
    };
    let before = breakpoint.clone();
    edit(breakpoint);
    if let Err(err) = check_support(editor, path) {
        if let Some(breakpoint) = editor
            .breakpoints
            .get_mut(path)
            .and_then(|list| list.iter_mut().find(|b| b.line == line))
        {
            *breakpoint = before;
        }
        return Err(err);
    }
    sync(editor, path);
    Ok(true)
}

/// Removes the breakpoint on `line` of `path`.
pub fn remove(editor: &mut Editor, path: &Path, line: usize) {
    if let Some(list) = editor.breakpoints.get_mut(path) {
        list.retain(|b| b.line != line);
    }
    sync(editor, path);
    if editor.breakpoints.get(path).is_some_and(Vec::is_empty) {
        editor.breakpoints.remove(path);
    }
}
