use std::path::{Path, PathBuf};
use std::rc::Rc;

use vortex_vcs::{CommitInfo, FileChange};
use vortex_view::{DocumentId, Editor};

use crate::compositor::Compositor;
use crate::live::Live;
use crate::ui::diff_view::{
    Actions, DiffView, FileNav, ListFiles, SelectFile, Side, Snapshot, Staging, Target,
};
use crate::ui::picker;

/// Whether the git configuration of the repository `path` is in can be trusted.
fn trust_full(editor: &Editor, path: &Path) -> bool {
    let workspace = vortex_loader::find_workspace_in(path.parent().unwrap_or(path)).0;
    editor
        .workspace_trust
        .query(&workspace, vortex_loader::workspace_trust::TrustQuery::Git)
        .is_trusted()
}

fn relative(path: &Path) -> String {
    vortex_stdx::path::get_relative_path(path)
        .display()
        .to_string()
}

/// The diff between the HEAD version of a changed file (its old path, for a rename) and its
/// current contents, with the index version for staging (`None` outside of a git working
/// tree), and the tab width to show it with.
fn head_snapshot(editor: &Editor, change: &FileChange) -> anyhow::Result<(Snapshot, usize)> {
    let (base_path, path) = paths_of(change);
    let trust_full = trust_full(editor, &path);
    // once a rename is committed, the new path is the one in HEAD
    let base_path = if base_path != path
        && editor
            .diff_providers
            .get_diff_base(&path, trust_full)
            .is_some()
    {
        path.clone()
    } else {
        base_path
    };
    let base = editor.diff_providers.get_diff_base(&base_path, trust_full);
    let left_title = match &base {
        Some(_) => format!("HEAD: {}", relative(&base_path)),
        None => "HEAD: (new file)".to_string(),
    };
    let in_head = base.is_some();
    let base = String::from_utf8(base.unwrap_or_default())
        .map_err(|_| anyhow::anyhow!("{} is a binary file", relative(&base_path)))?;

    // an open buffer may have unsaved changes: show those
    let (current, right_title, tab_width, exists) = match editor.document_by_path(&path) {
        Some(doc) => {
            let modified = if doc.is_modified() { " [modified]" } else { "" };
            (
                doc.text().to_string(),
                format!("Buffer: {}{modified}", relative(&path)),
                doc.tab_width(),
                true,
            )
        }
        None => match std::fs::read(&path) {
            Ok(bytes) => (
                String::from_utf8(bytes)
                    .map_err(|_| anyhow::anyhow!("{} is a binary file", relative(&path)))?,
                format!("Working tree: {}", relative(&path)),
                4,
                true,
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
                String::new(),
                format!("Working tree: {} (deleted)", relative(&path)),
                4,
                false,
            ),
            Err(err) => anyhow::bail!("failed to read {}: {err}", relative(&path)),
        },
    };

    // no index: not a git working tree, nothing to stage
    let staging = match editor.diff_providers.index_file(&path, trust_full) {
        Ok(index) => Some(Staging {
            change: change.clone(),
            index: index.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
            in_head,
        }),
        Err(err) => {
            log::debug!("no index for {}: {err}", path.display());
            None
        }
    };

    let snapshot = Snapshot {
        left: Side::new(left_title, base, &base_path, editor),
        right: Side::new(right_title, current, &path, editor),
        path: exists.then(|| path.clone()),
        staging,
    };
    Ok((snapshot, tab_width))
}

/// The diff of a changed file of the working tree: HEAD against the working tree (or the buffer,
/// with its unsaved changes). Its hunks can be staged, unstaged and reverted in a git working
/// tree. It may have no changes.
fn head_view(editor: &Editor, change: &FileChange) -> anyhow::Result<DiffView> {
    let (snapshot, tab_width) = head_snapshot(editor, change)?;
    let path = change.path().to_path_buf();
    let staging = snapshot.staging.clone();
    let view = DiffView::new(snapshot.left, snapshot.right, snapshot.path, tab_width)
        .with_soft_wrap(soft_wrap(editor, &path));
    let Some(staging) = staging else {
        return Ok(view);
    };
    let change = change.clone();
    Ok(view
        .with_actions(Actions {
            target: Target::File(path),
            staging: Some(staging),
        })
        .with_rebuild(move |editor| Ok(head_snapshot(editor, &change)?.0)))
}

/// The diff of an uncommitted change, on top of the pickers of the commit log. With `live`, it
/// follows the working tree, and stays open when the file has no changes left.
fn uncommitted_view(
    editor: &Editor,
    change: &FileChange,
    live: Option<Live>,
) -> anyhow::Result<DiffView> {
    let view = head_view(editor, change)?.with_on_open(|compositor| {
        // the uncommitted files picker and the commit log below it
        compositor.remove(picker::ID);
        compositor.remove(picker::ID);
    });
    Ok(match live {
        Some(live) => view.with_live(live),
        None => view,
    })
}

/// Opens the diff of an uncommitted change on top of the pickers of the commit log, see
/// [`uncommitted_view`], at the hunk at `line` of its right side (from 0), or at its first hunk.
/// `n`/`N` go through the other files of `files`.
pub(crate) fn open_uncommitted(
    editor: &mut Editor,
    compositor: &mut Compositor,
    change: &FileChange,
    line: Option<u32>,
    live: Option<Live>,
    files: ListFiles,
    select: SelectFile,
) {
    let mut view = match uncommitted_view(editor, change, live.clone()) {
        Ok(view) if view.has_changes() => view,
        Ok(_) => return editor.set_status(format!("No changes in {}", relative(change.path()))),
        Err(err) => return editor.set_error(err.to_string()),
    };
    let nav = FileNav {
        files,
        current: change.clone(),
        open: Rc::new(move |editor, change| uncommitted_view(editor, change, live.clone())),
        select,
    };
    if let Some(line) = line {
        view.go_to_line(line);
    }
    compositor.push(Box::new(view.with_nav(nav)));
}

/// The HEAD path and the current path to diff for an entry of the changed files picker.
pub(crate) fn paths_of(change: &FileChange) -> (PathBuf, PathBuf) {
    match change {
        FileChange::Renamed { from_path, to_path } => (from_path.clone(), to_path.clone()),
        change => (change.path().to_path_buf(), change.path().to_path_buf()),
    }
}

/// Opens the diff for an entry of the changed files picker, on top of the picker. Opening the
/// file from the diff closes the picker too.
pub(crate) fn open_from_picker(cx: &mut crate::compositor::Context, change: &FileChange) {
    let change = change.clone();
    cx.jobs.callback(async move {
        let call = move |editor: &mut Editor, compositor: &mut Compositor| match head_view(
            editor, &change,
        ) {
            Ok(view) if view.has_changes() => {
                compositor.push(Box::new(view.with_on_open(|compositor| {
                    compositor.remove(picker::ID);
                })))
            }
            Ok(_) => editor.set_status(format!("No changes in {}", relative(change.path()))),
            Err(err) => editor.set_error(err.to_string()),
        };
        Ok(crate::job::Callback::EditorCompositor(Box::new(call)))
    });
}

/// Opens the diff for the file of the current buffer, or, for a buffer without a file, the diff
/// against its diff base (a `:practice` buffer).
pub(crate) fn open_current(editor: &mut Editor, compositor: &mut Compositor) {
    let doc = vortex_view::doc!(editor);
    let view = match doc.path() {
        Some(path) => head_view(
            editor,
            &FileChange::Modified {
                path: path.to_path_buf(),
            },
        ),
        None => match doc.diff_handle() {
            Some(_) => Ok(buffer_view(editor, doc.id())),
            None => return editor.set_error("The current buffer has no file"),
        },
    };
    match view {
        Ok(view) if view.has_changes() => compositor.push(Box::new(view)),
        Ok(_) => editor.set_status(format!("No changes in {}", doc_name(editor))),
        Err(err) => editor.set_error(err.to_string()),
    }
}

fn doc_name(editor: &Editor) -> String {
    let doc = vortex_view::doc!(editor);
    match doc.path() {
        Some(path) => relative(path),
        None => doc.display_name().into_owned(),
    }
}

/// The sides of the diff of a buffer without a file: its diff base against its text.
fn buffer_snapshot(editor: &Editor, doc: DocumentId) -> anyhow::Result<Snapshot> {
    let doc = editor
        .document(doc)
        .ok_or_else(|| anyhow::anyhow!("the buffer is closed"))?;
    let base = doc
        .diff_handle()
        .map(|diff| diff.load().diff_base().to_string())
        .unwrap_or_default();
    let name = doc.display_name();
    let path = Path::new(name.as_ref());
    Ok(Snapshot {
        left: Side::new("Diff base".into(), base, path, editor),
        right: Side::new(
            format!("Buffer: {name}"),
            doc.text().to_string(),
            path,
            editor,
        ),
        path: None,
        staging: None,
    })
}

/// The diff of a buffer without a file against its diff base: its hunks can be reverted.
fn buffer_view(editor: &Editor, doc: DocumentId) -> DiffView {
    let snapshot = buffer_snapshot(editor, doc).expect("the buffer is open");
    let tab_width = editor.document(doc).map_or(4, |doc| doc.tab_width());
    DiffView::new(snapshot.left, snapshot.right, None, tab_width)
        .with_actions(Actions {
            target: Target::Buffer(doc),
            staging: None,
        })
        .with_rebuild(move |editor| buffer_snapshot(editor, doc))
}

/// The diff of `change` in the commits from `oldest` to `newest` (the same commit for one): from
/// the first parent of `oldest` to `newest`. Opening the file from it closes the commit pickers
/// below it. It may have no changes.
fn commit_view(
    editor: &Editor,
    cwd: &Path,
    oldest: &CommitInfo,
    newest: &CommitInfo,
    change: &FileChange,
) -> anyhow::Result<DiffView> {
    let trust_full = editor
        .workspace_trust
        .query(
            &vortex_loader::find_workspace_in(cwd).0,
            vortex_loader::workspace_trust::TrustQuery::Git,
        )
        .is_trusted();
    let read = |id: &str, path: &Path| -> anyhow::Result<Option<String>> {
        editor
            .diff_providers
            .file_at_commit(cwd, trust_full, id, path)?
            .map(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|_| anyhow::anyhow!("{} is a binary file", relative(path)))
            })
            .transpose()
    };

    let (base_path, path) = paths_of(change);
    let (base_short, short) = (&oldest.short_id, &newest.short_id);
    let (base, left_title) = match read(&format!("{}^", oldest.id), &base_path)? {
        Some(text) => (text, format!("{base_short}^: {}", relative(&base_path))),
        None => (String::new(), format!("{base_short}^: (new file)")),
    };
    let (current, right_title) = match read(&newest.id, &path)? {
        Some(text) => (text, format!("{short}: {}", relative(&path))),
        None => (
            String::new(),
            format!("{short}: {} (deleted)", relative(&path)),
        ),
    };
    let tab_width = editor
        .document_by_path(&path)
        .map_or(4, |doc| doc.tab_width());

    let left = Side::new(left_title, base, &base_path, editor);
    let right = Side::new(right_title, current, &path, editor);
    Ok(DiffView::new(
        left,
        right,
        path.exists().then_some(path.clone()),
        tab_width,
    )
    .with_soft_wrap(soft_wrap(editor, &path))
    // the commit's files picker and the commit picker below it
    .with_on_open(|compositor| {
        compositor.remove(picker::ID);
        compositor.remove(picker::ID);
    }))
}

/// Opens the diff of `change` in the commits from `oldest` to `newest` on top of the commit
/// pickers, see [`commit_view`]. `n`/`N` go through the other files of `files`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_commit(
    editor: &mut Editor,
    compositor: &mut Compositor,
    cwd: &Path,
    oldest: &CommitInfo,
    newest: &CommitInfo,
    change: &FileChange,
    files: ListFiles,
    select: SelectFile,
) -> anyhow::Result<()> {
    let view = commit_view(editor, cwd, oldest, newest, change)?;
    if !view.has_changes() {
        editor.set_status(format!("No changes in {}", relative(change.path())));
        return Ok(());
    }
    let (cwd, oldest, newest) = (cwd.to_path_buf(), oldest.clone(), newest.clone());
    let nav = FileNav {
        files,
        current: change.clone(),
        open: Rc::new(move |editor, change| commit_view(editor, &cwd, &oldest, &newest, change)),
        select,
    };
    compositor.push(Box::new(view.with_nav(nav)));
    Ok(())
}

/// Whether lines of `path` wrap in the editor: the setting of its buffer when it is open, else
/// `soft-wrap.enable` of its language, else of the editor.
fn soft_wrap(editor: &Editor, path: &Path) -> bool {
    if let Some(doc) = editor.document_by_path(path) {
        // any width wide enough to wrap
        return doc.text_format(1000, None).soft_wrap;
    }
    let loader = editor.syn_loader.load();
    loader
        .language_for_filename(path)
        .and_then(|language| {
            loader
                .language(language)
                .config()
                .soft_wrap
                .as_ref()?
                .enable
        })
        .or(editor.config().soft_wrap.enable)
        .unwrap_or(false)
}
