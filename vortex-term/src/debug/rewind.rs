//! Scrubbing through the recorded stops (debug mode's `h` / `l`, `H` / `L`, a click on the
//! lineline): the editor and the panel show a stop as it was, read-only; nothing executes. Play,
//! a step, or `p` comes back to the live position.

use vortex_view::Editor;

/// The error for what can't be done in rewind.
pub const READ_ONLY: &str = "REWIND: nothing is executing, values can't change";

/// Goes back `by` stops.
pub fn back(editor: &mut Editor, by: usize) {
    let history = &mut editor.debug.spectator.history;
    if history.stops.len() < 2 && !history.is_rewinding() {
        editor.set_error(if editor.debug.spectator.caps.records() {
            "Nothing recorded before this stop yet"
        } else {
            "The history is off"
        });
        return;
    }
    if editor.debug.spectator.is_playing() {
        super::playback::pause(editor);
    }
    editor.debug.spectator.history.back(by);
    show(editor);
}

/// Goes forward `by` stops; past the last one, back to live.
pub fn forward(editor: &mut Editor, by: usize) {
    if !editor.debug.is_rewinding() {
        super::mode::say(editor, "Live: nothing after this stop yet");
        return;
    }
    if editor.debug.spectator.history.forward(by) {
        show(editor);
    } else {
        live_shown(editor);
    }
}

/// Shows recorded stop `index` (debug mode's seeks); the last stop is the live one.
pub fn seek(editor: &mut Editor, index: usize) {
    if editor.debug.spectator.is_playing() {
        super::playback::pause(editor);
    }
    if editor.debug.spectator.history.seek(index) {
        show(editor);
    } else {
        live_shown(editor);
    }
}

/// Selects frame `index` of the stop shown.
pub fn select_frame(editor: &mut Editor, index: usize) {
    let history = &mut editor.debug.spectator.history;
    let Some(snapshot) = history.current() else {
        return;
    };
    if index < snapshot.frames.len() {
        history.frame = index;
        show(editor);
    }
}

/// Back to the live position, if rewinding.
pub fn to_live(editor: &mut Editor) {
    if editor.debug.is_rewinding() {
        editor.debug.spectator.history.live();
        live_shown(editor);
    }
}

fn live_shown(editor: &mut Editor) {
    if let Some(frame) = editor.debug_adapters.current_stack_frame().cloned() {
        super::playback::show_frame(editor, &frame, false);
    }
    super::mode::say(editor, "Live");
}

/// Shows the frame of the stop in rewind.
fn show(editor: &mut Editor) {
    let history = &editor.debug.spectator.history;
    let Some(frame) = history
        .current()
        .and_then(|snapshot| snapshot.frames.get(history.frame))
        .cloned()
    else {
        return;
    };
    super::playback::show_frame(editor, &frame, false);
}
