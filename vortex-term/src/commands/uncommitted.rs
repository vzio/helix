//! The uncommitted changes of the commit log (fork feature): one row per hunk of every change
//! that isn't committed, staged or not, saved or not.
//!
//! - The hunks are those of the diff view: from HEAD to the buffer of the file when it is open
//!   (with its unsaved edits), else to the file on disk.
//! - Each is marked with where its change is, in any combination: `S` in the index (staged),
//!   `M` in the file on disk and not in the index (unstaged), `•` in the buffer and not on disk
//!   (unsaved).
//! - Lockfiles are collapsed to one row with their line counts, binary files are one row.
//!
//! The list follows the working tree while the commit log watches it, and the buffers as they
//! are edited. Enter opens the diff of a change at its hunk, where it can be staged and
//! reverted, or the buffer at a change that is only in the buffer.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use vortex_core::{Rope, Selection};
use vortex_vcs::staging::{self, touches, LineHunk};
use vortex_vcs::{DiffProviderRegistry, FileChange};
use vortex_view::{
    editor::{Action, ChangesConfig},
    graphics::Rect,
    DocumentId, Editor,
};
use parking_lot::Mutex;
use tui::buffer::Buffer as Surface;

use super::changes::{
    self, file_group, hunks, line_counts, nav_files, select_row, sort_groups, text, Body,
    ChangeRow, Marks, NavFiles, RowData, Totals, What,
};
use super::diff_view;
use crate::compositor::{self, Component, Compositor, Context, Event, EventResult};
use crate::job::Callback;
use crate::live::{Live, Pending, RepoWatch};
use crate::ui::{
    overlay::{overlaid, Overlay},
    picker, Picker,
};

/// The versions of a file, `None` where it has none.
#[derive(Clone, Copy, Default)]
pub(crate) struct Versions<'a> {
    /// In HEAD, at its old path for a rename.
    pub(crate) head: Option<&'a [u8]>,
    pub(crate) index: Option<&'a [u8]>,
    /// On disk.
    pub(crate) disk: Option<&'a [u8]>,
    /// In its buffer, when the buffer is modified.
    pub(crate) buffer: Option<&'a str>,
}

/// The rows of a changed file: its divider, then its hunks from HEAD to the buffer (or the
/// disk), each with where its change is.
pub(crate) fn file_rows(change: &FileChange, versions: Versions, lockfile: bool) -> Vec<ChangeRow> {
    let Versions {
        head,
        index,
        disk,
        buffer,
    } = versions;
    // a rename is a change even without a change of contents: staged when the index has the new
    // path, else only on disk
    let renamed = matches!(change, FileChange::Renamed { .. });
    let mut file_marks = Marks {
        staged: index != head || (renamed && index.is_some()),
        unstaged: disk != index || (renamed && index.is_none()),
        unsaved: buffer.is_some_and(|buffer| Some(buffer.as_bytes()) != disk),
    };
    if file_marks == Marks::default() {
        // listed by git all the same (a change of mode): on disk
        file_marks.unstaged = true;
    }
    let alone = |body| file_group(change, Some(file_marks), body, Vec::new());
    if lockfile {
        return alone(Body::Lockfile(line_counts(
            head.unwrap_or_default(),
            disk.unwrap_or_default(),
        )));
    }
    let (Ok(head), Ok(index), Ok(disk)) = (text(head), text(index), text(disk)) else {
        return alone(Body::Binary);
    };
    let (head, index, disk) = (
        head.unwrap_or_default(),
        index.unwrap_or_default(),
        disk.unwrap_or_default(),
    );
    let work = buffer.unwrap_or(disk);
    let hunks = hunks(change, head, work);
    if hunks.is_empty() {
        return alone(Body::Unchanged);
    }

    let staged = staging::line_hunks(head, index);
    let unstaged = staging::line_hunks(index, disk);
    let (unsaved, to_disk) = match buffer {
        Some(buffer) => (
            staging::line_hunks(disk, buffer),
            staging::line_hunks(buffer, disk),
        ),
        None => (Vec::new(), Vec::new()),
    };
    let hunks = hunks
        .into_iter()
        .map(|(line_hunk, row)| {
            let hunk = &line_hunk;
            // the lines of the hunk in the file on disk
            let on_disk = map_lines(&hunk.after, &to_disk);
            let mut marks = Marks {
                staged: staged.iter().any(|s| touches(&s.before, &hunk.before)),
                unstaged: unstaged.iter().any(|u| touches(&u.after, &on_disk)),
                unsaved: unsaved.iter().any(|u| touches(&u.after, &hunk.after)),
            };
            if marks == Marks::default() {
                // the diffs line up differently: it is a change of the file all the same
                marks.unsaved = file_marks.unsaved;
                marks.unstaged = !file_marks.unsaved;
            }
            (row, Some(marks))
        })
        .collect();
    file_group(change, Some(file_marks), Body::Hunks, hunks)
}

/// The lines of the new text that `lines` of the old text are at, given the `hunks` of the
/// diff between them. Lines inserted at either end of `lines` stay out, lines changed inside are
/// in.
pub(crate) fn map_lines(lines: &Range<u32>, hunks: &[LineHunk]) -> Range<u32> {
    let map = |line: u32, end: bool| {
        let mut delta = 0i64;
        for hunk in hunks {
            if hunk.before.start < line && line < hunk.before.end {
                return if end {
                    hunk.after.end
                } else {
                    hunk.after.start
                };
            }
            let before = if end {
                hunk.before.end < line || (hunk.before.end == line && !hunk.before.is_empty())
            } else {
                hunk.before.end <= line
            };
            if !before {
                break;
            }
            delta += hunk.after.len() as i64 - hunk.before.len() as i64;
        }
        (line as i64 + delta).max(0) as u32
    };
    let start = map(lines.start, false);
    start..map(lines.end, true).max(start)
}

/// The root of the working tree `cwd` is in (`cwd` outside of one).
pub(crate) fn work_dir(editor: &Editor, cwd: &Path, trust_full: bool) -> PathBuf {
    editor
        .diff_providers
        .repo_state(cwd, trust_full)
        .map(|state| vortex_stdx::path::canonicalize(state.work_dir))
        .unwrap_or_else(|_| cwd.to_path_buf())
}

/// The modified buffers of files in `work_dir`.
pub(crate) fn modified_buffers<'a>(
    editor: &'a Editor,
    work_dir: &'a Path,
) -> impl Iterator<Item = &'a vortex_view::Document> {
    editor.documents().filter(move |doc| {
        doc.is_modified() && doc.path().is_some_and(|path| path.starts_with(work_dir))
    })
}

/// A modified buffer of the working tree, for listing its unsaved edits off the main thread.
struct Buffer {
    path: PathBuf,
    text: Rope,
}

/// What the uncommitted changes need from the editor, for listing them off the main thread.
#[derive(Clone)]
struct Source {
    cwd: PathBuf,
    /// The root of the working tree: the buffers inside it are listed.
    work_dir: PathBuf,
    providers: DiffProviderRegistry,
    trust_full: bool,
    config: ChangesConfig,
}

impl Source {
    fn new(editor: &Editor, cwd: PathBuf) -> Self {
        let trust_full = super::git_log::trust_full(editor, &cwd);
        Self {
            work_dir: work_dir(editor, &cwd, trust_full),
            trust_full,
            providers: editor.diff_providers.clone(),
            config: editor.config().changes.clone(),
            cwd,
        }
    }

    /// The modified buffers of the working tree, with their versions: the list is read again
    /// when they change.
    fn buffers(&self, editor: &Editor) -> Vec<(DocumentId, i32, PathBuf)> {
        modified_buffers(editor, &self.work_dir)
            .map(|doc| (doc.id(), doc.version(), doc.path().unwrap().to_path_buf()))
            .collect()
    }

    /// The changes of the working tree and of the index since HEAD, untracked files included,
    /// with the unsaved edits of `buffers`; and the unsaved edits of the other buffers.
    fn list(&self, buffers: Vec<Buffer>) -> anyhow::Result<Vec<ChangeRow>> {
        let changes = self.providers.changed_files(&self.cwd, self.trust_full)?;
        let mut buffers: HashMap<PathBuf, String> = buffers
            .into_iter()
            .map(|buffer| (buffer.path, buffer.text.to_string()))
            .collect();
        let mut groups = Vec::new();
        for change in changes {
            let (base_path, path) = diff_view::paths_of(&change);
            // once a rename is committed, the new path is the one in HEAD
            let committed = (base_path != path)
                .then(|| self.providers.get_diff_base(&path, self.trust_full))
                .flatten();
            let head =
                committed.or_else(|| self.providers.get_diff_base(&base_path, self.trust_full));
            let index = self
                .providers
                .index_file(&path, self.trust_full)
                .ok()
                .flatten();
            let disk = std::fs::read(&path).ok();
            let buffer = buffers.remove(&path);
            let versions = Versions {
                head: head.as_deref(),
                index: index.as_deref(),
                disk: disk.as_deref(),
                buffer: buffer.as_deref(),
            };
            let lockfile = self.config.is_lockfile(change.path());
            groups.push(file_rows(&change, versions, lockfile));
        }
        // the files without changes on disk (or ignored by git): their unsaved edits, from what
        // is on disk
        for (path, buffer) in buffers {
            let disk = std::fs::read(&path).ok();
            // modified, then edited back
            if disk.as_deref() == Some(buffer.as_bytes()) {
                continue;
            }
            let change = match disk {
                Some(_) => FileChange::Modified { path },
                None => FileChange::Untracked { path },
            };
            let versions = Versions {
                head: disk.as_deref(),
                index: disk.as_deref(),
                disk: disk.as_deref(),
                buffer: Some(&buffer),
            };
            groups.push(file_rows(&change, versions, false));
        }
        Ok(sort_groups(groups))
    }
}

/// The list of the uncommitted changes, which follows the working tree while `live` sees it
/// change and the modified buffers as they are edited. The cursor stays on the same hunk (or in
/// the same file, or at the same place, when it is gone).
pub(crate) struct UncommittedChanges {
    picker: Picker<ChangeRow, RowData>,
    source: Source,
    live: Option<Live>,
    /// The modified buffers and their versions when the list was last read.
    buffers: Vec<(DocumentId, i32, PathBuf)>,
    pending: Pending<anyhow::Result<Vec<ChangeRow>>>,
    /// The files listed, for `n`/`N` in the diffs opened from the list.
    files: Arc<Mutex<NavFiles>>,
}

/// The modified buffers of `buffers`, to read off the main thread.
fn snapshot(editor: &Editor, buffers: &[(DocumentId, i32, PathBuf)]) -> Vec<Buffer> {
    buffers
        .iter()
        .filter_map(|(id, _, path)| {
            Some(Buffer {
                path: path.clone(),
                text: editor.document(*id)?.text().clone(),
            })
        })
        .collect()
}

impl UncommittedChanges {
    fn refresh(&mut self, editor: &Editor) {
        let buffers = self.source.buffers(editor);
        let tree = self.live.as_mut().is_some_and(Live::changed);
        if tree || buffers != self.buffers {
            let snapshot = snapshot(editor, &buffers);
            self.buffers = buffers;
            let source = self.source.clone();
            self.pending.run(move || source.list(snapshot));
        }
        match self.pending.take() {
            Some(Ok(rows)) => self.replace(rows),
            Some(Err(err)) => log::debug!("uncommitted changes: {err}"),
            None => {}
        }
    }

    fn replace(&mut self, rows: Vec<ChangeRow>) {
        let selected = self
            .picker
            .selection()
            .map(|row| (row.change.path().to_path_buf(), row.line()));
        let index = self.picker.cursor_index();
        *self.files.lock() = nav_files(&rows);
        self.picker.replace_items(rows, index);
        if let Some((path, line)) = selected {
            select_row(&mut self.picker, &path, Some(line));
        }
    }
}

impl Component for UncommittedChanges {
    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        self.refresh(cx.editor);
        self.picker.render(area, surface, cx);
    }

    fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        self.picker.handle_event(event, cx)
    }

    fn cursor(
        &self,
        area: Rect,
        editor: &Editor,
    ) -> (
        Option<vortex_core::Position>,
        vortex_view::graphics::CursorKind,
    ) {
        self.picker.cursor(area, editor)
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        self.picker.required_size(viewport)
    }

    fn id(&self) -> Option<&'static str> {
        self.picker.id()
    }
}

/// Opens the buffer of `path` at `line` (from 0), in the current view or as `action` says.
fn open_at(editor: &mut Editor, path: &Path, line: u32, action: Action) {
    if let Err(err) = editor.open(path, action) {
        return editor.set_error(format!("unable to open \"{}\": {err}", path.display()));
    }
    let (view, doc) = vortex_view::current!(editor);
    let text = doc.text();
    let line = (line as usize).min(text.len_lines().saturating_sub(1));
    doc.set_selection(view.id, Selection::point(text.line_to_char(line)));
    vortex_view::align_view(doc, view, vortex_view::Align::Center);
}

/// Closes `layers` pickers (the list and the commit log), once the current event is handled.
fn close_log(cx: &mut Context, layers: usize) {
    cx.jobs.callback(async move {
        let call = move |_: &mut Editor, compositor: &mut Compositor| {
            for _ in 0..layers {
                compositor.remove(picker::ID);
            }
        };
        Ok(Callback::EditorCompositor(Box::new(call)))
    });
}

/// Pushes the list of the uncommitted changes on top of the commit log. With a `watch`, it and
/// the diffs opened from it update while the working tree changes.
pub(crate) fn open(cx: &mut compositor::Context, cwd: PathBuf, watch: Option<Arc<RepoWatch>>) {
    cx.jobs.callback(async move {
        let call = move |editor: &mut Editor, compositor: &mut Compositor| {
            let source = Source::new(editor, cwd.clone());
            let buffers = source.buffers(editor);
            let rows = match source.list(snapshot(editor, &buffers)) {
                Ok(rows) => rows,
                Err(err) => {
                    editor.set_error(format!("{err}"));
                    return;
                }
            };
            editor.set_status(summary(&rows));
            let files = Arc::new(Mutex::new(nav_files(&rows)));
            let picker = changes_picker(editor, cwd, rows, watch.clone(), files.clone());
            let list = UncommittedChanges {
                picker,
                source,
                live: watch.map(Live::new),
                buffers,
                pending: Pending::new(),
                files,
            };
            compositor.push(Box::new(overlaid(list)));
        };
        Ok(Callback::EditorCompositor(Box::new(call)))
    });
}

/// `Uncommitted changes: 5 changes in 2 files, +12 -3, 1 unsaved`.
fn summary(rows: &[ChangeRow]) -> String {
    if rows.is_empty() {
        return "No uncommitted changes".into();
    }
    let totals = Totals::of(rows);
    let mut summary = format!("Uncommitted changes: {}", totals.describe());
    if totals.unsaved > 0 {
        summary += &format!(", {} unsaved", totals.unsaved);
    }
    summary
}

/// The list of the uncommitted changes. Enter opens the diff of the file at the hunk (at its
/// first hunk on a divider), or the buffer at a change that is only in the buffer;
/// `C-s`/`C-v`... open the file at the hunk, closing the commit log.
fn changes_picker(
    editor: &Editor,
    cwd: PathBuf,
    rows: Vec<ChangeRow>,
    watch: Option<Arc<RepoWatch>>,
    files: Arc<Mutex<NavFiles>>,
) -> Picker<ChangeRow, RowData> {
    let enter = move |cx: &mut Context, row: &ChangeRow| {
        let line = match &row.what {
            What::Hunk(hunk) if row.marks.is_some_and(Marks::only_unsaved) => {
                open_at(
                    cx.editor,
                    row.change.path(),
                    hunk.lines.start,
                    Action::Replace,
                );
                // the list (which stays open for the diffs) and the commit log
                return close_log(cx, 2);
            }
            What::Hunk(hunk) => Some(hunk.lines.start),
            What::File(_) => None,
        };
        let change = row.change.clone();
        let live = watch.clone().map(Live::new);
        let files = files.clone();
        cx.jobs.callback(async move {
            let call = move |editor: &mut Editor, compositor: &mut Compositor| {
                diff_view::open_uncommitted(
                    editor,
                    compositor,
                    &change,
                    line,
                    live,
                    Rc::new(move || files.lock().clone()),
                    Rc::new(|compositor, change| {
                        if let Some(overlay) = compositor.find::<Overlay<UncommittedChanges>>() {
                            select_row(&mut overlay.content.picker, change.path(), None);
                        }
                    }),
                )
            };
            Ok(Callback::EditorCompositor(Box::new(call)))
        });
    };
    let open = |cx: &mut Context, row: &ChangeRow, action| {
        // the file as it is now, at the change, closing the commit log below too
        open_at(cx.editor, row.change.path(), row.line(), action);
        close_log(cx, 1);
    };
    changes::picker(editor, cwd, rows, true, enter, open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::changes::{tests::listed, PreviewLine};

    fn modified(path: &str) -> FileChange {
        FileChange::Modified {
            path: PathBuf::from(path),
        }
    }

    /// Each row as listed (a divider's title after `── `, a hunk's signs and location), with
    /// its marks.
    fn summary(rows: &[ChangeRow]) -> Vec<(String, String)> {
        listed(rows)
            .into_iter()
            .zip(rows)
            .map(|(listed, row)| (listed, row.marks.unwrap().symbols().concat()))
            .collect()
    }

    fn rows(
        change: &FileChange,
        head: &str,
        index: &str,
        disk: &str,
        buffer: Option<&str>,
    ) -> Vec<ChangeRow> {
        let versions = Versions {
            head: Some(head.as_bytes()),
            index: Some(index.as_bytes()),
            disk: Some(disk.as_bytes()),
            buffer,
        };
        file_rows(change, versions, false)
    }

    fn row(listed: &str, marks: &str) -> (String, String) {
        (listed.to_string(), marks.to_string())
    }

    const HEAD: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";

    #[test]
    fn hunks_with_their_marks() {
        // b is staged, f partly (F in the index, FF on disk), h only on disk
        let index = "a\nB\nc\nd\ne\nF\ng\nh\n";
        let disk = "a\nB\nc\nd\ne\nFF\ng\nH\n";
        let rows = rows(&modified("/r/x.txt"), HEAD, index, disk, None);
        assert_eq!(
            summary(&rows),
            [
                row("── x.txt  3 hunks  +3 -3  2 staged", "SM "),
                row("-+ x.txt:2", "S  "),
                row("-+ x.txt:6", "SM "),
                row("-+ x.txt:8", " M ")
            ]
        );
        let What::Hunk(hunk) = &rows[1].what else {
            panic!("not a hunk");
        };
        assert_eq!(
            hunk.preview.lines,
            [
                (PreviewLine::Context, "a".to_string()),
                (PreviewLine::Removed, "b".to_string()),
                (PreviewLine::Added, "B".to_string()),
                (PreviewLine::Context, "c".to_string()),
                (PreviewLine::Context, "d".to_string()),
                (PreviewLine::Context, "e".to_string()),
            ]
        );
    }

    #[test]
    fn staged_unstaged_and_unsaved_in_one_hunk() {
        // d: D staged, DD on disk, DDD in the buffer, which also has two new lines at the top
        // and an unsaved h
        let index = "a\nb\nc\nD\ne\nf\ng\nh\n";
        let disk = "a\nb\nc\nDD\ne\nf\ng\nh\n";
        let buffer = "0\n1\na\nb\nc\nDDD\ne\nf\ng\nH\n";
        let rows = rows(&modified("/r/x.txt"), HEAD, index, disk, Some(buffer));
        assert_eq!(
            summary(&rows),
            [
                row("── x.txt  3 hunks  +4 -2  1 staged  3 unsaved", "SM•"),
                row("+  x.txt:1-2", "  •"),
                row("-+ x.txt:6", "SM•"),
                row("-+ x.txt:10", "  •"),
            ]
        );
    }

    #[test]
    fn an_unsaved_hunk_next_to_one_on_disk() {
        // c changed on disk, d in the buffer: one hunk of the diff view, with both marks
        let disk = "a\nb\nC\nd\ne\nf\ng\nh\n";
        let buffer = "a\nb\nC\nD\ne\nf\ng\nh\n";
        let rows = rows(&modified("/r/x.txt"), HEAD, HEAD, disk, Some(buffer));
        assert_eq!(
            summary(&rows),
            [
                row("── x.txt  1 hunk  +2 -2  1 unsaved", " M•"),
                row("-+ x.txt:3-4", " M•")
            ]
        );
    }

    #[test]
    fn whole_files() {
        let untracked = FileChange::Untracked {
            path: "/r/new.md".into(),
        };
        let versions = Versions {
            disk: Some(b"one\ntwo\n"),
            ..Versions::default()
        };
        let rows = file_rows(&untracked, versions, false);
        assert_eq!(
            summary(&rows),
            [
                row("── new.md (untracked)  1 hunk  +2", " M "),
                row("++ new.md:1-2", " M ")
            ]
        );

        let deleted = FileChange::Deleted {
            path: "/r/old.md".into(),
        };
        let versions = Versions {
            head: Some(b"x\ny\n"),
            ..Versions::default()
        };
        let rows = file_rows(&deleted, versions, false);
        assert_eq!(
            summary(&rows),
            [
                row("── old.md (deleted)  1 hunk  -2  1 staged", "S  "),
                row("-- old.md:1", "S  ")
            ]
        );

        let versions = Versions {
            head: Some(b"\0a"),
            index: Some(b"\0a"),
            disk: Some(b"\0b"),
            buffer: None,
        };
        let rows = file_rows(&modified("/r/a.png"), versions, false);
        assert_eq!(summary(&rows), [row("── a.png (binary)", " M ")]);

        let versions = Versions {
            head: Some(b"a\nb\n"),
            index: Some(b"a\nB\nc\n"),
            disk: Some(b"a\nB\nc\n"),
            buffer: Some("a\nB\nc\nd\n"),
        };
        let rows = file_rows(&modified("/r/Cargo.lock"), versions, true);
        assert_eq!(
            summary(&rows),
            [row("── Cargo.lock  +2 -1  staged  unsaved", "S •")]
        );

        // staged, then undone on disk
        let undone = rows_of(&modified("/r/u.txt"), "a\n", "b\n", "a\n");
        assert_eq!(
            summary(&undone),
            [row("── u.txt (no lines changed)  staged", "SM ")]
        );
        assert!(undone[0].is_entry());
    }

    #[test]
    fn a_staged_rename_without_changes() {
        let renamed = FileChange::Renamed {
            from_path: "/r/a.txt".into(),
            to_path: "/r/b.txt".into(),
        };
        let rows = rows_of(&renamed, "x\n", "x\n", "x\n");
        assert_eq!(
            summary(&rows),
            [row("── a.txt → b.txt (no lines changed)  staged", "S  ")]
        );
    }

    fn rows_of(change: &FileChange, head: &str, index: &str, disk: &str) -> Vec<ChangeRow> {
        rows(change, head, index, disk, None)
    }

    #[test]
    fn mapping_lines() {
        let hunks = staging::line_hunks("a\nb\nc\nd\n", "x\na\nB\nb2\nc\nd\n");
        // c (2..3) is after the insertion at the top and b's growth by one line
        assert_eq!(map_lines(&(2..3), &hunks), 4..5);
        // b itself, changed: the lines that replaced it
        assert_eq!(map_lines(&(1..2), &hunks), 2..4);
        // the place of removed lines at the start: after the inserted line
        assert_eq!(map_lines(&(0..0), &hunks), 1..1);
        assert_eq!(map_lines(&(0..1), &[]), 0..1);
    }

    #[test]
    fn files_are_sorted_with_lockfiles_last() {
        let groups = vec![
            file_rows(
                &modified("/r/Cargo.lock"),
                Versions {
                    head: Some(b"a\n"),
                    index: Some(b"a\n"),
                    disk: Some(b"b\n"),
                    buffer: None,
                },
                true,
            ),
            rows_of(
                &modified("/r/b.rs"),
                "a\nb\nc\nd\ne\n",
                "a\nb\nc\nd\ne\n",
                "A\nb\nc\nd\nE\n",
            ),
            rows_of(&modified("/r/a.rs"), "a\n", "a\n", "b\n"),
        ];
        let rows = sort_groups(groups);
        assert_eq!(
            listed(&rows),
            [
                "── a.rs  1 hunk  +1 -1",
                "-+ a.rs:1",
                "── b.rs  2 hunks  +2 -2",
                "-+ b.rs:1",
                "-+ b.rs:5",
                "── Cargo.lock  +1 -1",
            ]
        );
        assert_eq!(
            nav_files(&rows)
                .iter()
                .map(|(change, skip)| (change.path().to_str().unwrap(), *skip))
                .collect::<Vec<_>>(),
            [
                ("/r/a.rs", false),
                ("/r/b.rs", false),
                ("/r/Cargo.lock", true)
            ]
        );
        assert_eq!(
            super::summary(&rows),
            "Uncommitted changes: 4 changes in 3 files, +4 -4"
        );
    }
}
