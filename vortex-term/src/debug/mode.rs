//! Debug mode (this fork): the debugger as a video player over the lineline.
//!
//! - A mode that overrides every key, like watch mode: its keys (`[keys.debug]`, the layer
//!   [`Layer::Debug`](crate::keymap::Layer)) come first while it is on, and its three rows are on
//!   the [mode bar](super::bar). `space G`, `:debug-mode` and `vx --debug` enter it; `Esc` leaves
//!   it, and the session goes on as it was (paused or playing), the panel as it is.
//! - One mode at a time: entering it ends watch mode, starting watch mode leaves it.
//! - **Following** (the default): the view follows the spectated thread, its line kept in the
//!   middle third of the view (centered when it leaves it, not at every line). Keys that move or
//!   search wait (the bar says that `v` looks around); keys that edit wait for `Esc`.
//! - **View mode** (`v`): your own cursor, drawn as in select mode, moves and searches freely;
//!   playback goes on, but the stops don't move the view. Breakpoints (`b`, `B`, `C`, `M`) and run
//!   to cursor (`r`) act on the cursor's line. Without a session there is nothing to follow, so
//!   debug mode starts in view mode, to set breakpoints before launching.
//! - Breakpoints are set only in debug mode, and the gutter's column is theirs while it is on
//!   (the bookmarks' otherwise).
//! - Nothing goes to the status line but errors: the notes of the debugger's commands go to the
//!   bar while debug mode is on ([`say`]).

use vortex_dap::StackFrame;
use vortex_view::{debug::DebugMode, Editor};

use super::{bar, playback, rewind};

/// Says what a debugger command did: on debug mode's bar while it is on, else on the status line
/// (the short result of a command run from the palette).
pub fn say(editor: &mut Editor, text: impl Into<String>) {
    let text = text.into();
    if editor.debug.mode_on() {
        editor.debug.note(text);
    } else {
        editor.set_status(text);
    }
}

/// Whether debug mode is on.
pub fn is_on(editor: &Editor) -> bool {
    editor.debug.mode_on()
}

/// Enters debug mode (nothing when it is on): ends watch mode, and follows the spectated thread,
/// or starts in view mode when there is no session to follow.
pub fn enter(editor: &mut Editor) {
    if editor.debug.mode_on() {
        return;
    }
    // one mode at a time: watch mode's changes stay, as when it ends
    crate::watch::end(editor);
    let session = bar::has_session(editor);
    editor.debug.mode = Some(DebugMode {
        viewing: !session,
        note: None,
    });
    if session {
        follow_now(editor);
    }
}

/// Leaves debug mode: the session goes on as it was. Nothing is printed: the bar going away says
/// it.
pub fn leave(editor: &mut Editor) {
    editor.debug.mode = None;
}

/// `:debug-mode`: enters debug mode, or leaves it when it is on.
pub fn toggle(editor: &mut Editor) {
    if editor.debug.mode_on() {
        leave(editor);
    } else {
        enter(editor);
    }
}

/// `v`: view mode (your own cursor), or following again.
pub fn toggle_view(editor: &mut Editor) {
    let Some(mode) = editor.debug.mode.as_mut() else {
        return;
    };
    mode.note = None;
    if mode.viewing {
        if !bar::has_session(editor) {
            editor.debug.note("no session to follow: start one first");
            return;
        }
        editor.debug.mode.as_mut().unwrap().viewing = false;
        follow_now(editor);
    } else {
        mode.viewing = true;
    }
}

/// A session started: debug mode follows it.
pub fn session_started(editor: &mut Editor) {
    if let Some(mode) = editor.debug.mode.as_mut() {
        mode.viewing = false;
        mode.note = None;
    }
}

/// Shows where the camera is now: the stop shown in rewind, or the spectated thread's frame.
fn follow_now(editor: &mut Editor) {
    let history = &editor.debug.spectator.history;
    let frame = match history.current() {
        Some(snapshot) => snapshot.frames.get(history.frame).cloned(),
        None => editor
            .debug_adapters
            .current_stack_frame()
            .cloned()
            .or_else(|| editor.debug.spectator.shown_frame.clone()),
    };
    if let Some(frame) = frame {
        camera(editor, &frame);
    }
}

/// The camera of debug mode while following: goes to `frame`'s line, keeping it in the middle
/// third of the view (another file opens centered, as any jump; within the file the view
/// scrolls only when the line leaves the middle third, and then centers it).
pub(crate) fn camera(editor: &mut Editor, frame: &StackFrame) {
    let Some(path) = frame.source.as_ref().and_then(|source| source.path.clone()) else {
        return;
    };
    let same = vortex_view::doc!(editor).path() == Some(&path);
    if !same {
        vortex_view::handlers::dap::jump_to_stack_frame(editor, frame);
        return;
    }
    let (view, doc) = vortex_view::current!(editor);
    let text = doc.text().clone();
    let line = frame
        .line
        .saturating_sub(1)
        .min(text.len_lines().saturating_sub(1));
    let pos = text.line_to_char(line) + frame.column.saturating_sub(1);
    let pos = pos.min(text.len_chars().saturating_sub(1));
    doc.set_selection(view.id, vortex_core::Selection::point(pos));
    let height = view.inner_height();
    let top = text.char_to_line(doc.view_offset(view.id).anchor.min(text.len_chars()));
    let third = height / 3;
    let middle = (top + third)..(top + height.saturating_sub(third)).max(top + third + 1);
    if !middle.contains(&line) {
        vortex_view::align_view(doc, view, vortex_view::Align::Center);
    }
}

/// The present: back to live from rewind.
pub fn present(editor: &mut Editor) {
    if editor.debug.is_rewinding() {
        rewind::to_live(editor);
    } else {
        say(editor, "live already");
    }
}

/// Seeks to the recorded stop of the spectated thread nearest the lineline's `counter`.
pub fn seek(editor: &mut Editor, counter: u64) {
    let spectator = &editor.debug.spectator;
    let key = spectator
        .history
        .current()
        .map(|snapshot| snapshot.key)
        .or(spectator.spectated);
    let Some(index) = spectator.history.nearest(counter, key) else {
        say(editor, "nothing recorded there");
        return;
    };
    rewind::seek(editor, index);
}

/// The lineline's position now: the stop shown in rewind, else the live stop's hit (else the
/// live counter, past every hit).
fn position(editor: &Editor) -> u64 {
    let debug = &editor.debug;
    debug
        .spectator
        .history
        .current()
        .and_then(|snapshot| snapshot.counter)
        .or_else(|| debug.engine.last_stop.map(|(_, at)| at))
        .unwrap_or_else(|| debug.engine.record.counter())
}

/// The counters of the breakpoint marks on the lineline.
fn breakpoint_marks(editor: &Editor) -> Vec<u64> {
    use vortex_view::debug::lineline::MarkKind;
    editor
        .debug
        .engine
        .record
        .marks()
        .filter(|mark| matches!(mark.kind, MarkKind::Breakpoint { .. }))
        .map(|mark| mark.at)
        .collect()
}

/// `l`: the next line. Live, one playback step; in rewind, one recorded stop forward.
pub fn line_forward(editor: &mut Editor) {
    if editor.debug.is_rewinding() {
        rewind::forward(editor, 1);
    } else if !no_session(editor) {
        playback::step_once(editor);
    }
}

/// `h`: the previous line, one recorded stop back (into rewind from live).
pub fn line_back(editor: &mut Editor) {
    if no_session(editor) && editor.debug.spectator.history.stops.is_empty() {
        return;
    }
    rewind::back(editor, 1);
}

/// `L`: the next breakpoint. Live, at full speed to your next breakpoint (the engine's
/// continue); in rewind, the next breakpoint mark recorded, else the present.
pub fn breakpoint_forward(editor: &mut Editor, continue_live: impl FnOnce(&mut Editor)) {
    if !editor.debug.is_rewinding() {
        if !no_session(editor) {
            continue_live(editor);
        }
        return;
    }
    let at = position(editor);
    match breakpoint_marks(editor).into_iter().find(|mark| *mark > at) {
        Some(mark) => seek_after(editor, mark, at),
        None => rewind::to_live(editor),
    }
}

/// `H`: the previous breakpoint mark recorded (into rewind from live).
pub fn breakpoint_back(editor: &mut Editor) {
    let at = position(editor);
    match breakpoint_marks(editor)
        .into_iter()
        .rev()
        .find(|mark| *mark < at)
    {
        Some(mark) => seek(editor, mark),
        None => say(editor, "no breakpoint was hit before this"),
    }
}

/// Seeks to the stop nearest `mark`, but never back to `at` or before it.
fn seek_after(editor: &mut Editor, mark: u64, at: u64) {
    let history = &editor.debug.spectator.history;
    let key = history.current().map(|snapshot| snapshot.key);
    match history.nearest(mark, key) {
        Some(index)
            if history.stops[index]
                .counter
                .is_some_and(|counter| counter > at) =>
        {
            rewind::seek(editor, index)
        }
        _ => {
            // the stop nearest that breakpoint is behind: the next stop then
            rewind::forward(editor, 1);
        }
    }
}

/// `Space`: plays or pauses; a program running at full speed is paused.
pub fn play_pause(editor: &mut Editor, pause_running: impl FnOnce(&mut Editor)) {
    if no_session(editor) {
        return;
    }
    let running = matches!(editor.debug.status, vortex_view::debug::Status::Running);
    if running && !editor.debug.spectator.is_playing() {
        pause_running(editor);
    } else {
        playback::toggle(editor);
    }
}

/// `B`: cycles the type of the breakpoint on the cursor's line: any thread, the main thread only,
/// sub-threads only.
pub fn cycle_breakpoint_kind(editor: &mut Editor) {
    let Some((path, line)) = super::panel::cursor_line(editor) else {
        return;
    };
    let mut kind = None;
    let result = super::breakpoints::edit(editor, &path, line, |breakpoint| {
        breakpoint.kind = breakpoint.kind.next();
        kind = Some(breakpoint.kind);
    });
    match (result, kind) {
        (Ok(_), Some(kind)) => say(editor, format!("breakpoint: {}", kind.label())),
        (Err(err), _) => editor.set_error(err.to_string()),
        _ => {}
    }
}

/// Says on the bar that there is no session, and returns whether that is so.
fn no_session(editor: &mut Editor) -> bool {
    if bar::has_session(editor) {
        return false;
    }
    editor.debug.note("no session: start one first");
    true
}

/// What a command does in debug mode, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// Runs: the debugger's commands, the command line, the palette, the panels.
    Runs,
    /// Moves or searches: runs in view mode, waits while following.
    Moves,
    /// Edits or opens something else: waits for `Esc`.
    Waits,
}

/// The note on the bar for a key that waits for debug mode to end.
pub const LEAVE_TO_EDIT: &str = "Esc leaves debug mode, then edit";
/// The note on the bar for a movement while following.
pub const VIEW_TO_MOVE: &str = "following: v looks around to move";

fn effect(name: &str) -> Effect {
    if name.starts_with("dap_")
        || name.starts_with("debug_")
        || matches!(
            name,
            "no_op"
                | "normal_mode"
                | "command_mode"
                | "command_palette"
                | "select_register"
                | "toggle_terminal"
                | "toggle_bookmark"
        )
    {
        return Effect::Runs;
    }
    match crate::watch::effect_of(name) {
        crate::watch::Effect::None => Effect::Runs,
        crate::watch::Effect::Move | crate::watch::Effect::Search => Effect::Moves,
        crate::watch::Effect::End => Effect::Waits,
    }
}

/// Called before a static command runs: `false` when debug mode holds it (the bar says why).
pub(crate) fn before_command(editor: &mut Editor, name: &str) -> bool {
    let Some(mode) = editor.debug.mode.as_ref() else {
        return true;
    };
    let viewing = mode.viewing;
    match effect(name) {
        Effect::Runs => true,
        Effect::Moves if viewing => true,
        Effect::Moves => {
            editor.debug.note(VIEW_TO_MOVE);
            false
        }
        Effect::Waits => {
            editor.debug.note(LEAVE_TO_EDIT);
            false
        }
    }
}

/// The mouse moved the cursor in the text, or scrolled: view mode.
pub(crate) fn moved(editor: &mut Editor) {
    if let Some(mode) = editor.debug.mode.as_mut() {
        mode.viewing = true;
        mode.note = None;
    }
}

/// Whether the cursor is your own in debug mode (view mode): drawn as in select mode.
pub(crate) fn viewing(editor: &Editor) -> bool {
    editor.debug.viewing()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects() {
        assert_eq!(effect("dap_toggle_breakpoint"), Effect::Runs);
        assert_eq!(effect("debug_view"), Effect::Runs);
        assert_eq!(effect("command_palette"), Effect::Runs);
        assert_eq!(effect("move_char_left"), Effect::Moves);
        assert_eq!(effect("search"), Effect::Moves);
        assert_eq!(effect("goto_file_start"), Effect::Moves);
        assert_eq!(effect("delete_selection"), Effect::Waits);
        assert_eq!(effect("insert_mode"), Effect::Waits);
        assert_eq!(effect("file_picker"), Effect::Waits);
    }
}
