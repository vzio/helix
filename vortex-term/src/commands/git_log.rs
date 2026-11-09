use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use vortex_vcs::DiffProviderRegistry;

use vortex_core::{movement::Direction, Position};
use vortex_event::status;
use vortex_vcs::{CommitInfo, FileChange, LogEntry};
use vortex_view::{
    graphics::{CursorKind, Rect},
    Editor,
};
use tui::{buffer::Buffer as Surface, text::Span};

use super::{changes, diff_view};
use crate::compositor::{self, Component, Compositor, Event, EventResult};
use crate::job::Callback;
use crate::live::{Live, Pending, RepoWatch};
use crate::ui::{overlay::overlaid, picker::PickerMode, Picker, PickerColumn};
use crate::{alt, key, shift};

/// Shown in the status line when the log opens.
const HINT: &str = "s filter, v extend, w/b section, enter files";

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
                    Span::styled(style.uncommitted.label(), style.uncommitted_style).into()
                }
                _ => row.text(|commit| commit.summary.as_str().into()),
            }
        }),
    ];
    let uncommitted = Arc::new(UncommittedCount::default());
    let style = CommitStyle {
        id: editor.theme.get("constant"),
        date: editor.theme.get("comment"),
        author: editor.theme.get("variable"),
        uncommitted_style: editor.theme.get("diff.delta"),
        uncommitted: uncommitted.clone(),
    };

    let watch = RepoWatch::start(editor, &cwd);
    let marked = Rc::new(Cell::new(None));
    let open = {
        let cwd = cwd.clone();
        let watch = watch.clone();
        move |cx: &mut compositor::Context, row: &LogRow| match &row.kind {
            RowKind::Uncommitted => changes::open_uncommitted(cx, cwd.clone(), watch.clone()),
            RowKind::Commit(commit) => {
                open_commit_files(cx, cwd.clone(), CommitRange::single(commit))
            }
            RowKind::Divider(_) => {}
        }
    };
    let first_rows = [LogRow {
        pos: 0,
        kind: RowKind::Uncommitted,
    }];
    let picker = Picker::new(columns, 3, first_rows, style, {
        let open = open.clone();
        move |cx, row, _action| open(cx, row)
    })
    .with_stacked_key_handler(key!(Enter), open)
    .with_modes()
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
    let injector = picker.injector();

    let trust_full = trust_full(editor, &cwd);
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
    editor.set_status(HINT);
    let log = CommitLog {
        picker,
        source: Source {
            cwd,
            providers: editor.diff_providers.clone(),
            trust_full,
        },
        marked,
        selection: Selection::default(),
        announced: false,
        live: watch.map(Live::new),
        refresh: Pending::new(),
        uncommitted,
        head: None,
    };
    // the number of uncommitted files, and the HEAD the log is read from
    let source = log.source.clone();
    log.refresh.run(move || source.read(None));
    Some(log)
}

/// The number of uncommitted files the commit log shows, updated while it is open.
#[derive(Default)]
pub(crate) struct UncommittedCount {
    /// The number of files, plus one; 0 while it isn't known yet.
    files: AtomicUsize,
}

impl UncommittedCount {
    fn set(&self, files: usize) {
        self.files.store(files + 1, Ordering::Relaxed);
    }

    /// `Uncommitted changes (3 files)`.
    fn label(&self) -> String {
        match self.files.load(Ordering::Relaxed) {
            0 => "Uncommitted changes".to_string(),
            1 => "Uncommitted changes (none)".to_string(),
            2 => "Uncommitted changes (1 file)".to_string(),
            files => format!("Uncommitted changes ({} files)", files - 1),
        }
    }
}

/// What the commit log reads off the main thread.
#[derive(Clone)]
struct Source {
    cwd: PathBuf,
    providers: DiffProviderRegistry,
    trust_full: bool,
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
pub(crate) struct LogRow {
    /// The position among the rows, dividers included; the uncommitted changes are at 0.
    pos: usize,
    kind: RowKind,
}

enum RowKind {
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
        Self { pos, kind }
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
    count: usize,
}

impl CommitRange {
    fn single(commit: &CommitInfo) -> Self {
        Self {
            oldest: commit.clone(),
            newest: commit.clone(),
            count: 1,
        }
    }

    /// `a1b2c3d..d4e5f6a (5 commits)`, or `a1b2c3d` for one commit.
    fn label(&self) -> String {
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
    /// Whether the status line shows the selection.
    announced: bool,
    /// Sees the working tree change while the log is open.
    live: Option<Live>,
    refresh: Pending<Refresh>,
    uncommitted: Arc<UncommittedCount>,
    /// The commit HEAD pointed to when the log was read, once known.
    head: Option<Option<String>>,
}

impl CommitLog {
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

    /// Shows the selection in the status line when it's more than the commit under the cursor.
    fn announce(&mut self, editor: &mut Editor) {
        let text = (self.is_range() || self.selection.extend)
            .then(|| self.range())
            .flatten()
            .map(|range| {
                let extend = if self.selection.extend {
                    "[extend] "
                } else {
                    ""
                };
                format!("{extend}{}", range.label())
            });
        match text {
            Some(text) => editor.set_status(text),
            None if self.announced => editor.clear_status(),
            None => {}
        }
        self.announced = self.is_range() || self.selection.extend;
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
        let rows = std::iter::once(LogRow {
            pos: 0,
            kind: RowKind::Uncommitted,
        })
        .chain(
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
}

impl Component for CommitLog {
    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut compositor::Context) {
        self.refresh();
        self.picker.render(area, surface, cx);
        // the first commit gets the cursor when it arrives
        self.follow_cursor();
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
                if let Some(range) = self.range() {
                    open_commit_files(cx, self.source.cwd.clone(), range);
                }
                return EventResult::Consumed(None);
            }
            _ => {
                let result = self.picker.handle_event(event, cx);
                self.follow_cursor();
                self.announce(cx.editor);
                return result;
            }
        }
        self.announce(cx.editor);
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
}

/// Pushes the picker of the files changed by the commits of `range` on top of the commit picker.
fn open_commit_files(cx: &mut compositor::Context, cwd: PathBuf, range: CommitRange) {
    let range = Arc::new(range);
    cx.jobs.callback(async move {
        let call = move |editor: &mut Editor, compositor: &mut Compositor| {
            let trust_full = trust_full(editor, &cwd);
            let changes = match editor.diff_providers.range_changes(
                &cwd,
                trust_full,
                &range.oldest.id,
                &range.newest.id,
            ) {
                Ok(changes) => changes,
                Err(err) => {
                    editor.set_error(format!("{err}"));
                    return;
                }
            };
            if changes.is_empty() {
                editor.set_status(format!("{} changes no files", range.label()));
                return;
            }
            if range.count == 1 {
                editor.set_status(format!(
                    "{} {}",
                    range.newest.short_id, range.newest.summary
                ));
            } else {
                editor.set_status(range.label());
            }
            let picker = commit_files_picker(editor, cwd, range, changes);
            compositor.push(Box::new(overlaid(picker)));
        };
        Ok(Callback::EditorCompositor(Box::new(call)))
    });
}

/// The files changed by the commits of `range`, lockfiles last. Enter and `C-g` open the diff of
/// the selected file on top.
fn commit_files_picker(
    editor: &Editor,
    cwd: PathBuf,
    range: Arc<CommitRange>,
    changes: Vec<FileChange>,
) -> Picker<changes::FileRow, changes::FileRowData> {
    let trust_full = trust_full(editor, &cwd);
    let read = |id: &str, path: &Path| {
        editor
            .diff_providers
            .file_at_commit(&cwd, trust_full, id, path)
            .ok()
            .flatten()
            .unwrap_or_default()
    };
    let parent = format!("{}^", range.oldest.id);
    let rows = changes::rows(changes, &editor.config().changes, |change| {
        let (base_path, path) = diff_view::paths_of(change);
        Some((read(&parent, &base_path), read(&range.newest.id, &path)))
    });

    let picker_cwd = cwd.clone();
    let show_diff = move |cx: &mut compositor::Context, change: &FileChange| {
        let cwd = cwd.clone();
        let range = range.clone();
        let change = change.clone();
        cx.jobs.callback(async move {
            let call = move |editor: &mut Editor, compositor: &mut Compositor| {
                if let Err(err) = diff_view::open_commit(
                    editor,
                    compositor,
                    &cwd,
                    &range.oldest,
                    &range.newest,
                    &change,
                ) {
                    editor.set_error(err.to_string());
                }
            };
            Ok(Callback::EditorCompositor(Box::new(call)))
        });
    };
    changes::files_picker(editor, picker_cwd, rows, show_diff)
}
