use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use vortex_vcs::DiffProviderRegistry;

use tui::{
    buffer::Buffer as Surface,
    text::{Span, Spans},
};
use vortex_core::{movement::Direction, Position};
use vortex_event::status;
use vortex_vcs::{CommitInfo, LogEntry};
use vortex_view::{
    editor::ChangesConfig,
    graphics::{CursorKind, Rect},
    Editor,
};

use super::line_history::{LineHistory, LineRow};
use super::{changes, diff_view, uncommitted};
use crate::compositor::{self, Component, Compositor, Event, EventResult};
use crate::job::{self, Callback};
use crate::live::{Live, Pending, RepoWatch};
use crate::ui::{
    overlay::{overlaid, Overlay},
    picker::PickerMode,
    Picker, PickerColumn,
};
use crate::{alt, key, shift};

pub(crate) fn trust_full(editor: &Editor, cwd: &Path) -> bool {
    editor
        .workspace_trust
        .query(
            &vortex_loader::find_workspace_in(cwd).0,
            vortex_loader::workspace_trust::TrustQuery::Git,
        )
        .is_trusted()
}

/// Lists the uncommitted changes, then the commits reachable from HEAD in sections by branch, see
/// `FORK.md`. Enter lists the files changed by the selected commit, or by the selected range of
/// commits, or the uncommitted files. While it is open, it follows the working tree: the number of
/// uncommitted files, and the log when HEAD moves.
pub(crate) fn commit_picker(editor: &mut Editor) -> Option<CommitLog> {
    let cwd = vortex_stdx::env::current_working_dir();
    if !cwd.exists() {
        editor.set_error("Current working directory does not exist");
        return None;
    }

    let uncommitted = Arc::new(UncommittedCount::default());
    let watch = RepoWatch::start(editor, &cwd);
    let requests = Requests::default();
    let open = {
        let cwd = cwd.clone();
        let watch = watch.clone();
        let requests = requests.clone();
        move |cx: &mut compositor::Context, row: &LogRow| match &row.kind {
            RowKind::Uncommitted => {
                // a commit still being read doesn't open over them
                requests.start();
                uncommitted::open(cx, cwd.clone(), watch.clone())
            }
            RowKind::Commit(commit) => {
                open_commit_files(cx, cwd.clone(), CommitRange::single(commit), &requests)
            }
            RowKind::Divider(_) => {}
        }
    };
    let trust_full = trust_full(editor, &cwd);
    let source = Source {
        cwd: cwd.clone(),
        providers: editor.diff_providers.clone(),
        trust_full,
    };
    let mut log = CommitLog::new(
        editor,
        source,
        uncommitted,
        requests,
        open,
        vec![LogRow::uncommitted(None)],
        None,
    );
    let injector = log.picker.injector();
    // the uncommitted changes are the first row
    let pos = AtomicUsize::new(1);
    editor
        .diff_providers
        .clone()
        .for_each_log_entry(cwd.clone(), trust_full, move |entry| match entry {
            Ok(entry) => {
                let pos = pos.fetch_add(1, Ordering::Relaxed);
                injector.push(LogRow::new(pos, entry)).is_ok()
            }
            Err(err) => {
                status::report_blocking(err);
                false
            }
        });
    log.work_dir = uncommitted::work_dir(editor, &cwd, trust_full);
    log.live = watch.map(Live::new);
    // the number of uncommitted files, and the HEAD the log is read from
    let source = log.source.clone();
    log.refresh.run(move || source.read(None));
    Some(log)
}

/// The number of uncommitted files the commit log shows, and of modified buffers, updated while
/// it is open.
#[derive(Default)]
pub(crate) struct UncommittedCount {
    /// The number of files, plus one; 0 while it isn't known yet.
    files: AtomicUsize,
    /// The number of modified buffers of the working tree.
    unsaved: AtomicUsize,
}

impl UncommittedCount {
    fn set(&self, files: usize) {
        self.files.store(files + 1, Ordering::Relaxed);
    }

    fn set_unsaved(&self, buffers: usize) {
        self.unsaved.store(buffers, Ordering::Relaxed);
    }

    /// `Uncommitted changes (3 files, 1 unsaved)`.
    fn label(&self) -> String {
        let files = self.files.load(Ordering::Relaxed);
        let unsaved = self.unsaved.load(Ordering::Relaxed);
        let mut counts = Vec::new();
        match files {
            0 | 1 => {}
            2 => counts.push("1 file".to_string()),
            files => counts.push(format!("{} files", files - 1)),
        }
        if unsaved > 0 {
            counts.push(format!("{unsaved} unsaved"));
        }
        match (files, counts.is_empty()) {
            (1, true) => "Uncommitted changes (none)".to_string(),
            (_, true) => "Uncommitted changes".to_string(),
            (_, false) => format!("Uncommitted changes ({})", counts.join(", ")),
        }
    }
}

/// What the commit log reads off the main thread.
#[derive(Clone)]
pub(crate) struct Source {
    pub(crate) cwd: PathBuf,
    pub(crate) providers: DiffProviderRegistry,
    pub(crate) trust_full: bool,
}

/// What the commit log learns when the working tree changes.
struct Refresh {
    /// The number of uncommitted files.
    files: Option<usize>,
    /// The commit HEAD points to.
    head: Option<String>,
    /// The log again, when HEAD moved since it was read.
    log: Option<Vec<LogEntry>>,
}

impl Source {
    /// Reads the number of uncommitted files and HEAD, and the log too if HEAD isn't `known`
    /// (`None`: not known yet, the log is being read).
    fn read(&self, known: Option<Option<String>>) -> Refresh {
        let files = self
            .providers
            .changed_files(&self.cwd, self.trust_full)
            .map_err(|err| log::debug!("uncommitted changes: {err}"))
            .ok()
            .map(|changes| changes.len());
        let head = self
            .providers
            .repo_state(&self.cwd, self.trust_full)
            .ok()
            .and_then(|state| state.head);
        let log = known
            .is_some_and(|known| known != head)
            .then(|| self.providers.log_entries(&self.cwd, self.trust_full).ok())
            .flatten();
        Refresh { files, head, log }
    }
}

/// A row of the commit picker: the uncommitted changes, a commit, or the divider of a section of
/// the log.
/// Whether a query looks like the start of a commit hash: 4 hexadecimal digits or more. The commits
/// whose hash starts with it are listed first, before the ones whose summary matches it.
fn is_hash_query(query: &str) -> bool {
    query.len() >= 4 && query.chars().all(|c| c.is_ascii_hexdigit())
}

pub(crate) struct LogRow {
    /// The position among the rows, dividers included; the uncommitted changes are at 0.
    pos: usize,
    kind: RowKind,
    /// In the line history: the lines followed, in the commit or in the buffer for the
    /// uncommitted changes.
    line: Option<Arc<LineRow>>,
}

pub(crate) enum RowKind {
    /// The changes of the working tree since HEAD: always the first row.
    Uncommitted,
    /// The label of the section that follows.
    Divider(String),
    Commit(CommitInfo),
}

impl LogRow {
    fn new(pos: usize, entry: LogEntry) -> Self {
        let kind = match entry {
            LogEntry::Divider(title) => RowKind::Divider(title),
            LogEntry::Commit(commit) => RowKind::Commit(commit),
        };
        Self {
            pos,
            kind,
            line: None,
        }
    }

    /// The uncommitted changes, the first row.
    pub(crate) fn uncommitted(line: Option<LineRow>) -> Self {
        Self {
            pos: 0,
            kind: RowKind::Uncommitted,
            line: line.map(Arc::new),
        }
    }

    /// The divider of a section of the line history.
    pub(crate) fn divider(pos: usize, label: String) -> Self {
        Self {
            pos,
            kind: RowKind::Divider(label),
            line: None,
        }
    }

    /// A commit of the line history.
    pub(crate) fn line_commit(pos: usize, commit: CommitInfo, line: LineRow) -> Self {
        Self {
            pos,
            kind: RowKind::Commit(commit),
            line: Some(Arc::new(line)),
        }
    }

    pub(crate) fn kind(&self) -> &RowKind {
        &self.kind
    }

    pub(crate) fn line(&self) -> Option<&LineRow> {
        self.line.as_deref()
    }

    /// Whether the row is a commit whose id starts with `prefix` (a hash query, see
    /// [`is_hash_query`]).
    fn hash_starts_with(&self, prefix: &str) -> bool {
        self.commit().is_some_and(|commit| {
            commit.id.len() >= prefix.len()
                && commit.id[..prefix.len()].eq_ignore_ascii_case(prefix)
        })
    }

    fn commit(&self) -> Option<&CommitInfo> {
        match &self.kind {
            RowKind::Commit(commit) => Some(commit),
            _ => None,
        }
    }

    /// The cell for `text` of the commit, or an empty one for a divider (dividers match no query)
    /// and for the uncommitted changes.
    fn text<'a>(
        &'a self,
        text: impl FnOnce(&'a CommitInfo) -> tui::widgets::Cell<'a>,
    ) -> tui::widgets::Cell<'a> {
        self.commit().map(text).unwrap_or_default()
    }
}

/// The commits whose combined change is listed: the difference between the first parent of
/// `oldest` and `newest`.
#[derive(Clone)]
pub(crate) struct CommitRange {
    pub(crate) oldest: CommitInfo,
    pub(crate) newest: CommitInfo,
    /// The number of commits in the log between them, both included.
    pub(crate) count: usize,
}

impl CommitRange {
    pub(crate) fn single(commit: &CommitInfo) -> Self {
        Self {
            oldest: commit.clone(),
            newest: commit.clone(),
            count: 1,
        }
    }

    /// `a1b2c3d..d4e5f6a (5 commits)`, or `a1b2c3d` for one commit.
    pub(crate) fn label(&self) -> String {
        match self.count {
            1 => self.newest.short_id.clone(),
            count => format!(
                "{}..{} ({count} commits)",
                self.oldest.short_id, self.newest.short_id
            ),
        }
    }
}

/// The range of commits selected in the commit picker. The log lists newest commits first, so
/// `newest` is before `oldest` in it.
#[derive(Default)]
struct Selection {
    /// The commit the range is extended from.
    anchor: Option<usize>,
    /// The commit the cursor was on when the selection was last updated.
    cursor: Option<usize>,
    /// The positions of the newest and the oldest selected commits.
    range: Option<(usize, usize)>,
    /// Whether movements extend the range.
    extend: bool,
}

/// The commit picker, with a selection of several commits. The picker has modes
/// ([`Picker::with_modes`]): in normal mode, the one it opens in, `v` (or `A-v`, in both modes)
/// toggles extend mode, in which movements extend the selection; `w`/`e` and `b` extend it to the
/// end and the start of the section of the log the cursor is in; `S-Down`/`S-Up` extend it by one
/// commit; `A-s` selects the section; and `Esc` collapses it. `Enter` lists the files changed by
/// the whole selection.
pub(crate) struct CommitLog {
    picker: Picker<LogRow, CommitStyle>,
    source: Source,
    /// The selected range of more than one commit, for the picker to mark the rows of.
    marked: Rc<Cell<Option<(usize, usize)>>>,
    selection: Selection,
    /// What is being loaded (the changes of a range), shown on the prompt row.
    loading: Option<String>,
    /// Sees the working tree change while the log is open.
    live: Option<Live>,
    refresh: Pending<Refresh>,
    uncommitted: Arc<UncommittedCount>,
    /// The root of the working tree, whose modified buffers are counted.
    work_dir: PathBuf,
    /// The commit HEAD pointed to when the log was read, once known.
    head: Option<Option<String>>,
    /// The lists of changes asked for: only the latest opens.
    requests: Requests,
    /// The line history (`Space L`): what it follows and its search.
    lines: Option<LineHistory>,
}

impl CommitLog {
    /// A commit log with `first_rows`, where `open` is what Enter does on a row (on a range of
    /// commits, see [`Self::open_range`]). With `lines`, the line history: it has a preview, and
    /// its uncommitted changes are those of the lines.
    pub(crate) fn new(
        editor: &Editor,
        source: Source,
        uncommitted: Arc<UncommittedCount>,
        requests: Requests,
        open: impl Fn(&mut compositor::Context, &LogRow) + Clone + 'static,
        first_rows: Vec<LogRow>,
        lines: Option<LineHistory>,
    ) -> Self {
        let columns = [
            PickerColumn::new("commit", |row: &LogRow, style: &CommitStyle| {
                row.text(|commit| Span::styled(commit.short_id.clone(), style.id).into())
            }),
            PickerColumn::new("date", |row: &LogRow, style: &CommitStyle| {
                row.text(|commit| Span::styled(commit.date.clone(), style.date).into())
            }),
            PickerColumn::new("author", |row: &LogRow, style: &CommitStyle| {
                row.text(|commit| Span::styled(commit.author.clone(), style.author).into())
            }),
            PickerColumn::new("summary", |row: &LogRow, style: &CommitStyle| {
                match &row.kind {
                    RowKind::Uncommitted => {
                        let label = match &style.uncommitted_label {
                            Some(label) => label.clone(),
                            None => style.uncommitted.label(),
                        };
                        Span::styled(label, style.uncommitted_style).into()
                    }
                    _ => row.text(|commit| match row.line() {
                        // where the lines come from
                        Some(line) if line.added => Spans::from(vec![
                            Span::raw(commit.summary.as_str()),
                            Span::styled(" (added)", style.added),
                        ])
                        .into(),
                        _ => commit.summary.as_str().into(),
                    }),
                }
            }),
        ];
        let style = CommitStyle {
            // the hash is there to be found and copied, not read: subtle
            id: editor
                .theme
                .try_get_exact("ui.text.inactive")
                .unwrap_or_else(|| editor.theme.get("comment")),
            date: editor.theme.get("comment"),
            author: editor.theme.get("variable"),
            uncommitted_style: editor.theme.get("diff.delta"),
            uncommitted: uncommitted.clone(),
            uncommitted_label: lines.as_ref().map(|_| "Uncommitted changes".to_string()),
            added: editor.theme.get("diff.plus"),
            preview: changes::PreviewStyles::new(editor),
        };
        let uncommitted_count = uncommitted;
        let marked = Rc::new(Cell::new(None));
        let picker = Picker::new(columns, 3, first_rows, style, {
            let open = open.clone();
            move |cx, row, _action| open(cx, row)
        })
        .with_stacked_key_handler(key!(Enter), open)
        .with_preferred(is_hash_query, LogRow::hash_starts_with)
        .with_modes()
        .without_header()
        .with_help(
            if lines.is_some() {
                "Line history"
            } else {
                "Commit log"
            },
            {
                let mut keys = vec![
                    ("Enter", "the changes of the commit or range"),
                    ("v A-v", "extend mode: movements extend the range"),
                    ("w e, b", "extend to the section's end, start"),
                    ("S-↓ S-↑", "extend by one commit"),
                    ("A-s", "the whole section"),
                    ("Esc", "collapse the range"),
                ];
                if lines.is_some() {
                    keys[0] = ("Enter", "the diff of the file at the lines");
                    keys.push(("o", "all the changes of the commit"));
                }
                vec![("commits", keys)]
            },
        )
        .with_dividers(|row, _| match &row.kind {
            RowKind::Divider(title) => Some(title.as_str().into()),
            _ => None,
        })
        .with_marked_rows({
            let marked = marked.clone();
            move |row| {
                marked
                    .get()
                    .is_some_and(|(newest, oldest): (usize, usize)| {
                        (newest..=oldest).contains(&row.pos)
                    })
            }
        });
        let picker = if lines.is_some() {
            picker.with_drawn_preview(super::line_history::draw_preview)
        } else {
            picker
        };
        let work_dir = source.cwd.clone();
        let mut log = CommitLog {
            picker,
            source,
            marked,
            selection: Selection::default(),
            loading: None,
            live: None,
            refresh: Pending::new(),
            uncommitted: uncommitted_count,
            work_dir,
            head: None,
            requests,
            lines,
        };
        log.update_info();
        log
    }

    pub(crate) fn injector(&self) -> crate::ui::picker::Injector<LogRow, CommitStyle> {
        self.picker.injector()
    }

    /// The rows of the newest and the oldest commits of the selection, if it has commits.
    fn range_rows(&self) -> Option<(&LogRow, &LogRow)> {
        let (first, last) = self.selection.range?;
        let mut commits = (first..=last)
            .filter_map(|pos| self.picker.item(pos as u32))
            .filter(|row| row.commit().is_some());
        let newest = commits.next()?;
        Some((newest, commits.next_back().unwrap_or(newest)))
    }

    fn commit_at(&self, pos: usize) -> Option<&CommitInfo> {
        self.picker.item(pos as u32)?.commit()
    }

    /// The position of the commit under the cursor.
    fn cursor_pos(&self) -> Option<usize> {
        self.picker.selection().map(|row| row.pos)
    }

    /// Updates the selection after the cursor moved: it's the commit under the cursor alone, or
    /// in extend mode the commits from the anchor to it.
    fn follow_cursor(&mut self) {
        let cursor = self.cursor_pos();
        if cursor == self.selection.cursor {
            return;
        }
        self.selection.cursor = cursor;
        let Some(cursor) = cursor else {
            return;
        };
        let anchor = match self.selection.anchor {
            Some(anchor) if self.selection.extend => anchor,
            _ => cursor,
        };
        self.select(anchor, cursor);
    }

    /// Selects the commits from `anchor` to `cursor` (in any order) in the log, filtered or not.
    fn select(&mut self, anchor: usize, cursor: usize) {
        self.selection.anchor = Some(anchor);
        let mut range = (anchor.min(cursor), anchor.max(cursor));
        // the uncommitted changes aren't a commit: a range of commits starts at the first one
        // after them (past the divider), so that it may be that commit alone
        if range.0 == 0 && range.1 > 0 {
            range.0 = (1..=range.1)
                .find(|&pos| self.commit_at(pos).is_some())
                .unwrap_or(range.1);
        }
        self.selection.range = Some(range);
        self.marked.set((range.0 != range.1).then_some(range));
    }

    /// The commits of the selection, both ends included, if it has any.
    fn range(&self) -> Option<CommitRange> {
        let (first, last) = self.selection.range?;
        // the ends may be dividers
        let mut commits = (first..=last).filter_map(|pos| self.commit_at(pos));
        let newest = commits.next()?;
        let (oldest, count) = commits.fold((newest, 1), |(_, count), commit| (commit, count + 1));
        Some(CommitRange {
            oldest: oldest.clone(),
            newest: newest.clone(),
            count,
        })
    }

    fn is_range(&self) -> bool {
        self.marked.get().is_some()
    }

    /// Shows on the prompt row what is being loaded, else the selection when it's more than the
    /// commit under the cursor, else (line history) what the history follows.
    fn update_info(&mut self) {
        let info = self
            .loading
            .clone()
            .or_else(|| {
                (self.is_range() || self.selection.extend)
                    .then(|| self.range())
                    .flatten()
                    .map(|range| {
                        let extend = if self.selection.extend {
                            "[extend] "
                        } else {
                            ""
                        };
                        format!("{extend}{}", range.label())
                    })
            })
            .or_else(|| self.lines.as_ref().map(LineHistory::status));
        self.picker.set_info(info);
    }

    /// Says on the prompt row that the changes of a range are being read (`None`: they are not).
    fn set_loading(&mut self, loading: Option<String>) {
        self.loading = loading;
        self.update_info();
    }

    /// Moves the cursor by one commit, extending the selection to it.
    fn extend_by(&mut self, direction: Direction) {
        self.follow_cursor();
        let before = self.picker.cursor_index();
        self.picker.move_by(1, direction);
        // no wrapping around: that would select the whole log
        let wrapped = match direction {
            Direction::Forward => self.picker.cursor_index() < before,
            Direction::Backward => self.picker.cursor_index() > before,
        };
        if wrapped {
            self.picker.set_cursor_index(before);
        }
        let Some(cursor) = self.cursor_pos() else {
            return;
        };
        self.selection.cursor = Some(cursor);
        let anchor = self.selection.anchor.unwrap_or(cursor);
        self.select(anchor, cursor);
    }

    /// Selects the commits of the section of the log under the cursor.
    fn select_section(&mut self) {
        let Some(cursor) = self.cursor_pos() else {
            return;
        };
        let is_commit = |pos: usize| self.commit_at(pos).is_some();
        let mut newest = cursor;
        while newest > 0 && is_commit(newest - 1) {
            newest -= 1;
        }
        let mut oldest = cursor;
        while is_commit(oldest + 1) {
            oldest += 1;
        }
        self.selection.cursor = Some(cursor);
        // extending from here goes on from the oldest commit
        self.select(oldest, newest);
    }

    /// Extends the selection to the last commit of the section under the cursor that is shown
    /// in `direction`, and puts the cursor on it. The selection goes on from its anchor if it has
    /// several commits or extend mode is on, else it starts at the commit under the cursor. With
    /// a query, the commits that don't match are not shown, so they are not stopped at.
    fn extend_to_section_end(&mut self, direction: Direction) {
        self.follow_cursor();
        let Some(cursor) = self.cursor_pos() else {
            return;
        };
        let is_commit = |pos: usize| self.commit_at(pos).is_some();
        let (mut newest, mut oldest) = (cursor, cursor);
        while newest > 0 && is_commit(newest - 1) {
            newest -= 1;
        }
        while is_commit(oldest + 1) {
            oldest += 1;
        }
        // The rows that match are not in the order of the log: look for the one that is the
        // furthest in the section.
        let mut target = (cursor, self.picker.cursor_index());
        let mut index = 0;
        while let Some(row) = self.picker.matched_item(index) {
            let further = match direction {
                Direction::Forward => row.pos > target.0,
                Direction::Backward => row.pos < target.0,
            };
            if further && (newest..=oldest).contains(&row.pos) && row.commit().is_some() {
                target = (row.pos, index);
            }
            index += 1;
        }
        let anchor = match self.selection.anchor {
            Some(anchor) if self.selection.extend || self.is_range() => anchor,
            _ => cursor,
        };
        self.picker.set_cursor_index(target.1);
        self.selection.cursor = Some(target.0);
        self.select(anchor, target.0);
    }

    /// Follows the working tree: reads the number of uncommitted files again when it changed, and
    /// the log too when HEAD moved (a commit, a checkout...), keeping the cursor on the same row.
    fn refresh(&mut self) {
        if self.live.as_mut().is_some_and(Live::changed) {
            let source = self.source.clone();
            let known = self.head.clone();
            self.refresh.run(move || source.read(known));
        }
        let Some(refresh) = self.refresh.take() else {
            return;
        };
        if let Some(files) = refresh.files {
            self.uncommitted.set(files);
        }
        self.head = Some(refresh.head);
        let Some(log) = refresh.log else {
            return;
        };
        let selected = self
            .picker
            .selection()
            .map(|row| row.commit().map(|commit| commit.id.clone()));
        let rows = std::iter::once(LogRow::uncommitted(None)).chain(
            log.into_iter()
                .enumerate()
                .map(|(i, entry)| LogRow::new(i + 1, entry)),
        );
        self.picker.replace_items(rows, 0);
        self.selection = Selection::default();
        self.marked.set(None);
        // the same commit, or the uncommitted changes
        if let Some(selected) = selected {
            let mut index = 0;
            while let Some(row) = self.picker.matched_item(index) {
                if row.commit().map(|commit| &commit.id) == selected.as_ref() {
                    self.picker.set_cursor_index(index);
                    break;
                }
                index += 1;
            }
        }
    }

    fn toggle_extend(&mut self) {
        self.follow_cursor();
        self.selection.extend = !self.selection.extend;
    }

    /// Back to the commit under the cursor alone.
    fn collapse(&mut self) {
        self.selection.extend = false;
        self.selection.cursor = self.cursor_pos();
        if let Some(cursor) = self.selection.cursor {
            self.select(cursor, cursor);
        }
    }

    /// Enter on a range of commits: the list of their combined change, or in the line history,
    /// the diff of the file over the range at the lines.
    fn open_range(&self, cx: &mut compositor::Context) {
        let Some(range) = self.range() else {
            return;
        };
        if self.lines.is_none() {
            return open_commit_files(cx, self.source.cwd.clone(), range, &self.requests);
        }
        if let Some((newest, oldest)) = self.range_rows() {
            if let (Some(newest_line), Some(oldest_line)) = (newest.line(), oldest.line()) {
                let cwd = self.source.cwd.clone();
                super::line_history::open_diff(cx, cwd, range, oldest_line, newest_line);
            }
        }
    }

    /// `o` in the line history: the list of all the changes of the commit under the cursor, or
    /// of the range, or the uncommitted changes, as Enter in the commit log.
    fn open_whole(&self, cx: &mut compositor::Context) {
        let cwd = self.source.cwd.clone();
        if let Some(range) = self.range() {
            return open_commit_files(cx, cwd, range, &self.requests);
        }
        if self
            .picker
            .selection()
            .is_some_and(|row| matches!(row.kind, RowKind::Uncommitted))
        {
            self.requests.start();
            let watch = RepoWatch::start(cx.editor, &cwd);
            uncommitted::open(cx, cwd, watch);
        }
    }
}

impl Component for CommitLog {
    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut compositor::Context) {
        self.refresh();
        self.uncommitted
            .set_unsaved(uncommitted::modified_buffers(cx.editor, &self.work_dir).count());
        self.update_info();
        self.picker.render(area, surface, cx);
        // the first commit gets the cursor when it arrives
        self.follow_cursor();
        self.update_info();
    }

    fn handle_event(&mut self, event: &Event, cx: &mut compositor::Context) -> EventResult {
        let Event::Key(key) = event else {
            return self.picker.handle_event(event, cx);
        };
        // The second key of `gg`/`ge` is the picker's.
        let normal = self.picker.mode() == PickerMode::Normal && !self.picker.pending_key();
        match *key {
            alt!('v') => self.toggle_extend(),
            key!('v') if normal => self.toggle_extend(),
            key!('w') | key!('e') if normal => self.extend_to_section_end(Direction::Forward),
            key!('b') if normal => self.extend_to_section_end(Direction::Backward),
            shift!(Down) => self.extend_by(Direction::Forward),
            shift!(Up) => self.extend_by(Direction::Backward),
            alt!('s') => self.select_section(),
            key!(Esc) if normal && (self.is_range() || self.selection.extend) => self.collapse(),
            key!(Enter) if self.is_range() => {
                self.open_range(cx);
                return EventResult::Consumed(None);
            }
            // the whole commit, in the line history
            key!('o') if normal && self.lines.is_some() => {
                self.open_whole(cx);
                return EventResult::Consumed(None);
            }
            _ => {
                let result = self.picker.handle_event(event, cx);
                self.follow_cursor();
                self.update_info();
                return result;
            }
        }
        self.update_info();
        EventResult::Consumed(None)
    }

    fn cursor(&self, area: Rect, editor: &Editor) -> (Option<Position>, CursorKind) {
        self.picker.cursor(area, editor)
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        self.picker.required_size(viewport)
    }

    fn id(&self) -> Option<&'static str> {
        self.picker.id()
    }
}

pub(crate) struct CommitStyle {
    id: vortex_view::graphics::Style,
    date: vortex_view::graphics::Style,
    author: vortex_view::graphics::Style,
    uncommitted_style: vortex_view::graphics::Style,
    uncommitted: Arc<UncommittedCount>,
    /// The label of the uncommitted changes, when it isn't their count (the line history).
    uncommitted_label: Option<String>,
    /// `(added)` in the line history.
    added: vortex_view::graphics::Style,
    pub(crate) preview: changes::PreviewStyles,
}

/// How long the changes of commits may take to read before the status line says they are
/// being read: below that, the list just appears.
const LOADING_DELAY: Duration = Duration::from_millis(100);

/// The requests of the commit log for a list of changes. Each new one makes the earlier ones
/// stale: their results are dropped, as they are when the log is closed (a closed picker is
/// kept for resuming it, so closed means not among the layers of the compositor).
#[derive(Clone, Default)]
pub(crate) struct Requests(Arc<AtomicU64>);

impl Requests {
    /// A new request, which is current while no newer one is made and the log is open.
    fn start(&self) -> Request {
        let number = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        Request {
            requests: Arc::downgrade(&self.0),
            number,
        }
    }
}

#[derive(Clone)]
struct Request {
    requests: Weak<AtomicU64>,
    number: u64,
}

impl Request {
    /// Whether no newer request was made (off the main thread: the log may be closed).
    fn is_latest(&self) -> bool {
        self.requests
            .upgrade()
            .is_some_and(|requests| requests.load(Ordering::Relaxed) == self.number)
    }

    /// Whether the log it was made by is open, on the screen.
    fn log_is_open(&self, compositor: &mut Compositor) -> bool {
        compositor
            .find::<Overlay<CommitLog>>()
            .is_some_and(|log| Weak::as_ptr(&self.requests) == Arc::as_ptr(&log.content.requests.0))
    }

    /// Whether its result is wanted: it is the latest, and its log is open.
    fn is_current(&self, compositor: &mut Compositor) -> bool {
        self.is_latest() && self.log_is_open(compositor)
    }
}

/// Pushes the list of the changes of the commits of `range` on top of the commit picker. The
/// changes are read in the background, while the log stays usable: the list appears when they
/// are read, unless another list was asked for or the log was closed meanwhile.
pub(crate) fn open_commit_files(
    cx: &mut compositor::Context,
    cwd: PathBuf,
    range: CommitRange,
    requests: &Requests,
) {
    let request = requests.start();
    let range = Arc::new(range);
    let trust_full = trust_full(cx.editor, &cwd);
    let providers = cx.editor.diff_providers.clone();
    let config = cx.editor.config().changes.clone();
    let loading = format!("Loading changes of {}…", range.label());
    let done = Arc::new(AtomicBool::new(false));

    // the prompt row says so only when it takes long enough to be noticed
    tokio::spawn({
        let (request, done) = (request.clone(), done.clone());
        async move {
            tokio::time::sleep(LOADING_DELAY).await;
            job::dispatch(move |_, compositor| {
                if request.is_current(compositor) && !done.load(Ordering::Relaxed) {
                    if let Some(log) = compositor.find::<Overlay<CommitLog>>() {
                        log.content.set_loading(Some(loading));
                    }
                }
            })
            .await;
        }
    });

    tokio::task::spawn_blocking(move || {
        let rows = read_commit_rows(&providers, &cwd, trust_full, &range, &config, &request);
        done.store(true, Ordering::Relaxed);
        job::dispatch_blocking(move |editor, compositor| {
            if !request.is_current(compositor) {
                return;
            }
            // read: not "loading" any more
            if let Some(log) = compositor.find::<Overlay<CommitLog>>() {
                log.content.set_loading(None);
            }
            let rows = match rows {
                Ok(Some(rows)) => rows,
                Ok(None) => return,
                Err(err) => return editor.set_error(format!("{err}")),
            };
            if rows.is_empty() {
                return editor.set_status(format!("{} changes no files", range.label()));
            }
            let totals = changes::Totals::of(&rows).describe();
            let info = if range.count == 1 {
                format!(
                    "{}: {totals} · {}",
                    range.newest.short_id, range.newest.summary
                )
            } else {
                format!("{}: {totals}", range.label())
            };
            let picker = commit_changes_picker(editor, cwd, range, rows).with_info(info);
            compositor.push(Box::new(overlaid(picker)));
        });
    });
}

/// The rows of the changes of `range`: the hunks of each file, under its divider. Read off the
/// main thread, the repository opened once for all the files; `None` when `request` became
/// stale before they were all read, which stops the reading.
fn read_commit_rows(
    providers: &DiffProviderRegistry,
    cwd: &Path,
    trust_full: bool,
    range: &CommitRange,
    config: &ChangesConfig,
    request: &Request,
) -> anyhow::Result<Option<Vec<changes::ChangeRow>>> {
    let (oldest, newest) = (&range.oldest.id, &range.newest.id);
    let files = providers.range_changes(cwd, trust_full, oldest, newest)?;
    let paths: Vec<_> = files.iter().map(diff_view::paths_of).collect();
    let mut groups = Vec::with_capacity(files.len());
    providers.for_each_range_file(
        cwd,
        trust_full,
        oldest,
        newest,
        &paths,
        |i, before, after| {
            if !request.is_latest() {
                return false;
            }
            let group =
                changes::commit_group(&files[i], before.as_deref(), after.as_deref(), config);
            groups.push(group);
            true
        },
    )?;
    if groups.len() < files.len() {
        return Ok(None);
    }
    Ok(Some(changes::sort_groups(groups)))
}

/// The list of the changes of `range`. Enter and `C-g` open the diff of the file of the row on
/// top, at its hunk; `C-s`/`C-v`... open the file as it is now, closing the commit log.
fn commit_changes_picker(
    editor: &Editor,
    cwd: PathBuf,
    range: Arc<CommitRange>,
    rows: Vec<changes::ChangeRow>,
) -> Picker<changes::ChangeRow, changes::RowData> {
    let picker_cwd = cwd.clone();
    let files = Arc::new(changes::nav_files(&rows));
    let enter = move |cx: &mut compositor::Context, row: &changes::ChangeRow| {
        let cwd = cwd.clone();
        let range = range.clone();
        let change = row.change.clone();
        // a deleted file has no lines on the right side: its first hunk
        let line = match &row.what {
            changes::What::Hunk(hunk) if hunk.edit != changes::Edit::Delete => {
                Some(hunk.lines.start)
            }
            _ => None,
        };
        let files = files.clone();
        cx.jobs.callback(async move {
            let call = move |editor: &mut Editor, compositor: &mut Compositor| {
                if let Err(err) = diff_view::open_commit(
                    editor,
                    compositor,
                    &cwd,
                    &range.oldest,
                    &range.newest,
                    &change,
                    line,
                    std::rc::Rc::new(move || files.as_ref().clone()),
                    std::rc::Rc::new(changes::select_in_commit_files),
                ) {
                    editor.set_error(err.to_string());
                }
            };
            Ok(Callback::EditorCompositor(Box::new(call)))
        });
    };
    let open = |cx: &mut compositor::Context, row: &changes::ChangeRow, action| {
        // the file as it is now, closing the commit log below too
        let path = row.change.path().to_path_buf();
        if let Err(err) = cx.editor.open(&path, action) {
            cx.editor
                .set_error(format!("unable to open \"{}\": {err}", path.display()));
        }
        cx.jobs.callback(async move {
            let call = |_: &mut Editor, compositor: &mut Compositor| {
                compositor.remove(crate::ui::picker::ID);
            };
            Ok(Callback::EditorCompositor(Box::new(call)))
        });
    };
    changes::picker(editor, picker_cwd, rows, false, enter, open)
}
