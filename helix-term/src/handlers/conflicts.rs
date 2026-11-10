//! Git merge conflicts as diagnostics (this fork, see FORK.md, "Conflict diagnostics").
//!
//! Which files are unmerged comes from the index of the repositories of the working directory
//! and of the open documents, read in the background:
//!
//! - when a document is opened, and after switching buffers;
//! - when the terminal or the editor (back from the terminal panel) regains focus, with the
//!   disk checks of [`super::disk`];
//! - when the watch of a live view ([`crate::live::RepoWatch`]) sees the repository change, so
//!   a merge, rebase or cherry-pick that starts or ends shows up while the commit log is open.
//!
//! The conflict blocks of an unmerged file are found in its text: in the document's on every
//! edit ([`helix_view::Document::update_conflict_diagnostics`]), so removing the markers clears
//! the diagnostic at once, and in the file on disk for the files that aren't open (`space D`,
//! the workspace count of the statusline).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use helix_event::register_hook;
use helix_loader::workspace_trust::TrustQuery;
use helix_vcs::conflict::{conflict_blocks, ConflictBlock};
use helix_vcs::DiffProviderRegistry;
use helix_view::events::{DocumentDidChange, DocumentDidOpen, DocumentFocusLost};
use helix_view::handlers::Handlers;
use helix_view::Editor;

use crate::job;

type Files = BTreeMap<PathBuf, Vec<ConflictBlock>>;

/// Where to look for repositories, each with the workspace-trust decision for it.
fn places(editor: &Editor) -> Vec<(PathBuf, bool)> {
    let mut dirs = vec![helix_stdx::env::current_working_dir()];
    for doc in editor.documents() {
        if let Some(dir) = doc.path().and_then(|path| path.parent()) {
            if !dirs.iter().any(|known| dir.starts_with(known)) {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    dirs.into_iter()
        .map(|dir| {
            let trust_full = editor
                .workspace_trust
                .query(&helix_loader::find_workspace_in(&dir).0, TrustQuery::Git)
                .is_trusted();
            (dir, trust_full)
        })
        .collect()
}

/// The unmerged files of the repositories of `places`, with their conflict blocks on disk.
fn read(providers: &DiffProviderRegistry, places: &[(PathBuf, bool)]) -> Files {
    let mut work_dirs: Vec<PathBuf> = Vec::new();
    let mut files = Files::new();
    for (dir, trust_full) in places {
        if work_dirs.iter().any(|work_dir| dir.starts_with(work_dir)) {
            continue;
        }
        let (work_dir, unmerged) = match providers.conflicted_files(dir, *trust_full) {
            Ok(found) => found,
            Err(err) => {
                log::debug!("no conflicts read for {}: {err}", dir.display());
                continue;
            }
        };
        // canonical, like the paths of the documents
        let work_dir = helix_stdx::path::canonicalize(work_dir);
        for file in unmerged {
            let file = match file.strip_prefix(&work_dir) {
                Ok(_) => file,
                Err(_) => helix_stdx::path::canonicalize(file),
            };
            let blocks = disk_blocks(&file);
            files.insert(file, blocks);
        }
        work_dirs.push(work_dir);
    }
    files
}

/// The conflict blocks of a file on disk: none for a file that is deleted (a modify/delete
/// conflict) or binary.
fn disk_blocks(file: &Path) -> Vec<ConflictBlock> {
    match std::fs::read(file) {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) => conflict_blocks(text.split_inclusive('\n')),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

/// Takes the result of refresh number `number`, unless a newer one was taken already, and marks
/// the open documents as unmerged or not.
fn apply(editor: &mut Editor, number: u64, files: Files) {
    if number <= editor.conflicts.applied {
        return;
    }
    editor.conflicts.applied = number;
    editor.conflicts.files = files;
    let conflicts = &editor.conflicts;
    for doc in editor.documents.values_mut() {
        let conflicted = doc
            .path()
            .is_some_and(|path| conflicts.files.contains_key(path));
        // an unmerged document's diagnostics are kept up to date by its edits already
        if conflicted != doc.is_conflicted() {
            doc.set_conflicted(conflicted);
        }
    }
    helix_event::request_redraw();
}

/// Reads which files are unmerged again, in the background.
pub fn refresh(editor: &mut Editor) {
    editor.conflicts.started += 1;
    let number = editor.conflicts.started;
    let places = places(editor);
    let providers = editor.diff_providers.clone();
    tokio::task::spawn_blocking(move || {
        let files = read(&providers, &places);
        job::dispatch_blocking(move |editor, _| apply(editor, number, files));
    });
}

/// Reads which files are unmerged again, now: for `space D`, which lists the conflicts of the
/// files that aren't open.
pub fn refresh_now(editor: &mut Editor) {
    editor.conflicts.started += 1;
    let number = editor.conflicts.started;
    let files = read(&editor.diff_providers, &places(editor));
    apply(editor, number, files);
}

pub(super) fn register_hooks(_handlers: &Handlers) {
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        let editor = &mut *event.editor;
        let conflicted = editor
            .document(event.doc)
            .and_then(|doc| doc.path())
            .is_some_and(|path| editor.conflicts.files.contains_key(path));
        if conflicted {
            if let Some(doc) = editor.document_mut(event.doc) {
                doc.set_conflicted(true);
            }
        }
        // the file may have become unmerged since the last look
        refresh(editor);
        Ok(())
    });
    register_hook!(move |event: &mut DocumentDidChange<'_>| {
        if event.doc.is_conflicted() {
            event.doc.update_conflict_diagnostics();
        }
        Ok(())
    });
    register_hook!(move |event: &mut DocumentFocusLost<'_>| {
        refresh(event.editor);
        Ok(())
    });
}
