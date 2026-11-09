//! Stage, unstage and revert the hunks of a diff of the working tree (fork feature), or whole
//! files. Called by the diff view (`ui/diff_view.rs`), which shows the result.
//!
//! - Staging and unstaging write the index with the git CLI (see `helix_vcs`'s
//!   `set_index_file`), from the texts the diff shows: they change no text, so they are not
//!   part of undo; unstaging is the inverse of staging.
//! - Reverting is an edit of the file's buffer (opened if needed), one transaction, then a write:
//!   `u` in the buffer undoes it. It goes back to the index version, so what is staged is never
//!   lost; in a buffer without a file (`:practice`), to the diff base.
//! - An untracked file has nothing to go back to: reverting it deletes it, after a confirmation.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, ensure, Result};
use helix_core::Rope;
use helix_vcs::staging::{self, LineHunk};
use helix_vcs::FileChange;
use helix_view::editor::Action;
use helix_view::{DocumentId, Editor};

use crate::ui::diff_view::{Actions, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Stage,
    Unstage,
    Revert,
}

/// What an action applies to.
pub(crate) enum Scope {
    /// A hunk of the diff from HEAD (the left side) to the working tree (the right side).
    Hunk(LineHunk),
    File,
}

pub(crate) enum Outcome {
    /// Done, with a message for the statusline.
    Done(String),
    /// Reverting this untracked file deletes it: ask first, then call [`delete_untracked`].
    ConfirmDelete(PathBuf),
}

/// Runs `op` on `scope`. `head` and `work` are the texts the diff shows (left and right side).
pub(crate) fn run(
    editor: &mut Editor,
    actions: &Actions,
    head: &str,
    work: &str,
    op: Op,
    scope: Scope,
) -> Result<Outcome> {
    let path = match &actions.target {
        Target::File(path) => path.clone(),
        Target::Buffer(doc) => {
            ensure!(
                op == Op::Revert,
                "only the files of a git working tree can be staged"
            );
            return revert_buffer(editor, *doc, head, work, &scope).map(Outcome::Done);
        }
    };
    let Some(staging) = &actions.staging else {
        bail!("only the files of a git working tree can be staged");
    };
    let trust_full = trust_full(editor, &path);
    let what = |scope: &Scope| match scope {
        Scope::Hunk(_) => "the hunk".to_string(),
        Scope::File => relative(&path),
    };

    // the diff must show what is there now, or the hunk is not the one on screen: it is built
    // again after any action, so the next key works on what is there
    let index = editor.diff_providers.index_file(&path, trust_full)?;
    let index = index
        .map(|bytes| String::from_utf8(bytes).map_err(|_| anyhow!("the index has a binary file")))
        .transpose()?;
    if op != Op::Unstage {
        crate::handlers::disk::check_paths(editor, std::slice::from_ref(&path));
        if let Some(doc) = editor.document_by_path(&path) {
            ensure!(
                !doc.is_modified(),
                "{} has unsaved changes: write it first",
                relative(&path)
            );
        }
        ensure!(
            current_text(editor, &path)? == work,
            "{} changed: try again",
            relative(&path)
        );
    }
    ensure!(index == staging.index, "the index changed: try again");
    let index_text = index.as_deref().unwrap_or("");

    match (op, scope) {
        (Op::Stage, Scope::Hunk(hunk)) => {
            let staged = staging::stage_hunk(index_text, work, &hunk);
            if index.is_some() && staged == index_text {
                return Ok(Outcome::Done("The hunk is already staged".into()));
            }
            editor
                .diff_providers
                .set_index_file(&path, Some(staged.as_bytes()), trust_full)?;
            Ok(Outcome::Done("Staged the hunk".into()))
        }
        (Op::Stage, scope) => {
            for path in paths(&staging.change) {
                let contents = match std::fs::read(&path) {
                    Ok(contents) => Some(contents),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                    Err(err) => bail!("failed to read {}: {err}", relative(&path)),
                };
                editor
                    .diff_providers
                    .set_index_file(&path, contents.as_deref(), trust_full)?;
            }
            Ok(Outcome::Done(format!("Staged {}", what(&scope))))
        }
        (Op::Unstage, Scope::Hunk(hunk)) => {
            if index.is_none() {
                return Ok(Outcome::Done("Nothing is staged".into()));
            }
            let unstaged = staging::unstage_hunk(head, index_text, &hunk);
            if unstaged == index_text {
                return Ok(Outcome::Done("Nothing of the hunk is staged".into()));
            }
            // a new file with nothing staged left is untracked again
            let contents = (staging.in_head || !unstaged.is_empty()).then_some(unstaged);
            editor.diff_providers.set_index_file(
                &path,
                contents.as_deref().map(str::as_bytes),
                trust_full,
            )?;
            Ok(Outcome::Done("Unstaged the hunk".into()))
        }
        (Op::Unstage, scope) => {
            for path in paths(&staging.change) {
                editor.diff_providers.reset_index_file(&path, trust_full)?;
            }
            Ok(Outcome::Done(format!("Unstaged {}", what(&scope))))
        }
        (Op::Revert, scope) => {
            let Some(index) = &index else {
                // untracked: git has no version of it to go back to
                return Ok(Outcome::ConfirmDelete(path));
            };
            let reverted = match &scope {
                Scope::Hunk(hunk) => staging::revert_hunk(index, work, hunk),
                Scope::File => index.clone(),
            };
            if reverted == work {
                return Ok(Outcome::Done(match scope {
                    Scope::Hunk(_) => "The hunk is staged: u unstages it, then r reverts it".into(),
                    Scope::File => format!(
                        "{} is staged: U unstages it, then R reverts it",
                        relative(&path)
                    ),
                }));
            }
            let doc = match editor.document_by_path(&path) {
                Some(doc) => doc.id(),
                None => editor.open(&path, Action::Load)?,
            };
            edit(editor, doc, &reverted);
            editor.save::<PathBuf>(doc, None, false)?;
            Ok(Outcome::Done(format!(
                "Reverted {}: u in its buffer brings it back",
                what(&scope)
            )))
        }
    }
}

/// Deletes the untracked file at `path`, closing its buffer.
pub(crate) fn delete_untracked(editor: &mut Editor, path: &Path) -> Result<String> {
    if let Some(doc) = editor.document_by_path(path) {
        ensure!(
            !doc.is_modified(),
            "{} has unsaved changes: write it first",
            relative(path)
        );
        let doc = doc.id();
        editor
            .close_document(doc, true)
            .map_err(|_| anyhow!("failed to close the buffer of {}", relative(path)))?;
    }
    std::fs::remove_file(path)
        .map_err(|err| anyhow!("failed to delete {}: {err}", relative(path)))?;
    Ok(format!("Deleted {}", relative(path)))
}

/// Reverts a buffer without a file to the left side of its diff (its diff base).
fn revert_buffer(
    editor: &mut Editor,
    doc: DocumentId,
    base: &str,
    work: &str,
    scope: &Scope,
) -> Result<String> {
    let text = editor
        .document(doc)
        .ok_or_else(|| anyhow!("the buffer is closed"))?
        .text()
        .to_string();
    ensure!(text == work, "the buffer changed: try again");
    let (reverted, what) = match scope {
        Scope::Hunk(hunk) => (staging::revert_hunk(base, work, hunk), "the hunk"),
        Scope::File => (base.to_string(), "the buffer"),
    };
    edit(editor, doc, &reverted);
    Ok(format!("Reverted {what}: u brings it back"))
}

/// Changes the text of `doc` to `text`, as one undoable step.
fn edit(editor: &mut Editor, doc: DocumentId, text: &str) {
    let view = editor.get_synced_view_id(doc);
    let document = editor
        .documents
        .get_mut(&doc)
        .expect("the document is open");
    let transaction = helix_core::diff::compare_ropes(document.text(), &Rope::from(text));
    let view = editor.tree.get_mut(view);
    document.apply(&transaction, view.id);
    document.append_changes_to_history(view);
}

/// The text of the file at `path`: its buffer when it is open, else what is on disk (nothing
/// for a deleted file).
fn current_text(editor: &Editor, path: &Path) -> Result<String> {
    if let Some(doc) = editor.document_by_path(path) {
        return Ok(doc.text().to_string());
    }
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| anyhow!("{} is binary", relative(path))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(anyhow!("failed to read {}: {err}", relative(path))),
    }
}

/// The paths a whole-file action changes: both for a rename.
fn paths(change: &FileChange) -> Vec<PathBuf> {
    match change {
        FileChange::Renamed { from_path, to_path } => vec![from_path.clone(), to_path.clone()],
        change => vec![change.path().to_path_buf()],
    }
}

fn trust_full(editor: &Editor, path: &Path) -> bool {
    let workspace = helix_loader::find_workspace_in(path.parent().unwrap_or(path)).0;
    editor
        .workspace_trust
        .query(&workspace, helix_loader::workspace_trust::TrustQuery::Git)
        .is_trusted()
}

fn relative(path: &Path) -> String {
    helix_stdx::path::get_relative_path(path)
        .display()
        .to_string()
}
