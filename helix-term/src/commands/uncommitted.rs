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

use helix_core::{Rope, Selection};
use helix_vcs::staging::{self, touches, LineHunk};
use helix_vcs::{DiffProviderRegistry, FileChange};
use helix_view::{
    editor::{Action, ChangesConfig},
    graphics::{Rect, Style},
    DocumentId, Editor,
};
use parking_lot::Mutex;
use tui::{
    buffer::Buffer as Surface,
    text::{Span, Spans},
};

use super::changes::{line_counts, LockfileLines, NavFiles};
use super::diff_view;
use crate::compositor::{self, Component, Compositor, Context, Event, EventResult};
use crate::job::Callback;
use crate::live::{Live, Pending, RepoWatch};
use crate::ui::{
    overlay::{overlaid, Overlay},
    picker, Picker, PickerColumn,
};
use crate::{ctrl, key};

/// Lines of context around a hunk in the preview.
const CONTEXT: usize = 3;

/// Where the change of a row is. Any combination: a hunk can be staged, changed again on disk
/// since, and edited again in the buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Marks {
    /// In the index: HEAD and the index differ there.
    pub(crate) staged: bool,
    /// On disk and not in the index: the index and the file differ there.
    pub(crate) unstaged: bool,
    /// In the buffer and not on disk: the file and its modified buffer differ there.
    pub(crate) unsaved: bool,
}

impl Marks {
    /// The letters in their columns: `SM•`, `S  `, ` M•`...
    fn symbols(self) -> [&'static str; 3] {
        [
            if self.staged { "S" } else { " " },
            if self.unstaged { "M" } else { " " },
            if self.unsaved { "•" } else { " " },
        ]
    }

    /// `staged, unsaved`.
    fn describe(self) -> String {
        [
            (self.staged, "staged"),
            (self.unstaged, "unstaged"),
            (self.unsaved, "unsaved"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join(", ")
    }

    /// Only in the buffer: Enter opens the buffer rather than the diff.
    fn only_unsaved(self) -> bool {
        self.unsaved && !self.staged && !self.unstaged
    }
}

/// What a row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum What {
    /// A hunk. `lines` are the lines it has now, from 0, in the buffer when the file is open
    /// (else in the file): what the right side of its diff shows. An empty range is where lines
    /// were removed.
    Hunk {
        lines: Range<u32>,
        preview: Arc<HunkPreview>,
    },
    /// A lockfile, collapsed to one row.
    Lockfile(LockfileLines),
    Binary,
    /// A change without changed lines: an empty new file, a change of mode, or a change staged
    /// and then undone in the working tree.
    File,
}

/// The lines of a hunk with some context, for the preview.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HunkPreview {
    pub(crate) lines: Vec<(PreviewLine, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewLine {
    Context,
    Removed,
    Added,
}

/// A row of the uncommitted changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangeRow {
    /// The change of the file. A file with only unsaved edits is `Modified`, or `Untracked` if
    /// it isn't on disk yet.
    pub(crate) change: FileChange,
    pub(crate) marks: Marks,
    pub(crate) what: What,
}

impl ChangeRow {
    /// The first line of the row, from 0 (0 for a whole file).
    pub(crate) fn line(&self) -> u32 {
        match &self.what {
            What::Hunk { lines, .. } => lines.start,
            _ => 0,
        }
    }

    fn is_lockfile(&self) -> bool {
        matches!(self.what, What::Lockfile(_))
    }
}

/// The text of a version of a file (`None` when it doesn't exist), or `Err` when it is binary.
fn text(bytes: Option<&[u8]>) -> Result<Option<&str>, ()> {
    match bytes {
        None => Ok(None),
        Some(bytes) if bytes.contains(&0) => Err(()),
        Some(bytes) => std::str::from_utf8(bytes).map(Some).map_err(|_| ()),
    }
}

/// The lines of `text` without their line endings.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n')
        .map(|line| line.trim_end_matches('\n').trim_end_matches('\r'))
        .collect()
}

/// The preview of `hunk` of the diff from `before` to `after`.
fn preview(before: &[&str], after: &[&str], hunk: &LineHunk) -> HunkPreview {
    let (start, end) = (hunk.after.start as usize, hunk.after.end as usize);
    let mut lines = Vec::new();
    let mut push = |kind, text: &[&str]| {
        lines.extend(text.iter().map(|line| (kind, line.to_string())));
    };
    push(
        PreviewLine::Context,
        &after[start.saturating_sub(CONTEXT)..start],
    );
    push(
        PreviewLine::Removed,
        &before[hunk.before.start as usize..hunk.before.end as usize],
    );
    push(PreviewLine::Added, &after[start..end]);
    push(
        PreviewLine::Context,
        &after[end..(end + CONTEXT).min(after.len())],
    );
    HunkPreview { lines }
}

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

/// The rows of a changed file: its hunks from HEAD to the buffer (or the disk), each with
/// where its change is.
pub(crate) fn file_rows(change: &FileChange, versions: Versions, lockfile: bool) -> Vec<ChangeRow> {
    let Versions {
        head,
        index,
        disk,
        buffer,
    } = versions;
    let file_marks = Marks {
        staged: index != head,
        unstaged: disk != index,
        unsaved: buffer.is_some_and(|buffer| Some(buffer.as_bytes()) != disk),
    };
    let row = |what| {
        vec![ChangeRow {
            change: change.clone(),
            marks: file_marks,
            what,
        }]
    };
    if lockfile {
        return row(What::Lockfile(line_counts(
            head.unwrap_or_default(),
            disk.unwrap_or_default(),
        )));
    }
    let (Ok(head), Ok(index), Ok(disk)) = (text(head), text(index), text(disk)) else {
        return row(What::Binary);
    };
    let (head, index, disk) = (
        head.unwrap_or_default(),
        index.unwrap_or_default(),
        disk.unwrap_or_default(),
    );
    let work = buffer.unwrap_or(disk);
    let hunks = staging::line_hunks(head, work);
    if hunks.is_empty() {
        return row(What::File);
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
    let (before, after) = (lines(head), lines(work));
    hunks
        .iter()
        .map(|hunk| {
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
            ChangeRow {
                change: change.clone(),
                marks,
                what: What::Hunk {
                    lines: hunk.after.clone(),
                    preview: Arc::new(preview(&before, &after, hunk)),
                },
            }
        })
        .collect()
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

/// Sorts the rows by path, with the lockfiles last, then by line.
pub(crate) fn sort_rows(rows: &mut [ChangeRow]) {
    rows.sort_by(|a, b| {
        let key = |row: &ChangeRow| {
            (
                row.is_lockfile(),
                row.change.path().to_path_buf(),
                row.line(),
            )
        };
        key(a).cmp(&key(b))
    });
}

/// The files of `rows` for `n`/`N` in their diffs, in the order of the list.
fn nav_files(rows: &[ChangeRow]) -> NavFiles {
    let mut files: NavFiles = Vec::new();
    for row in rows {
        if files
            .last()
            .is_none_or(|(change, _)| change.path() != row.change.path())
        {
            files.push((row.change.clone(), row.is_lockfile()));
        }
    }
    files
}

/// The root of the working tree `cwd` is in (`cwd` outside of one).
pub(crate) fn work_dir(editor: &Editor, cwd: &Path, trust_full: bool) -> PathBuf {
    editor
        .diff_providers
        .repo_state(cwd, trust_full)
        .map(|state| helix_stdx::path::canonicalize(state.work_dir))
        .unwrap_or_else(|_| cwd.to_path_buf())
}

/// The modified buffers of files in `work_dir`.
pub(crate) fn modified_buffers<'a>(
    editor: &'a Editor,
    work_dir: &'a Path,
) -> impl Iterator<Item = &'a helix_view::Document> {
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
        let mut rows = Vec::new();
        for change in changes {
            let (base_path, path) = diff_view::paths_of(&change);
            // once a rename is committed, the new path is the one in HEAD
            let head = match self.providers.get_diff_base(&path, self.trust_full) {
                Some(head) if base_path != path => Some(head),
                _ => self.providers.get_diff_base(&base_path, self.trust_full),
            };
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
            rows.extend(file_rows(&change, versions, lockfile));
        }
        // the files without changes on disk (or ignored by git): their unsaved edits, from what
        // is on disk
        for (path, buffer) in buffers {
            let disk = std::fs::read(&path).ok();
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
            rows.extend(file_rows(&change, versions, false));
        }
        sort_rows(&mut rows);
        Ok(rows)
    }
}

/// The styles of the rows and of the preview.
pub(crate) struct RowData {
    cwd: PathBuf,
    staged: Style,
    unstaged: Style,
    unsaved: Style,
    quiet: Style,
    added: Style,
    removed: Style,
}

impl RowData {
    fn new(editor: &Editor, cwd: PathBuf) -> Self {
        let theme = &editor.theme;
        Self {
            cwd,
            staged: theme.get("diff.plus"),
            unstaged: theme.get("diff.delta"),
            unsaved: theme.get("warning"),
            quiet: theme
                .try_get("ui.text.inactive")
                .unwrap_or_else(|| theme.get("comment")),
            added: theme.get("diff.plus"),
            removed: theme.get("diff.minus"),
        }
    }

    /// The marks of a row, each letter in its color.
    fn marks(&self, marks: Marks) -> Spans<'static> {
        let styles = [self.staged, self.unstaged, self.unsaved];
        Spans::from(
            marks
                .symbols()
                .into_iter()
                .zip(styles)
                .map(|(symbol, style)| Span::styled(symbol, style))
                .collect::<Vec<_>>(),
        )
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.cwd)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// `src/main.rs:10-12`, with what happened to the file when it isn't just modified.
    pub(crate) fn location(&self, row: &ChangeRow) -> String {
        let path = self.relative(row.change.path());
        let place = match &row.what {
            What::Hunk { lines, .. } => match lines.len() {
                // the place of removed lines: the line before them
                0 => format!("{path}:{}", lines.start.max(1)),
                1 => format!("{path}:{}", lines.start + 1),
                _ => format!("{path}:{}-{}", lines.start + 1, lines.end),
            },
            _ => path,
        };
        let note = match (&row.change, &row.what) {
            (_, What::Lockfile(LockfileLines::Counts { added, removed })) => {
                format!(" (+{added} -{removed})")
            }
            (_, What::Lockfile(LockfileLines::Binary)) | (_, What::Binary) => " (binary)".into(),
            (FileChange::Untracked { .. }, _) => " (untracked)".into(),
            (FileChange::Added { .. }, _) => " (added)".into(),
            (FileChange::Deleted { .. }, _) => " (deleted)".into(),
            (FileChange::Conflict { .. }, _) => " (conflict)".into(),
            (FileChange::Renamed { from_path, .. }, _) => {
                format!(" (renamed from {})", self.relative(from_path))
            }
            (FileChange::Modified { .. }, _) => String::new(),
        };
        place + note.as_str()
    }
}

/// Draws the preview of `row`: where it is, then its hunk with a few lines of context.
fn draw_preview(
    row: &ChangeRow,
    data: &RowData,
    editor: &Editor,
    area: Rect,
    surface: &mut Surface,
) {
    let text = editor.theme.get("ui.text");
    let width = area.width as usize;
    let mut y = area.y;
    let mut line = |surface: &mut Surface, text: &str, style: Style| {
        if y < area.bottom() {
            surface.set_stringn(area.x, y, text, width, style);
            y += 1;
        }
    };
    let title = format!("{} · {}", data.location(row), row.marks.describe());
    line(surface, &title, text);
    line(surface, "", text);
    match &row.what {
        What::Hunk { preview, .. } => {
            for (kind, content) in &preview.lines {
                let (prefix, style) = match kind {
                    PreviewLine::Context => (' ', text),
                    PreviewLine::Removed => ('-', data.removed),
                    PreviewLine::Added => ('+', data.added),
                };
                let content = content.replace('\t', "    ");
                line(surface, &format!("{prefix} {content}"), style);
            }
        }
        What::Lockfile(_) => line(surface, "A lockfile: Enter shows its diff.", data.quiet),
        What::Binary => line(surface, "A binary file.", data.quiet),
        What::File => line(surface, "No lines changed.", data.quiet),
    }
}

/// Puts the cursor of `picker` on the row of `path` at `line`, or else on the first row of
/// `path`. Returns whether it found one.
fn select_row(picker: &mut Picker<ChangeRow, RowData>, path: &Path, line: Option<u32>) -> bool {
    // the matched rows are ready (unless there are many)
    let mut first = None;
    let mut i = 0;
    while let Some(row) = picker.matched_item(i) {
        if row.change.path() == path {
            if line.is_none_or(|line| row.line() == line) {
                picker.set_cursor_index(i);
                return true;
            }
            first.get_or_insert(i);
        }
        i += 1;
    }
    if let Some(first) = first {
        picker.set_cursor_index(first);
    }
    first.is_some()
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
        Option<helix_core::Position>,
        helix_view::graphics::CursorKind,
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
    let (view, doc) = helix_view::current!(editor);
    let text = doc.text();
    let line = (line as usize).min(text.len_lines().saturating_sub(1));
    doc.set_selection(view.id, Selection::point(text.line_to_char(line)));
    helix_view::align_view(doc, view, helix_view::Align::Center);
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

/// `Uncommitted changes: 5 changes in 2 files, 1 unsaved`.
fn summary(rows: &[ChangeRow]) -> String {
    if rows.is_empty() {
        return "No uncommitted changes".into();
    }
    let plural = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    let files = nav_files(rows).len();
    let unsaved = rows.iter().filter(|row| row.marks.unsaved).count();
    let mut summary = format!(
        "Uncommitted changes: {} in {}",
        plural(rows.len(), "change", "changes"),
        plural(files, "file", "files"),
    );
    if unsaved > 0 {
        summary += &format!(", {unsaved} unsaved");
    }
    summary
}

/// The picker of the uncommitted changes. Enter opens the diff of the file at the hunk, or the
/// buffer at a change that is only in the buffer; `C-s`/`C-v`... open the file at the hunk,
/// closing the commit log.
fn changes_picker(
    editor: &Editor,
    cwd: PathBuf,
    rows: Vec<ChangeRow>,
    watch: Option<Arc<RepoWatch>>,
    files: Arc<Mutex<NavFiles>>,
) -> Picker<ChangeRow, RowData> {
    let columns = [
        PickerColumn::new("marks", |row: &ChangeRow, data: &RowData| {
            data.marks(row.marks).into()
        }),
        PickerColumn::new("change", |row: &ChangeRow, data: &RowData| {
            let location = data.location(row);
            if row.is_lockfile() {
                Span::styled(location, data.quiet).into()
            } else {
                location.into()
            }
        }),
    ];
    let data = RowData::new(editor, cwd);
    let enter = move |cx: &mut Context, row: &ChangeRow| {
        if let (true, What::Hunk { lines, .. }) = (row.marks.only_unsaved(), &row.what) {
            open_at(cx.editor, row.change.path(), lines.start, Action::Replace);
            // the list (which stays open for the diffs) and the commit log
            return close_log(cx, 2);
        }
        let change = row.change.clone();
        let line = match &row.what {
            What::Hunk { lines, .. } => Some(lines.start),
            _ => None,
        };
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
    Picker::new(columns, 1, rows, data, |cx, row: &ChangeRow, action| {
        // the file as it is now, at the change, closing the commit log below too
        open_at(cx.editor, row.change.path(), row.line(), action);
        close_log(cx, 1);
    })
    .with_drawn_preview(draw_preview)
    .with_modes()
    .with_stacked_key_handler(key!(Enter), enter.clone())
    .with_stacked_key_handler(ctrl!('g'), enter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modified(path: &str) -> FileChange {
        FileChange::Modified {
            path: PathBuf::from(path),
        }
    }

    /// The location and the marks of each row.
    fn summary(rows: &[ChangeRow]) -> Vec<(String, String)> {
        let data = RowData {
            cwd: PathBuf::from("/r"),
            staged: Style::default(),
            unstaged: Style::default(),
            unsaved: Style::default(),
            quiet: Style::default(),
            added: Style::default(),
            removed: Style::default(),
        };
        rows.iter()
            .map(|row| (data.location(row), row.marks.symbols().concat()))
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

    fn row(location: &str, marks: &str) -> (String, String) {
        (location.to_string(), marks.to_string())
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
                row("x.txt:2", "S  "),
                row("x.txt:6", "SM "),
                row("x.txt:8", " M ")
            ]
        );
        let What::Hunk { preview, .. } = &rows[0].what else {
            panic!("not a hunk");
        };
        assert_eq!(
            preview.lines,
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
                row("x.txt:1-2", "  •"),
                row("x.txt:6", "SM•"),
                row("x.txt:10", "  •"),
            ]
        );
    }

    #[test]
    fn an_unsaved_hunk_next_to_one_on_disk() {
        // c changed on disk, d in the buffer: one hunk of the diff view, with both marks
        let disk = "a\nb\nC\nd\ne\nf\ng\nh\n";
        let buffer = "a\nb\nC\nD\ne\nf\ng\nh\n";
        let rows = rows(&modified("/r/x.txt"), HEAD, HEAD, disk, Some(buffer));
        assert_eq!(summary(&rows), [row("x.txt:3-4", " M•")]);
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
        assert_eq!(summary(&rows), [row("new.md:1-2 (untracked)", " M ")]);

        let deleted = FileChange::Deleted {
            path: "/r/old.md".into(),
        };
        let versions = Versions {
            head: Some(b"x\ny\n"),
            ..Versions::default()
        };
        let rows = file_rows(&deleted, versions, false);
        assert_eq!(summary(&rows), [row("old.md:1 (deleted)", "S  ")]);

        let versions = Versions {
            head: Some(b"\0a"),
            index: Some(b"\0a"),
            disk: Some(b"\0b"),
            buffer: None,
        };
        let rows = file_rows(&modified("/r/a.png"), versions, false);
        assert_eq!(summary(&rows), [row("a.png (binary)", " M ")]);

        let versions = Versions {
            head: Some(b"a\nb\n"),
            index: Some(b"a\nB\nc\n"),
            disk: Some(b"a\nB\nc\n"),
            buffer: Some("a\nB\nc\nd\n"),
        };
        let rows = file_rows(&modified("/r/Cargo.lock"), versions, true);
        assert_eq!(summary(&rows), [row("Cargo.lock (+2 -1)", "S •")]);

        // staged, then undone on disk
        let undone = rows_of(&modified("/r/u.txt"), "a\n", "b\n", "a\n");
        assert_eq!(undone[0].what, What::File);
        assert_eq!(summary(&undone), [row("u.txt", "SM ")]);
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
    fn rows_are_sorted_with_lockfiles_last() {
        let row_at = |path: &str, line: u32, what: Option<What>| ChangeRow {
            change: modified(path),
            marks: Marks {
                unstaged: true,
                ..Marks::default()
            },
            what: what.unwrap_or(What::Hunk {
                lines: line..line + 1,
                preview: Arc::new(HunkPreview { lines: Vec::new() }),
            }),
        };
        let mut rows = vec![
            row_at(
                "/r/Cargo.lock",
                0,
                Some(What::Lockfile(LockfileLines::Binary)),
            ),
            row_at("/r/b.rs", 4, None),
            row_at("/r/a.rs", 9, None),
            row_at("/r/b.rs", 1, None),
        ];
        sort_rows(&mut rows);
        assert_eq!(
            summary(&rows),
            [
                row("a.rs:10", " M "),
                row("b.rs:2", " M "),
                row("b.rs:5", " M "),
                row("Cargo.lock (binary)", " M "),
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
    }

    #[test]
    fn describing_marks() {
        let marks = Marks {
            staged: true,
            unstaged: false,
            unsaved: true,
        };
        assert_eq!(marks.describe(), "staged, unsaved");
        assert!(!marks.only_unsaved());
        assert!(Marks {
            unsaved: true,
            ..Marks::default()
        }
        .only_unsaved());
    }
}
