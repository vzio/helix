//! `:worktrees` (`:wt`, this fork, see FORK.md, "Worktrees"): a picker of the repository's
//! worktrees, the current one first and marked, the others by when they were last modified,
//! newest first. It opens at once with their paths and branches; their uncommitted changes,
//! unpushed commits and last modified times fill in from a background job per worktree, in the
//! dashboard's loading style. `Enter` does what `:cd <worktree>` does, nothing more. Only local
//! git is read, never the network.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use tui::{
    buffer::Buffer as Surface,
    text::{Span, Spans},
    widgets::Cell,
};
use vortex_vcs::summary::{Details, Repo, Worktree};
use vortex_view::{
    graphics::{CursorKind, Rect, Style},
    Editor,
};

use crate::compositor::{self, Component, Compositor, Event, EventResult};
use crate::dashboard::render::{shimmer, Styles};
use crate::key;
use crate::ui::{Picker, PickerColumn};

/// Shown instead of the picker outside a repository, or without other worktrees.
pub(crate) const NO_WORKTREES: &str = "no other worktrees";
/// The width of a loading cell.
const LOADING: u16 = 8;
/// How often the loading cells move.
const FRAME: Duration = Duration::from_millis(45);

/// What the background job read of a worktree.
enum Loaded {
    Pending,
    Ready(Details),
    Failed,
}

/// A row of the picker: a worktree.
pub(crate) struct Row {
    worktree: Worktree,
    /// The worktree the working directory is in.
    current: bool,
    /// When it was last modified from its git dir alone, for the order it opens in.
    quick: Option<SystemTime>,
    loaded: Mutex<Loaded>,
}

impl Row {
    fn new(worktree: Worktree, current: bool) -> Self {
        let loaded = if worktree.missing {
            Loaded::Failed
        } else {
            Loaded::Pending
        };
        Self {
            quick: worktree.quick_modified(),
            worktree,
            current,
            loaded: Mutex::new(loaded),
        }
    }

    /// When it was last modified, once read (see `Repo::status`).
    fn modified(&self) -> Option<SystemTime> {
        match &*self.loaded.lock().unwrap() {
            Loaded::Ready(details) => details.modified,
            _ => None,
        }
    }

    /// The cell of a background column: `f` of the details once read, the shimmer while they
    /// load, `missing` or `couldn't read` (calm) when they can't be read.
    fn detail(
        &self,
        data: &Data,
        f: impl FnOnce(&Details, &Styles) -> Vec<Span<'static>>,
    ) -> Cell<'static> {
        match &*self.loaded.lock().unwrap() {
            Loaded::Ready(details) => Spans::from(f(details, &data.styles)).into(),
            Loaded::Pending => {
                let spans = shimmer(LOADING, data.born, Instant::now(), &data.styles)
                    .into_iter()
                    .map(|(text, style)| Span::styled(text, style))
                    .collect::<Vec<_>>();
                Spans::from(spans).into()
            }
            Loaded::Failed if self.worktree.missing => Cell::default(),
            Loaded::Failed => Span::styled("couldn't read", data.styles.label).into(),
        }
    }
}

/// What the columns draw with.
pub(crate) struct Data {
    styles: Styles,
    /// The current worktree's marker.
    marker: Style,
    /// When the picker opened, for the shimmer.
    born: Instant,
}

fn changes_cell(details: &Details, s: &Styles) -> Vec<Span<'static>> {
    let changes = &details.changes;
    if changes.is_clean() {
        return vec![Span::styled("clean", s.good)];
    }
    let mut spans = Vec::new();
    for (n, what) in [
        (changes.staged, "staged"),
        (changes.unstaged, "unstaged"),
        (changes.untracked, "untracked"),
    ] {
        if n == 0 {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", s.label));
        }
        spans.push(Span::styled(format!("{n} {what}"), s.value));
    }
    spans
}

fn unpushed_cell(details: &Details, s: &Styles) -> Vec<Span<'static>> {
    vec![match &details.upstream {
        None => Span::styled("no upstream", s.label),
        Some(upstream) if upstream.ahead == 0 => Span::styled("all pushed", s.good),
        Some(upstream) => Span::styled(format!("{} unpushed", upstream.ahead), s.warning),
    }]
}

fn modified_cell(details: &Details, s: &Styles) -> Vec<Span<'static>> {
    match details.modified {
        Some(modified) => vec![Span::styled(
            vortex_stdx::time::format_ago(modified, SystemTime::now()),
            s.label,
        )],
        None => Vec::new(),
    }
}

/// The picker, with what it needs to order its rows once they are all read.
pub(crate) struct WorktreePicker {
    picker: Picker<Arc<Row>, Data>,
    rows: Vec<Arc<Row>>,
    /// The background jobs still reading.
    pending: Arc<AtomicUsize>,
    /// Whether the rows were ordered by their full last modified times.
    sorted: bool,
}

/// The order of the rows: the current one first, then by last modified, newest first, from
/// `time` (the quick one while opening, the full one once read); the undated last, by path.
fn order(rows: &mut [Arc<Row>], time: impl Fn(&Row) -> Option<SystemTime>) {
    rows.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then_with(|| time(b).cmp(&time(a)))
            .then_with(|| a.worktree.path.cmp(&b.worktree.path))
    });
}

/// `:worktrees`: the picker of the repository's worktrees, or `None` (and a message) outside a
/// repository or without other worktrees (missing ones don't count).
pub(crate) fn worktree_picker(editor: &mut Editor) -> Option<WorktreePicker> {
    let cwd = vortex_stdx::env::current_working_dir();
    let trust_full = super::git_log::trust_full(editor, &cwd);
    let listed = (|| {
        let repo = Repo::open(&cwd, trust_full).ok()??;
        let current = repo.current_worktree().ok()?;
        let others = repo.worktrees().ok()?;
        Some((current, others))
    })();
    let Some((current, others)) =
        listed.filter(|(_, others)| others.iter().any(|worktree| !worktree.missing))
    else {
        editor.set_status(NO_WORKTREES);
        return None;
    };

    let mut rows: Vec<Arc<Row>> = std::iter::once(Row::new(current, true))
        .chain(others.into_iter().map(|worktree| Row::new(worktree, false)))
        .map(Arc::new)
        .collect();
    order(&mut rows, |row| row.quick);

    let columns = [
        PickerColumn::new("", |row: &Arc<Row>, data: &Data| {
            if row.current {
                Span::styled("●", data.marker).into()
            } else {
                Cell::default()
            }
        })
        .without_filtering(),
        PickerColumn::new("path", |row: &Arc<Row>, data: &Data| {
            let path = vortex_stdx::path::fold_home_dir(&row.worktree.path);
            Span::styled(path.display().to_string(), data.styles.value).into()
        }),
        PickerColumn::new("branch", |row: &Arc<Row>, data: &Data| {
            let worktree = &row.worktree;
            let s = &data.styles;
            match (&worktree.branch, &worktree.short_id) {
                _ if worktree.missing => Span::styled("missing", s.label).into(),
                (Some(branch), _) => Span::styled(branch.clone(), s.detail).into(),
                (None, Some(id)) => Span::styled(format!("detached at {id}"), s.label).into(),
                (None, None) => Span::styled("detached", s.label).into(),
            }
        }),
        PickerColumn::new("changes", |row: &Arc<Row>, data: &Data| {
            row.detail(data, changes_cell)
        })
        .without_filtering(),
        PickerColumn::new("unpushed", |row: &Arc<Row>, data: &Data| {
            row.detail(data, unpushed_cell)
        })
        .without_filtering(),
        PickerColumn::new("modified", |row: &Arc<Row>, data: &Data| {
            row.detail(data, modified_cell)
        })
        .without_filtering(),
    ];
    let data = Data {
        styles: Styles::new(&editor.theme),
        marker: editor
            .theme
            .try_get_exact("ui.text.focus")
            .unwrap_or_else(|| editor.theme.get("ui.text")),
        born: Instant::now(),
    };
    let picker = Picker::new(columns, 1, rows.clone(), data, |cx, row: &Arc<Row>, _| {
        pick(cx, row)
    })
    .with_modes()
    .without_header()
    .with_help(
        "Worktrees",
        vec![("worktrees", vec![("Enter", "go to the worktree (:cd)")])],
    );

    let pending = Arc::new(AtomicUsize::new(0));
    for row in rows.iter().filter(|row| !row.worktree.missing) {
        pending.fetch_add(1, Ordering::Relaxed);
        let row = row.clone();
        let pending = pending.clone();
        let read = move || {
            let loaded = match row.worktree.details(trust_full) {
                Ok(details) => Loaded::Ready(details),
                Err(err) => {
                    log::debug!("worktrees: {}: {err:#}", row.worktree.path.display());
                    Loaded::Failed
                }
            };
            *row.loaded.lock().unwrap() = loaded;
            pending.fetch_sub(1, Ordering::Relaxed);
            vortex_event::request_redraw();
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => drop(handle.spawn_blocking(read)),
            Err(_) => read(),
        }
    }
    // frames for the shimmer while something loads
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let pending = Arc::downgrade(&pending);
        handle.spawn(tick(pending));
    }

    Some(WorktreePicker {
        picker,
        rows,
        pending,
        sorted: false,
    })
}

/// Asks for a frame every [`FRAME`] while a row loads.
async fn tick(pending: Weak<AtomicUsize>) {
    loop {
        tokio::time::sleep(FRAME).await;
        match pending.upgrade() {
            Some(pending) if pending.load(Ordering::Relaxed) > 0 => vortex_event::request_redraw(),
            _ => break,
        }
    }
}

/// `Enter` on `row`: `:cd` to it, nothing else; nothing for the current worktree.
fn pick(cx: &mut compositor::Context, row: &Row) {
    if row.current || row.worktree.missing {
        return;
    }
    if let Err(err) = super::typed::apply_directory_change(cx, &row.worktree.path) {
        cx.editor.set_error(err.to_string());
    }
}

impl WorktreePicker {
    /// Once every row is read, orders them by their full last modified times, keeping the cursor
    /// on its row. Not while filtering: the rows would move under the query.
    fn sort_when_read(&mut self) {
        if self.sorted || self.pending.load(Ordering::Relaxed) > 0 {
            return;
        }
        self.sorted = true;
        if !self.picker.query_line().is_empty() {
            return;
        }
        let mut rows = self.rows.clone();
        order(&mut rows, |row| row.modified().or(row.quick));
        let same = rows.iter().zip(&self.rows).all(|(a, b)| Arc::ptr_eq(a, b));
        if same {
            return;
        }
        let selected = self
            .picker
            .selection()
            .and_then(|selected| rows.iter().position(|row| Arc::ptr_eq(row, selected)))
            .unwrap_or(0);
        self.picker.replace_items(rows.clone(), selected as u32);
        self.rows = rows;
    }
}

impl Component for WorktreePicker {
    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut compositor::Context) {
        self.sort_when_read();
        self.picker.render(area, surface, cx);
    }

    fn handle_event(&mut self, event: &Event, cx: &mut compositor::Context) -> EventResult {
        if let Event::Key(key!(Enter)) = event {
            let Some(row) = self.picker.selection().cloned() else {
                return EventResult::Consumed(None);
            };
            // a missing worktree can't be picked: the picker stays
            if row.worktree.missing {
                cx.editor.set_error(format!(
                    "{} is missing",
                    vortex_stdx::path::fold_home_dir(&row.worktree.path).display()
                ));
                return EventResult::Consumed(None);
            }
            pick(cx, &row);
            return EventResult::Consumed(Some(Box::new(|compositor: &mut Compositor, _| {
                compositor.pop();
            })));
        }
        self.picker.handle_event(event, cx)
    }

    fn cursor(&self, area: Rect, editor: &Editor) -> (Option<vortex_core::Position>, CursorKind) {
        self.picker.cursor(area, editor)
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        self.picker.required_size(viewport)
    }

    fn id(&self) -> Option<&'static str> {
        self.picker.id()
    }
}
