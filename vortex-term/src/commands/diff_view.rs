use std::path::{Path, PathBuf};

use vortex_vcs::{CommitInfo, FileChange};
use vortex_view::Editor;

use crate::compositor::Compositor;
use crate::live::Live;
use crate::ui::diff_view::{DiffView, Side};
use crate::ui::picker;

/// The two sides of the diff between the HEAD version of `base_path` and the current contents
/// of `path` (they only differ for renamed files), the file to open from the diff, and the tab
/// width to show it with.
fn head_sides(
    editor: &Editor,
    base_path: &Path,
    path: &Path,
) -> anyhow::Result<(Side, Side, Option<PathBuf>, usize)> {
    let relative = |path: &Path| {
        vortex_stdx::path::get_relative_path(path)
            .display()
            .to_string()
    };

    let workspace = vortex_loader::find_workspace_in(path.parent().unwrap_or(path)).0;
    let trust_full = editor
        .workspace_trust
        .query(&workspace, vortex_loader::workspace_trust::TrustQuery::Git)
        .is_trusted();
    let base = editor.diff_providers.get_diff_base(base_path, trust_full);
    let left_title = match &base {
        Some(_) => format!("HEAD: {}", relative(base_path)),
        None => "HEAD: (new file)".to_string(),
    };
    let base = String::from_utf8(base.unwrap_or_default())
        .map_err(|_| anyhow::anyhow!("{} is a binary file", relative(base_path)))?;

    // an open buffer may have unsaved changes: show those
    let (current, right_title, tab_width, exists) = match editor.document_by_path(path) {
        Some(doc) => {
            let modified = if doc.is_modified() { " [modified]" } else { "" };
            (
                doc.text().to_string(),
                format!("Buffer: {}{modified}", relative(path)),
                doc.tab_width(),
                true,
            )
        }
        None => match std::fs::read(path) {
            Ok(bytes) => (
                String::from_utf8(bytes)
                    .map_err(|_| anyhow::anyhow!("{} is a binary file", relative(path)))?,
                format!("Working tree: {}", relative(path)),
                4,
                true,
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
                String::new(),
                format!("Working tree: {} (deleted)", relative(path)),
                4,
                false,
            ),
            Err(err) => anyhow::bail!("failed to read {}: {err}", relative(path)),
        },
    };

    let left = Side::new(left_title, base, base_path, editor);
    let right = Side::new(right_title, current, path, editor);
    Ok((left, right, exists.then(|| path.to_path_buf()), tab_width))
}

/// A side by side diff between the HEAD version of `base_path` and the current contents of
/// `path`, or `None` if they are the same.
fn head_diff(
    editor: &mut Editor,
    base_path: &Path,
    path: &Path,
) -> anyhow::Result<Option<DiffView>> {
    let (left, right, open, tab_width) = head_sides(editor, base_path, path)?;
    let view = DiffView::new(left, right, open, tab_width).with_soft_wrap(soft_wrap(editor, path));
    if !view.has_changes() {
        let path = vortex_stdx::path::get_relative_path(path);
        editor.set_status(format!("No changes in {}", path.display()));
        return Ok(None);
    }
    Ok(Some(view))
}

/// Opens the diff of an uncommitted change on top of the pickers of the commit log: HEAD against
/// the working tree (or the buffer, with its unsaved changes). With `live`, it follows the
/// working tree, and stays open when the file has no changes left.
pub(crate) fn open_uncommitted(
    editor: &mut Editor,
    compositor: &mut Compositor,
    change: &FileChange,
    live: Option<Live>,
) {
    let (base_path, path) = paths_of(change);
    let view = match head_diff(editor, &base_path, &path) {
        Ok(Some(view)) => view,
        Ok(None) => return,
        Err(err) => return editor.set_error(err.to_string()),
    };
    let view = view.with_on_open(|compositor| {
        // the uncommitted files picker and the commit log below it
        compositor.remove(picker::ID);
        compositor.remove(picker::ID);
    });
    let view = match live {
        Some(live) => view.with_live(live, move |editor| {
            // once a rename is committed, the new path is the one in HEAD
            let committed = base_path != path && {
                let workspace = vortex_loader::find_workspace_in(path.parent().unwrap_or(&path)).0;
                let trust_full = editor
                    .workspace_trust
                    .query(&workspace, vortex_loader::workspace_trust::TrustQuery::Git)
                    .is_trusted();
                editor
                    .diff_providers
                    .get_diff_base(&path, trust_full)
                    .is_some()
            };
            let base_path = if committed { &path } else { &base_path };
            let (left, right, open, _) = head_sides(editor, base_path, &path)?;
            Ok((left, right, open))
        }),
        None => view,
    };
    compositor.push(Box::new(view));
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
    let (base_path, path) = paths_of(change);
    cx.jobs.callback(async move {
        let call = move |editor: &mut Editor, compositor: &mut Compositor| match head_diff(
            editor, &base_path, &path,
        ) {
            Ok(Some(view)) => compositor.push(Box::new(view.with_on_open(|compositor| {
                compositor.remove(picker::ID);
            }))),
            Ok(None) => {}
            Err(err) => editor.set_error(err.to_string()),
        };
        Ok(crate::job::Callback::EditorCompositor(Box::new(call)))
    });
}

/// Opens the diff for the file of the current buffer.
pub(crate) fn open_current(editor: &mut Editor, compositor: &mut Compositor) {
    let Some(path) = vortex_view::doc!(editor).path().map(Path::to_path_buf) else {
        editor.set_error("The current buffer has no file");
        return;
    };
    match head_diff(editor, &path, &path) {
        Ok(Some(view)) => compositor.push(Box::new(view)),
        Ok(None) => {}
        Err(err) => editor.set_error(err.to_string()),
    }
}

/// Opens the diff of `change` in the commits from `oldest` to `newest` (the same commit for one):
/// from the first parent of `oldest` to `newest`. It's on top of the commit pickers, and opening
/// the file from the diff closes those pickers too.
pub(crate) fn open_commit(
    editor: &mut Editor,
    compositor: &mut Compositor,
    cwd: &Path,
    oldest: &CommitInfo,
    newest: &CommitInfo,
    change: &FileChange,
) -> anyhow::Result<()> {
    let relative = |path: &Path| {
        vortex_stdx::path::get_relative_path(path)
            .display()
            .to_string()
    };
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
    let view = DiffView::new(
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
    });
    if !view.has_changes() {
        editor.set_status(format!("No changes in {}", relative(&path)));
        return Ok(());
    }
    compositor.push(Box::new(view));
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
