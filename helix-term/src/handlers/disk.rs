//! Stale-buffer protection (fork feature): documents whose file changed on disk.
//!
//! There is no background watcher. The disk is looked at when it matters: after switching to a
//! buffer, when the terminal or the editor (coming back from the terminal panel) regains focus,
//! and right before writing. A document without unsaved changes is reloaded silently, as an
//! undoable step. One with unsaved changes is left alone and the user is warned once per
//! distinct disk content; writing it is refused (see `Document::save_impl`) until `:reload` or
//! `:write!`. A file that was deleted is ignored.

use std::path::PathBuf;

use helix_event::register_hook;
use helix_view::{
    document::DiskChange, events::DocumentFocusLost, handlers::Handlers, DocumentId, Editor,
};

use crate::job;

/// Applies the rule above to these documents.
pub fn check_docs(editor: &mut Editor, docs: impl IntoIterator<Item = DocumentId>) {
    for doc_id in docs {
        check_doc(editor, doc_id);
    }
}

/// The documents whose path is one of `paths` (absolute; compare canonicalized).
pub fn check_paths(editor: &mut Editor, paths: &[PathBuf]) {
    let paths: Vec<_> = paths.iter().map(helix_stdx::path::canonicalize).collect();
    let docs: Vec<_> = editor
        .documents()
        .filter(|doc| {
            doc.path()
                .is_some_and(|path| paths.iter().any(|p| p == path))
        })
        .map(|doc| doc.id())
        .collect();
    check_docs(editor, docs);
}

/// Checks all open documents.
pub fn check_all(editor: &mut Editor) {
    let docs: Vec<_> = editor.documents().map(|doc| doc.id()).collect();
    check_docs(editor, docs);
}

fn check_doc(editor: &mut Editor, doc_id: DocumentId) {
    let Some(doc) = editor.document_mut(doc_id) else {
        return;
    };
    if doc.disk_change() != DiskChange::Changed {
        return;
    }
    if doc.is_modified() {
        if doc.take_disk_change_warning() {
            let message = format!(
                "{} changed on disk: :reload to load it, or :write! to overwrite it",
                doc.display_name()
            );
            editor.set_warning(message);
        }
        return;
    }
    reload(editor, doc_id);
}

/// Reloads a document that has no unsaved changes, through a view that shows it.
fn reload(editor: &mut Editor, doc_id: DocumentId) {
    let scrolloff = editor.config().scrolloff;
    let view_id = editor.get_synced_view_id(doc_id);
    let doc = editor.documents.get_mut(&doc_id).unwrap();
    let trust_full = editor
        .workspace_trust
        .query(
            doc.workspace_root(),
            helix_loader::workspace_trust::TrustQuery::Git,
        )
        .is_trusted();
    let view = editor.tree.get_mut(view_id);
    if let Err(err) = doc.reload(view, &editor.diff_providers, trust_full) {
        editor.set_error(err.to_string());
        return;
    }
    let view_ids: Vec<_> = doc.selections().keys().copied().collect();
    for id in view_ids {
        if editor.tree.contains(id) {
            let view = editor.tree.get_mut(id);
            if view.doc == doc_id {
                view.sync_changes(doc);
                if id == view_id {
                    view.ensure_cursor_in_view(doc, scrolloff);
                }
            }
        }
    }
    if let Some(path) = doc.path().map(ToOwned::to_owned) {
        editor
            .language_servers
            .file_event_handler
            .file_changed(path);
    }
    super::blame::request(editor, doc_id);
}

pub(super) fn register_hooks(_handlers: &Handlers) {
    register_hook!(move |_event: &mut DocumentFocusLost<'_>| {
        // The caller may still change the new document's selection, so look later.
        job::dispatch_blocking(|editor, _| {
            let doc_id = view!(editor).doc;
            check_docs(editor, [doc_id]);
        });
        Ok(())
    });
}
