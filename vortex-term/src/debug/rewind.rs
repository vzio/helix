//! Scrubbing through the recorded stops (`[` / `]`): the editor and the panel show a stop as it
//! was, read-only; nothing executes. Play, a step, or `L` comes back to the live position.

use vortex_view::{handlers::dap::jump_to_stack_frame, Editor};

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
        editor.set_status("Live: nothing after this stop yet");
        return;
    }
    if editor.debug.spectator.history.forward(by) {
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
        jump_to_stack_frame(editor, &frame);
    }
    editor.set_status("Live");
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
    jump_to_stack_frame(editor, &frame);
}
