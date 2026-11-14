//! The lists of changes of the commit log (fork feature): the uncommitted changes, and the
//! changes of a commit or of a range of commits, in the same shape.
//!
//! - One row per hunk, grouped under a divider row per file with its counts:
//!   `── src/main.rs  3 hunks  +24 -7  1 staged`.
//! - Each hunk is marked with what it does: `+` adds lines, `-` removes lines, `-+` changes
//!   them, `++` is in a new file, `--` in a deleted one.
//! - Lockfiles are collapsed to their divider, with their line counts, after the other files;
//!   binary files, and files without changed lines, are a divider alone. These dividers are rows
//!   the cursor stops on: Enter opens their diff.
//!
//! The uncommitted changes also mark where the change of each hunk is (see [`Marks`] and
//! [`super::uncommitted`]).

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use imara_diff::{Algorithm, Diff, InternedInput};
use tui::{
    buffer::Buffer as Surface,
    text::{Span, Spans},
};
use vortex_vcs::staging::{self, LineHunk};
use vortex_vcs::FileChange;
use vortex_view::{
    editor::{Action, ChangesConfig},
    graphics::{Rect, Style},
    Editor,
};

use crate::compositor::{Compositor, Context};
use crate::ui::{overlay::Overlay, Picker, PickerColumn};
use crate::{ctrl, key};

/// Lines of context around a hunk in the preview.
const CONTEXT: usize = 3;

/// Where the change of an uncommitted hunk is. Any combination: a hunk can be staged, changed
/// again on disk since, and edited again in the buffer.
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
    pub(crate) fn symbols(self) -> [&'static str; 3] {
        [
            if self.staged { "S" } else { " " },
            if self.unstaged { "M" } else { " " },
            if self.unsaved { "•" } else { " " },
        ]
    }

    /// `staged, unsaved`.
    pub(crate) fn describe(self) -> String {
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
    pub(crate) fn only_unsaved(self) -> bool {
        self.unsaved && !self.staged && !self.unstaged
    }
}

/// What a hunk does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edit {
    /// Only adds lines: `+`.
    Add,
    /// Only removes lines: `-`.
    Remove,
    /// Removes and adds lines: `-+`.
    Change,
    /// Is in a file that is created (new or untracked): `++`.
    Create,
    /// Is in a file that is deleted: `--`.
    Delete,
}

impl Edit {
    fn of(change: &FileChange, hunk: &LineHunk) -> Edit {
        match change {
            FileChange::Added { .. } | FileChange::Untracked { .. } => Edit::Create,
            FileChange::Deleted { .. } => Edit::Delete,
            _ if hunk.before.is_empty() => Edit::Add,
            _ if hunk.after.is_empty() => Edit::Remove,
            _ => Edit::Change,
        }
    }

    /// The signs, `-` before `+`.
    pub(crate) fn signs(self) -> &'static str {
        match self {
            Edit::Add => "+",
            Edit::Remove => "-",
            Edit::Change => "-+",
            Edit::Create => "++",
            Edit::Delete => "--",
        }
    }
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

/// A hunk of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hunk {
    /// The lines it has now, from 0: what the right side of its diff shows (for the uncommitted
    /// changes, the buffer when the file is open, else the file). An empty range is where lines
    /// were removed.
    pub(crate) lines: Range<u32>,
    pub(crate) added: u32,
    pub(crate) removed: u32,
    pub(crate) edit: Edit,
    pub(crate) preview: Arc<HunkPreview>,
}

/// What the divider of a file stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Body {
    /// Its hunks, listed under it.
    Hunks,
    /// A lockfile, collapsed to its divider.
    Lockfile(LockfileLines),
    Binary,
    /// A change without changed lines: an empty new file, a change of mode, or a change staged
    /// and then undone in the working tree.
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockfileLines {
    Counts { added: u32, removed: u32 },
    Binary,
}

/// The divider of a file: what it has.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FileSummary {
    pub(crate) body: Body,
    pub(crate) hunks: u32,
    pub(crate) added: u32,
    pub(crate) removed: u32,
    /// The hunks that are staged, and unsaved (uncommitted changes).
    pub(crate) staged: u32,
    pub(crate) unsaved: u32,
    /// The first line of its first hunk, and its preview.
    pub(crate) first: Option<(u32, Arc<HunkPreview>)>,
}

/// What a row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum What {
    /// The divider of a file.
    File(Arc<FileSummary>),
    Hunk(Hunk),
}

/// A row of a list of changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangeRow {
    /// The change of the file. In the uncommitted changes, a file with only unsaved edits is
    /// `Modified`, or `Untracked` if it isn't on disk yet.
    pub(crate) change: FileChange,
    /// Where the change is, in the uncommitted changes: of the hunk, or of the whole file for
    /// its divider. `None` in the changes of commits.
    pub(crate) marks: Option<Marks>,
    pub(crate) what: What,
}

impl ChangeRow {
    /// The first line of the row, from 0: of the first hunk for a divider, 0 without one.
    pub(crate) fn line(&self) -> u32 {
        match &self.what {
            What::Hunk(hunk) => hunk.lines.start,
            What::File(summary) => summary.first.as_ref().map_or(0, |(line, _)| *line),
        }
    }

    pub(crate) fn is_lockfile(&self) -> bool {
        matches!(&self.what, What::File(summary) if matches!(summary.body, Body::Lockfile(_)))
    }

    /// The divider of a file.
    pub(crate) fn is_divider(&self) -> bool {
        matches!(self.what, What::File(_))
    }

    /// The divider of a file without hunk rows (a lockfile, a binary file, no changed lines):
    /// the cursor stops on it.
    pub(crate) fn is_entry(&self) -> bool {
        matches!(&self.what, What::File(summary) if summary.body != Body::Hunks)
    }

    /// Whether the row is a change, counted in the totals: a hunk, or a file without hunks.
    pub(crate) fn is_change(&self) -> bool {
        !self.is_divider() || self.is_entry()
    }
}

/// The text of a version of a file (`None` when it doesn't exist), or `Err` when it is binary.
pub(crate) fn text(bytes: Option<&[u8]>) -> Result<Option<&str>, ()> {
    match bytes {
        None => Ok(None),
        Some(bytes) if bytes.contains(&0) => Err(()),
        Some(bytes) => std::str::from_utf8(bytes).map(Some).map_err(|_| ()),
    }
}

/// The lines of `text` without their line endings.
pub(crate) fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n')
        .map(|line| line.trim_end_matches('\n').trim_end_matches('\r'))
        .collect()
}

/// The preview of `hunk` of the diff from `before` to `after` (their [`lines`]).
pub(crate) fn preview(before: &[&str], after: &[&str], hunk: &LineHunk) -> HunkPreview {
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

/// The hunks of `change` from `before` to `after`, each with the line hunk it is made of.
pub(crate) fn hunks(change: &FileChange, before: &str, after: &str) -> Vec<(LineHunk, Hunk)> {
    let (before_lines, after_lines) = (lines(before), lines(after));
    staging::line_hunks(before, after)
        .into_iter()
        .map(|line_hunk| {
            let hunk = Hunk {
                lines: line_hunk.after.clone(),
                added: line_hunk.after.len() as u32,
                removed: line_hunk.before.len() as u32,
                edit: Edit::of(change, &line_hunk),
                preview: Arc::new(preview(&before_lines, &after_lines, &line_hunk)),
            };
            (line_hunk, hunk)
        })
        .collect()
}

/// The rows of a file: its divider, then its hunks (with their marks, in the uncommitted
/// changes). `marks` are those of the whole file.
pub(crate) fn file_group(
    change: &FileChange,
    marks: Option<Marks>,
    body: Body,
    hunks: Vec<(Hunk, Option<Marks>)>,
) -> Vec<ChangeRow> {
    let count = |mark: fn(&Marks) -> bool| {
        hunks
            .iter()
            .filter(|(_, marks)| marks.as_ref().is_some_and(mark))
            .count() as u32
    };
    let (added, removed) = match body {
        Body::Lockfile(LockfileLines::Counts { added, removed }) => (added, removed),
        _ => hunks.iter().fold((0, 0), |(added, removed), (hunk, _)| {
            (added + hunk.added, removed + hunk.removed)
        }),
    };
    let summary = FileSummary {
        body,
        hunks: hunks.len() as u32,
        added,
        removed,
        staged: count(|marks| marks.staged),
        unsaved: count(|marks| marks.unsaved),
        first: hunks
            .first()
            .map(|(hunk, _)| (hunk.lines.start, hunk.preview.clone())),
    };
    let mut rows = vec![ChangeRow {
        change: change.clone(),
        marks,
        what: What::File(Arc::new(summary)),
    }];
    rows.extend(hunks.into_iter().map(|(hunk, marks)| ChangeRow {
        change: change.clone(),
        marks,
        what: What::Hunk(hunk),
    }));
    rows
}

/// The rows of a file changed by commits, from its version `before` to `after` (`None` where
/// it doesn't exist).
pub(crate) fn commit_file_rows(
    change: &FileChange,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    lockfile: bool,
) -> Vec<ChangeRow> {
    if lockfile {
        let counts = line_counts(before.unwrap_or_default(), after.unwrap_or_default());
        return file_group(change, None, Body::Lockfile(counts), Vec::new());
    }
    let (Ok(before), Ok(after)) = (text(before), text(after)) else {
        return file_group(change, None, Body::Binary, Vec::new());
    };
    let hunks: Vec<_> = hunks(
        change,
        before.unwrap_or_default(),
        after.unwrap_or_default(),
    )
    .into_iter()
    .map(|(_, hunk)| (hunk, None))
    .collect();
    let body = if hunks.is_empty() {
        Body::Unchanged
    } else {
        Body::Hunks
    };
    file_group(change, None, body, hunks)
}

/// The rows of a file changed by commits, from its two versions (`None` where it doesn't
/// exist): a lockfile as `config` says.
pub(crate) fn commit_group(
    change: &FileChange,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    config: &ChangesConfig,
) -> Vec<ChangeRow> {
    commit_file_rows(change, before, after, config.is_lockfile(change.path()))
}

/// The rows of the files, sorted by path with the lockfiles last. Each group starts with the
/// divider of its file.
pub(crate) fn sort_groups(mut groups: Vec<Vec<ChangeRow>>) -> Vec<ChangeRow> {
    groups.retain(|group| !group.is_empty());
    groups.sort_by(|a, b| {
        let key = |group: &[ChangeRow]| (group[0].is_lockfile(), group[0].change.path().to_owned());
        key(a).cmp(&key(b))
    });
    groups.into_iter().flatten().collect()
}

/// The lines added and removed between `before` and `after`, as the diff view counts them.
pub(crate) fn line_counts(before: &[u8], after: &[u8]) -> LockfileLines {
    // `bun.lockb`, or a lockfile that git stores as binary
    if before.contains(&0) || after.contains(&0) {
        return LockfileLines::Binary;
    }
    let (before, after) = (
        String::from_utf8_lossy(before),
        String::from_utf8_lossy(after),
    );
    let input = InternedInput::new(before.as_ref(), after.as_ref());
    let diff = Diff::compute(Algorithm::Histogram, &input);
    LockfileLines::Counts {
        added: diff.count_additions(),
        removed: diff.count_removals(),
    }
}

/// The files of a list for `n`/`N` in their diffs: each with whether they skip it (lockfiles).
pub(crate) type NavFiles = Vec<(FileChange, bool)>;

/// The files of `rows` for `n`/`N`, in the order of the list.
pub(crate) fn nav_files(rows: &[ChangeRow]) -> NavFiles {
    rows.iter()
        .filter(|row| row.is_divider())
        .map(|row| (row.change.clone(), row.is_lockfile()))
        .collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// `+24 -7`, without the zero counts: empty when both are.
pub(crate) fn line_counts_text(added: u32, removed: u32) -> String {
    let mut counts = Vec::new();
    if added > 0 {
        counts.push(format!("+{added}"));
    }
    if removed > 0 {
        counts.push(format!("-{removed}"));
    }
    counts.join(" ")
}

/// How much a list has.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    /// Hunks, and files without hunks.
    pub(crate) changes: usize,
    pub(crate) files: usize,
    pub(crate) added: u32,
    pub(crate) removed: u32,
    /// Changes with unsaved edits.
    pub(crate) unsaved: usize,
}

impl Totals {
    pub(crate) fn of(rows: &[ChangeRow]) -> Totals {
        let mut totals = Totals::default();
        for row in rows {
            if let What::File(summary) = &row.what {
                totals.files += 1;
                totals.added += summary.added;
                totals.removed += summary.removed;
            }
            if row.is_change() {
                totals.changes += 1;
                totals.unsaved += row.marks.is_some_and(|marks| marks.unsaved) as usize;
            }
        }
        totals
    }

    /// `12 changes in 4 files, +80 -31`.
    pub(crate) fn describe(&self) -> String {
        let mut text = format!(
            "{} in {}",
            plural(self.changes, "change", "changes"),
            plural(self.files, "file", "files")
        );
        let counts = line_counts_text(self.added, self.removed);
        if !counts.is_empty() {
            text += &format!(", {counts}");
        }
        text
    }
}

/// The styles of the rows and of the preview, and the directory paths are shown from.
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
    pub(crate) fn new(editor: &Editor, cwd: PathBuf) -> Self {
        let theme = &editor.theme;
        // the colors of the signs of the diff view
        let sign = |scope: &str| match theme.get(scope).fg {
            Some(color) => Style::default().fg(color),
            None => Style::default(),
        };
        Self {
            cwd,
            staged: theme.get("diff.plus"),
            unstaged: theme.get("diff.delta"),
            unsaved: theme.get("warning"),
            quiet: theme
                .try_get("ui.text.inactive")
                .unwrap_or_else(|| theme.get("comment")),
            added: sign("diff.plus"),
            removed: sign("diff.minus"),
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

    /// The signs of a hunk, `-` in the color of `diff.minus` and `+` in the color of
    /// `diff.plus`.
    fn signs(&self, edit: Edit) -> Spans<'static> {
        Spans::from(
            edit.signs()
                .chars()
                .map(|sign| {
                    let style = if sign == '+' {
                        self.added
                    } else {
                        self.removed
                    };
                    Span::styled(sign.to_string(), style)
                })
                .collect::<Vec<_>>(),
        )
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.cwd)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// `src/main.rs:10-12` for a hunk, `src/main.rs` for a file.
    pub(crate) fn location(&self, row: &ChangeRow) -> String {
        let path = self.relative(row.change.path());
        match &row.what {
            What::Hunk(hunk) => place(&path, &hunk.lines),
            What::File(_) => path,
        }
    }

    /// The title of the divider of a file:
    /// `src/main.rs  3 hunks  +24 -7  1 staged`, `src/old.rs → src/new.rs  1 hunk  +3`,
    /// `Cargo.lock  +340 -12`, `logo.png (binary)`.
    pub(crate) fn divider(&self, row: &ChangeRow) -> String {
        let What::File(summary) = &row.what else {
            return self.location(row);
        };
        let mut title = match &row.change {
            FileChange::Renamed { from_path, to_path } => {
                format!("{} → {}", self.relative(from_path), self.relative(to_path))
            }
            change => self.relative(change.path()),
        };
        let created_or_deleted = match &row.change {
            FileChange::Untracked { .. } => Some("untracked"),
            FileChange::Added { .. } => Some("new"),
            FileChange::Deleted { .. } => Some("deleted"),
            _ => None,
        };
        let mut notes: Vec<&str> = created_or_deleted.into_iter().collect();
        if let FileChange::Conflict { .. } = row.change {
            notes.push("conflict");
        }
        match summary.body {
            Body::Binary | Body::Lockfile(LockfileLines::Binary) => notes.push("binary"),
            Body::Unchanged if created_or_deleted.is_some() => notes.push("empty"),
            Body::Unchanged => notes.push("no lines changed"),
            Body::Hunks | Body::Lockfile(LockfileLines::Counts { .. }) => {}
        }
        if !notes.is_empty() {
            title += &format!(" ({})", notes.join(", "));
        }

        let mut parts = Vec::new();
        if summary.body == Body::Hunks {
            parts.push(plural(summary.hunks as usize, "hunk", "hunks"));
        }
        let counts = line_counts_text(summary.added, summary.removed);
        if !counts.is_empty() {
            parts.push(counts);
        }
        if let Some(marks) = row.marks {
            if summary.body == Body::Hunks {
                if summary.staged > 0 {
                    parts.push(format!("{} staged", summary.staged));
                }
                if summary.unsaved > 0 {
                    parts.push(format!("{} unsaved", summary.unsaved));
                }
            } else {
                // the file is the change
                if marks.staged {
                    parts.push("staged".into());
                }
                if marks.unsaved {
                    parts.push("unsaved".into());
                }
            }
        }
        for part in parts {
            title += "  ";
            title += &part;
        }
        title
    }
}

/// `src/main.rs:10-12`: where the lines of a hunk (from 0) are in `path`; for removed lines,
/// the line before them.
pub(crate) fn place(path: &str, lines: &Range<u32>) -> String {
    match lines.len() {
        0 => format!("{path}:{}", lines.start.max(1)),
        1 => format!("{path}:{}", lines.start + 1),
        _ => format!("{path}:{}-{}", lines.start + 1, lines.end),
    }
}

/// The styles of a preview of hunks.
pub(crate) struct PreviewStyles {
    pub(crate) text: Style,
    pub(crate) added: Style,
    pub(crate) removed: Style,
    pub(crate) quiet: Style,
}

impl PreviewStyles {
    pub(crate) fn new(editor: &Editor) -> Self {
        let data = RowData::new(editor, PathBuf::new());
        Self {
            text: editor.theme.get("ui.text"),
            added: data.added,
            removed: data.removed,
            quiet: data.quiet,
        }
    }
}

/// Draws `title`, an empty line and the lines of `preview` (when there is one) from line `*y`
/// of `area` down, moving `*y` past them.
pub(crate) fn draw_hunk(
    surface: &mut Surface,
    area: Rect,
    y: &mut u16,
    title: &str,
    preview: Option<&HunkPreview>,
    styles: &PreviewStyles,
) {
    let width = area.width as usize;
    let mut line = |surface: &mut Surface, text: &str, style: Style| {
        if *y < area.bottom() {
            surface.set_stringn(area.x, *y, text, width, style);
            *y += 1;
        }
    };
    let text = styles.text;
    line(surface, title, text);
    line(surface, "", text);
    for (kind, content) in preview.iter().flat_map(|preview| &preview.lines) {
        let (prefix, style) = match kind {
            PreviewLine::Context => (' ', text),
            PreviewLine::Removed => ('-', text.patch(styles.removed)),
            PreviewLine::Added => ('+', text.patch(styles.added)),
        };
        let content = content.replace('\t', "    ");
        line(surface, &format!("{prefix} {content}"), style);
    }
}

/// Draws the preview of `row`: where it is, then its hunk with a few lines of context. A
/// divider shows the first hunk of its file.
pub(crate) fn draw_preview(
    row: &ChangeRow,
    data: &RowData,
    editor: &Editor,
    area: Rect,
    surface: &mut Surface,
) {
    let styles = PreviewStyles {
        text: editor.theme.get("ui.text"),
        added: data.added,
        removed: data.removed,
        quiet: data.quiet,
    };
    let mut y = area.y;
    let (title, preview) = match &row.what {
        What::Hunk(hunk) => {
            let mut title = data.location(row);
            let counts = line_counts_text(hunk.added, hunk.removed);
            if !counts.is_empty() {
                title += &format!(" · {counts}");
            }
            if let Some(marks) = row.marks {
                title += &format!(" · {}", marks.describe());
            }
            (title, Some(&hunk.preview))
        }
        What::File(summary) => (
            data.divider(row),
            summary.first.as_ref().map(|(_, preview)| preview),
        ),
    };
    draw_hunk(
        surface,
        area,
        &mut y,
        &title,
        preview.map(Arc::as_ref),
        &styles,
    );
    if preview.is_some() {
        return;
    }
    let What::File(summary) = &row.what else {
        return;
    };
    let note = match summary.body {
        Body::Lockfile(_) => "A lockfile: Enter shows its diff.",
        Body::Binary => "A binary file.",
        Body::Unchanged | Body::Hunks => "No lines changed.",
    };
    if y < area.bottom() {
        surface.set_stringn(area.x, y, note, area.width as usize, data.quiet);
    }
}

/// Puts the cursor of `picker` on the hunk of `path` at `line`, or else on the first change of
/// `path` (its first hunk, or the file when it has none). Returns whether it found one.
pub(crate) fn select_row(
    picker: &mut Picker<ChangeRow, RowData>,
    path: &Path,
    line: Option<u32>,
) -> bool {
    // the matched rows are ready (unless there are many)
    let mut first = None;
    let mut i = 0;
    while let Some(row) = picker.matched_item(i) {
        if row.change.path() == path && row.is_change() {
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

/// Puts the cursor of the topmost list of changes of a commit or range on the first change of
/// `change`.
pub(crate) fn select_in_commit_files(compositor: &mut Compositor, change: &FileChange) {
    if let Some(overlay) = compositor.find::<Overlay<Picker<ChangeRow, RowData>>>() {
        select_row(&mut overlay.content, change.path(), None);
    }
}

/// A list of changes: a modal picker with a divider per file, `w`/`b` between the files.
/// `with_marks` adds the column of the [`Marks`] (uncommitted changes). Enter and `C-g` run
/// `enter` on the row, keeping the list open below what it opens; `C-s`, `C-v` and the like run
/// `open` with their action.
pub(crate) fn picker(
    editor: &Editor,
    cwd: PathBuf,
    rows: Vec<ChangeRow>,
    with_marks: bool,
    enter: impl Fn(&mut Context, &ChangeRow) + Clone + 'static,
    open: impl Fn(&mut Context, &ChangeRow, Action) + 'static,
) -> Picker<ChangeRow, RowData> {
    let mut columns = Vec::new();
    if with_marks {
        columns.push(PickerColumn::new(
            "marks",
            |row: &ChangeRow, data: &RowData| match row.marks {
                Some(marks) if row.is_change() => data.marks(marks).into(),
                _ => "".into(),
            },
        ));
    }
    columns.push(PickerColumn::new(
        "edit",
        |row: &ChangeRow, data: &RowData| match &row.what {
            What::Hunk(hunk) => data.signs(hunk.edit).into(),
            What::File(_) => "".into(),
        },
    ));
    columns.push(PickerColumn::new(
        "change",
        |row: &ChangeRow, data: &RowData| {
            if row.is_lockfile() {
                Span::styled(data.location(row), data.quiet).into()
            } else {
                data.location(row).into()
            }
        },
    ));
    let primary = columns.len() - 1;
    let data = RowData::new(editor, cwd);
    Picker::new(columns, primary, rows, data, open)
        .with_dividers(|row: &ChangeRow, data: &RowData| {
            row.is_divider().then(|| data.divider(row).into())
        })
        .with_groups(ChangeRow::is_entry)
        .with_section_keys()
        .with_drawn_preview(draw_preview)
        .with_modes()
        .without_header()
        .with_help(
            "Changes",
            vec![(
                "changes",
                vec![
                    ("Enter C-g", "the diff, at the hunk"),
                    ("C-s C-v", "open the file in a split"),
                ],
            )],
        )
        .with_stacked_key_handler(key!(Enter), enter.clone())
        .with_stacked_key_handler(ctrl!('g'), enter)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn modified(path: &str) -> FileChange {
        FileChange::Modified {
            path: PathBuf::from(path),
        }
    }

    pub(crate) fn data() -> RowData {
        RowData {
            cwd: PathBuf::from("/r"),
            staged: Style::default(),
            unstaged: Style::default(),
            unsaved: Style::default(),
            quiet: Style::default(),
            added: Style::default(),
            removed: Style::default(),
        }
    }

    /// Each row: a divider's title after `── `, a hunk's signs and location.
    pub(crate) fn listed(rows: &[ChangeRow]) -> Vec<String> {
        let data = data();
        rows.iter()
            .map(|row| match &row.what {
                What::File(_) => format!("── {}", data.divider(row)),
                What::Hunk(hunk) => format!("{:2} {}", hunk.edit.signs(), data.location(row)),
            })
            .collect()
    }

    #[test]
    fn the_signs_of_the_hunks() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        // b changed, an x added after e, i removed
        let after = "a\nB\nc\nd\ne\nx\nf\ng\nh\nj\n";
        let rows = commit_file_rows(
            &modified("/r/x.txt"),
            Some(before.as_bytes()),
            Some(after.as_bytes()),
            false,
        );
        assert_eq!(
            listed(&rows),
            [
                "── x.txt  3 hunks  +2 -2",
                "-+ x.txt:2",
                "+  x.txt:6",
                "-  x.txt:9",
            ]
        );

        let new = FileChange::Added {
            path: "/r/new.rs".into(),
        };
        let rows = commit_file_rows(&new, None, Some(b"one\ntwo\n"), false);
        assert_eq!(
            listed(&rows),
            ["── new.rs (new)  1 hunk  +2", "++ new.rs:1-2"]
        );

        let deleted = FileChange::Deleted {
            path: "/r/old.rs".into(),
        };
        let rows = commit_file_rows(&deleted, Some(b"one\ntwo\nthree\n"), None, false);
        assert_eq!(
            listed(&rows),
            ["── old.rs (deleted)  1 hunk  -3", "-- old.rs:1"]
        );
    }

    #[test]
    fn files_without_hunk_rows() {
        let rows = commit_file_rows(&modified("/r/a.png"), Some(b"\0a"), Some(b"\0b"), false);
        assert_eq!(listed(&rows), ["── a.png (binary)"]);
        assert!(rows[0].is_entry());

        let rows = commit_file_rows(
            &modified("/r/Cargo.lock"),
            Some(b"a\nb\nc\n"),
            Some(b"a\nB\nc\nd\n"),
            true,
        );
        assert_eq!(listed(&rows), ["── Cargo.lock  +2 -1"]);
        assert!(rows[0].is_entry() && rows[0].is_lockfile());

        let empty = FileChange::Added {
            path: "/r/empty".into(),
        };
        let rows = commit_file_rows(&empty, None, Some(b""), false);
        assert_eq!(listed(&rows), ["── empty (new, empty)"]);

        let renamed = FileChange::Renamed {
            from_path: "/r/a.rs".into(),
            to_path: "/r/b.rs".into(),
        };
        let rows = commit_file_rows(&renamed, Some(b"x\n"), Some(b"x\n"), false);
        assert_eq!(listed(&rows), ["── a.rs → b.rs (no lines changed)"]);
        let rows = commit_file_rows(&renamed, Some(b"x\n"), Some(b"y\n"), false);
        assert_eq!(
            listed(&rows),
            ["── a.rs → b.rs  1 hunk  +1 -1", "-+ b.rs:1"]
        );
    }

    /// The rows of the changes of commits: `contents` gives the two versions of a file.
    fn commit_rows(
        changes: Vec<FileChange>,
        config: &ChangesConfig,
        contents: impl Fn(&FileChange) -> (Option<Vec<u8>>, Option<Vec<u8>>),
    ) -> Vec<ChangeRow> {
        let groups = changes
            .iter()
            .map(|change| {
                let (before, after) = contents(change);
                commit_group(change, before.as_deref(), after.as_deref(), config)
            })
            .collect();
        sort_groups(groups)
    }

    #[test]
    fn files_are_sorted_with_lockfiles_last() {
        let config = ChangesConfig::default();
        let changes = vec![
            modified("/r/Cargo.lock"),
            modified("/r/src/main.rs"),
            modified("/r/README.md"),
        ];
        let rows = commit_rows(changes, &config, |change| {
            match change.path().file_name().unwrap().to_str().unwrap() {
                "Cargo.lock" => (Some(b"a\n".to_vec()), Some(b"a\nb\n".to_vec())),
                _ => (Some(b"a\n".to_vec()), Some(b"b\n".to_vec())),
            }
        });
        assert_eq!(
            listed(&rows),
            [
                "── README.md  1 hunk  +1 -1",
                "-+ README.md:1",
                "── src/main.rs  1 hunk  +1 -1",
                "-+ src/main.rs:1",
                "── Cargo.lock  +1",
            ]
        );
        assert_eq!(
            nav_files(&rows)
                .iter()
                .map(|(change, skip)| (change.path().to_str().unwrap(), *skip))
                .collect::<Vec<_>>(),
            [
                ("/r/README.md", false),
                ("/r/src/main.rs", false),
                ("/r/Cargo.lock", true)
            ]
        );
        let totals = Totals::of(&rows);
        assert_eq!(totals.describe(), "3 changes in 3 files, +3 -2");
    }

    #[test]
    fn binary_lockfiles() {
        assert_eq!(line_counts(b"\0\x01", b"\0\x02"), LockfileLines::Binary);
        let rows = commit_file_rows(&modified("/r/bun.lockb"), Some(b"\0a"), Some(b"\0b"), true);
        assert_eq!(listed(&rows), ["── bun.lockb (binary)"]);
    }

    #[test]
    fn configured_lockfiles() {
        let config = ChangesConfig {
            lockfiles: vec!["deps.lock".into()],
        };
        let rows = commit_rows(
            vec![modified("/r/Cargo.lock"), modified("/r/deps.lock")],
            &config,
            |_| (Some(b"a\n".to_vec()), Some(b"b\n".to_vec())),
        );
        assert_eq!(
            listed(&rows),
            [
                "── Cargo.lock  1 hunk  +1 -1",
                "-+ Cargo.lock:1",
                "── deps.lock  +1 -1"
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
