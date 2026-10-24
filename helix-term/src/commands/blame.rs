//! Commands of inline git blame (fork feature).

use helix_view::{
    blame::BlameState,
    editor::{Config, ConfigEvent},
    DocumentId, Editor,
};

use super::Context;
use crate::handlers::blame as handler;

/// Show the blame of the line of the cursor in the statusline, using the configured format.
pub fn blame_line(cx: &mut Context) {
    let (view, doc) = current_ref!(cx.editor);
    let doc_id = doc.id();
    if doc.path().is_none() {
        cx.editor.set_error("Buffer has no file to blame");
        return;
    }
    let line = doc
        .selection(view.id)
        .primary()
        .cursor_line(doc.text().slice(..));
    if matches!(doc.blame.state(), BlameState::Unknown | BlameState::Pending) {
        // show it as soon as it is known
        let doc = doc_mut!(cx.editor, &doc_id);
        doc.blame.set_status_line(Some(line));
        handler::request_unconditionally(cx.editor, doc_id);
        cx.editor.set_status("Computing blame...");
    } else {
        show_in_statusline(cx.editor, doc_id, line);
    }
}

pub(crate) fn show_in_statusline(editor: &mut Editor, doc_id: DocumentId, line: usize) {
    match handler::describe(editor, doc_id, line) {
        Ok(text) => editor.set_status(text),
        Err(err) => editor.set_error(err),
    }
}

/// Flip `inline-blame.enable`, like `:toggle inline-blame.enable` does.
pub fn toggle_inline_blame(cx: &mut Context) {
    if let Err(err) = toggle(cx.editor) {
        cx.editor.set_error(err.to_string());
    }
}

pub(crate) fn toggle(editor: &mut Editor) -> anyhow::Result<()> {
    let mut config: Config = Config::clone(&editor.config());
    config.inline_blame.enable = !config.inline_blame.enable;
    let enabled = config.inline_blame.enable;
    editor
        .config_events
        .0
        .send(ConfigEvent::Update(Box::new(config)))?;
    editor.set_status(format!(
        "Inline blame {}",
        if enabled { "enabled" } else { "disabled" }
    ));
    Ok(())
}
