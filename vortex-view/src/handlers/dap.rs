//! Helpers for the debugger: the debug sessions themselves are handled in `vortex-term`'s
//! `debug` module (fork rework).

use crate::editor::Action;
use crate::{align_view, Align, Editor};
use vortex_core::Selection;

#[macro_export]
macro_rules! debugger {
    ($editor:expr) => {{
        let Some(debugger) = $editor.debug_adapters.get_active_client_mut() else {
            return;
        };
        debugger
    }};
}

// general utils:
pub fn dap_pos_to_pos(doc: &vortex_core::Rope, line: usize, column: usize) -> Option<usize> {
    // 1-indexing to 0 indexing
    let line = doc.try_line_to_char(line.checked_sub(1)?).ok()?;
    let pos = line + column.saturating_sub(1);
    // TODO: this is probably utf-16 offsets
    Some(pos)
}

/// Opens the frame's source at its line (and range, when the adapter gives one).
/// Runs `f` in a view showing source code: the focused view, unless it shows a debugged
/// program's memory (this fork), which keeps its place and the focus while the source moves in
/// another view (a split of it when there is none).
pub fn in_source_view<R>(editor: &mut Editor, f: impl FnOnce(&mut Editor) -> R) -> R {
    let focused = editor.tree.focus;
    let shows_memory = |editor: &Editor, view: crate::ViewId| {
        let doc = editor.tree.get(view).doc;
        editor.documents[&doc].memory().is_some()
    };
    if !shows_memory(editor, focused) {
        return f(editor);
    }
    let other = editor
        .tree
        .views()
        .map(|(view, _)| view.id)
        .find(|&view| !shows_memory(editor, view));
    match other {
        Some(view) => editor.tree.focus = view,
        None => {
            editor.new_file(Action::VerticalSplit);
        }
    }
    let result = f(editor);
    if editor.tree.contains(focused) {
        editor.tree.focus = focused;
    }
    result
}

pub fn jump_to_stack_frame(editor: &mut Editor, frame: &vortex_dap::StackFrame) {
    in_source_view(editor, |editor| jump_in_view(editor, frame))
}

fn jump_in_view(editor: &mut Editor, frame: &vortex_dap::StackFrame) {
    let path = if let Some(vortex_dap::Source {
        path: Some(ref path),
        ..
    }) = frame.source
    {
        path.clone()
    } else {
        return;
    };

    // a frame in code whose source isn't on this machine (libc, the Rust sysroot without
    // rust-src): stay where we are rather than open an empty buffer
    if !path.exists() {
        return;
    }
    if let Err(e) = editor.open(&path, Action::Replace) {
        editor.set_error(format!("Unable to jump to stack frame: {}", e));
        return;
    }

    let (view, doc) = current!(editor);

    let text_end = doc.text().len_chars().saturating_sub(1);
    let start = dap_pos_to_pos(doc.text(), frame.line, frame.column).unwrap_or(0);
    let end = frame
        .end_line
        .and_then(|end_line| dap_pos_to_pos(doc.text(), end_line, frame.end_column.unwrap_or(0)))
        .unwrap_or(start);

    let selection = Selection::single(start.min(text_end), end.min(text_end));
    doc.set_selection(view.id, selection);
    align_view(doc, view, Align::Center);
}
