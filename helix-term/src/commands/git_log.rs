use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use helix_core::{movement::Direction, Position};
use helix_event::status;
use helix_vcs::{CommitInfo, FileChange, LogEntry};
use helix_view::{
    graphics::{CursorKind, Rect},
    Editor,
};
use tui::{buffer::Buffer as Surface, text::Span};

use super::{diff_view, file_change_columns};
use crate::compositor::{self, Component, Compositor, Event, EventResult};
use crate::job::Callback;
use crate::ui::{
    overlay::overlaid,
    picker::{self, PickerMode},
    Picker, PickerColumn,
};
use crate::{alt, ctrl, key, shift};

/// Shown in the status line when the log opens.
const HINT: &str = "s filter, v extend, w/b section, enter files";

fn trust_full(editor: &Editor, cwd: &Path) -> bool {
    editor
        .workspace_trust
        .query(
            &helix_loader::find_workspace_in(cwd).0,
            helix_loader::workspace_trust::TrustQuery::Git,
        )
        .is_trusted()
}

/// Lists the commits reachable from HEAD in sections by branch, see `FORK.md`. Enter lists the
/// files changed by the selected commit, or by the selected range of commits.
pub(crate) fn commit_picker(editor: &mut Editor) -> Option<CommitLog> {
    let cwd = helix_stdx::env::current_working_dir();
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
        PickerColumn::new("summary", |row: &LogRow, _: &CommitStyle| {
            row.text(|commit| commit.summary.as_str().into())
        }),
    ];
    let style = CommitStyle {
        id: editor.theme.get("constant"),
        date: editor.theme.get("comment"),
        author: editor.theme.get("variable"),
    };

    let marked = Rc::new(Cell::new(None));
    let open_cwd = cwd.clone();
    let picker = Picker::new(columns, 3, [], style, move |cx, row, _action| {
        if let Some(commit) = row.commit() {
            open_commit_files(cx, open_cwd.clone(), CommitRange::single(commit))
        }
    })
    .with_stacked_key_handler(key!(Enter), {
        let cwd = cwd.clone();
        move |cx, row| {
            if let Some(commit) = row.commit() {
                open_commit_files(cx, cwd.clone(), CommitRange::single(commit))
            }
        }
    })
    .with_modes()
    .with_dividers(|row, _| match &row.entry {
        LogEntry::Divider(title) => Some(title.as_str().into()),
        LogEntry::Commit(_) => None,
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
    let pos = AtomicUsize::new(0);
    editor
        .diff_providers
        .clone()
        .for_each_log_entry(cwd.clone(), trust_full, move |entry| match entry {
            Ok(entry) => {
                let pos = pos.fetch_add(1, Ordering::Relaxed);
                injector.push(LogRow { pos, entry }).is_ok()
            }
            Err(err) => {
                status::report_blocking(err);
                false
            }
        });
    editor.set_status(HINT);
    Some(CommitLog {
        picker,
        cwd,
        marked,
        selection: Selection::default(),
        announced: false,
    })
}

/// A row of the commit picker: a commit or the divider of a section of the log.
pub(crate) struct LogRow {
    /// The position among the rows of the log, dividers included.
    pos: usize,
    entry: LogEntry,
}

impl LogRow {
    fn commit(&self) -> Option<&CommitInfo> {
        match &self.entry {
            LogEntry::Commit(commit) => Some(commit),
            LogEntry::Divider(_) => None,
        }
    }

    /// The cell for `text` of the commit, or an empty one for a divider: dividers match no query.
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
    cwd: PathBuf,
    /// The selected range of more than one commit, for the picker to mark the rows of.
    marked: Rc<Cell<Option<(usize, usize)>>>,
    selection: Selection,
    /// Whether the status line shows the selection.
    announced: bool,
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
        let range = (anchor.min(cursor), anchor.max(cursor));
        self.selection.range = Some(range);
        self.marked.set((range.0 != range.1).then_some(range));
    }

    /// The commits of the selection, both ends included, if it has any.
    fn range(&self) -> Option<CommitRange> {
        let (newest, oldest) = self.selection.range?;
        let count = (newest..=oldest)
            .filter(|&pos| self.commit_at(pos).is_some())
            .count();
        Some(CommitRange {
            oldest: self.commit_at(oldest)?.clone(),
            newest: self.commit_at(newest)?.clone(),
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
                    open_commit_files(cx, self.cwd.clone(), range);
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
    id: helix_view::graphics::Style,
    date: helix_view::graphics::Style,
    author: helix_view::graphics::Style,
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

/// The files changed by the commits of `range`. Enter and `C-g` open the diff of the selected file
/// on top.
fn commit_files_picker(
    editor: &Editor,
    cwd: PathBuf,
    range: Arc<CommitRange>,
    changes: Vec<FileChange>,
) -> Picker<FileChange, super::FileChangeData> {
    let (columns, data) = file_change_columns(editor, cwd.clone());
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

    Picker::new(
        columns,
        1,
        changes,
        data,
        |cx, change: &FileChange, action| {
            // the file as it is now, closing the commit picker below too
            let path = change.path().to_path_buf();
            if let Err(err) = cx.editor.open(&path, action) {
                cx.editor
                    .set_error(format!("unable to open \"{}\": {err}", path.display()));
            }
            cx.jobs.callback(async move {
                let call = |_: &mut Editor, compositor: &mut Compositor| {
                    compositor.remove(picker::ID);
                };
                Ok(Callback::EditorCompositor(Box::new(call)))
            });
        },
    )
    .with_stacked_key_handler(key!(Enter), show_diff.clone())
    .with_stacked_key_handler(ctrl!('g'), show_diff)
}
