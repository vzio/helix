//! Line history (`Space L`, `:line-history`): the commits that changed the cursor's line or the
//! selected lines, in the commit log's picker (see `FORK.md`).
//!
//! The lines are mapped from the buffer to HEAD through the buffer's diff against HEAD (the one
//! the gutter shows); the walk of [`vortex_vcs::line_history`] follows them back from there on a
//! thread of its own, filling the picker as it finds commits, and stops when the picker closes.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use tui::buffer::Buffer as Surface;
use vortex_vcs::line_history::{map_back, normalize, touching, LineCommit, LineEvent};
use vortex_vcs::staging::{line_hunks, LineHunk};
use vortex_vcs::{CommitInfo, FileChange};
use vortex_view::{graphics::Rect, Editor};

use super::changes::{self, HunkPreview};
use super::git_log::{self, CommitLog, CommitRange, CommitStyle, LogRow, RowKind};
use crate::compositor::{self, Compositor};
use crate::job::Callback;

/// The lines followed in a commit (or in the buffer, for the uncommitted changes), for a row of
/// the line history.
pub(crate) struct LineRow {
    /// The file, in the commit (absolute).
    pub(crate) path: PathBuf,
    /// The file in the commit's first parent, where it had another path.
    pub(crate) old_path: Option<PathBuf>,
    /// The lines followed, from 0.
    pub(crate) lines: Vec<Range<u32>>,
    /// The commit added (some of) the lines: their history starts there.
    pub(crate) added: bool,
    /// The hunks of the commit that touch the lines: where each is, and its preview.
    hunks: Vec<(String, Arc<HunkPreview>)>,
}

impl LineRow {
    /// The change of the file from the first parent of `oldest` (`self`) to `newest`.
    fn change(&self, newest: &LineRow) -> FileChange {
        let from = self.old_path.as_ref().unwrap_or(&self.path);
        if *from == newest.path {
            FileChange::Modified {
                path: newest.path.clone(),
            }
        } else {
            FileChange::Renamed {
                from_path: from.clone(),
                to_path: newest.path.clone(),
            }
        }
    }
}

fn relative(path: &Path) -> String {
    vortex_stdx::path::get_relative_path(path)
        .display()
        .to_string()
}

/// The previews of the hunks of the diff from `before` to `after` that touch `lines`, each under
/// its place in `path` and its counts: `src/main.rs:12-14 · +3 -1`.
fn previews(
    path: &Path,
    before: &str,
    after: &str,
    hunks: &[LineHunk],
) -> Vec<(String, Arc<HunkPreview>)> {
    let path = relative(path);
    let (before, after) = (changes::lines(before), changes::lines(after));
    hunks
        .iter()
        .map(|hunk| {
            let mut title = changes::place(&path, &hunk.after);
            let counts =
                changes::line_counts_text(hunk.after.len() as u32, hunk.before.len() as u32);
            if !counts.is_empty() {
                title += &format!(" · {counts}");
            }
            (title, Arc::new(changes::preview(&before, &after, hunk)))
        })
        .collect()
}

/// `src/main.rs:12-14`, `src/main.rs:3,12-14`: the lines (from 0) as one-based ranges.
fn label(path: &Path, lines: &[Range<u32>]) -> String {
    let ranges: Vec<String> = lines
        .iter()
        .map(|range| match range.len() {
            1 => format!("{}", range.start + 1),
            _ => format!("{}-{}", range.start + 1, range.end),
        })
        .collect();
    format!("{}:{}", relative(path), ranges.join(","))
}

/// `1,240`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

fn commits(n: usize) -> String {
    match n {
        1 => "1 commit".to_string(),
        n => format!("{} commits", thousands(n)),
    }
}

/// The progress of a walk of the line history, shared with its thread.
#[derive(Default)]
struct Search {
    /// The commits looked at.
    searched: AtomicUsize,
    /// The commits listed.
    found: AtomicUsize,
    /// The walk ended: it went through all the lines' history, failed or was stopped.
    done: AtomicBool,
    error: Mutex<Option<String>>,
}

/// What the line history follows and how its search goes, for its picker's prompt row.
pub(crate) struct LineHistory {
    /// `src/main.rs:12-14`.
    label: String,
    /// `None` when none of the lines is committed: there is nothing to search.
    search: Option<Arc<Search>>,
}

impl LineHistory {
    /// `src/main.rs:12-14 · 7 commits`, with the progress while it searches.
    pub(crate) fn status(&self) -> String {
        let Some(search) = &self.search else {
            return format!("{} · these lines aren't committed yet", self.label);
        };
        let found = commits(search.found.load(Ordering::Relaxed));
        if let Some(error) = search.error.lock().unwrap().as_ref() {
            return format!("{} · {found} · stopped: {error}", self.label);
        }
        if search.done.load(Ordering::Relaxed) {
            return format!("{} · {found}", self.label);
        }
        let searched = commits(search.searched.load(Ordering::Relaxed));
        format!("{} · {found} · searching… {searched}", self.label)
    }
}

/// The walks of line histories running, by file: for the tests to see a walk stop.
static RUNNING: LazyLock<Mutex<HashMap<PathBuf, usize>>> = LazyLock::new(Default::default);

/// The number of walks of the line history of `file` running (for the tests: closing the picker
/// stops its walk).
pub fn walks_running(file: &Path) -> usize {
    RUNNING.lock().unwrap().get(file).copied().unwrap_or(0)
}

/// Counts a walk in [`RUNNING`] while it lives.
struct Running(PathBuf);

impl Running {
    fn start(file: &Path) -> Running {
        *RUNNING
            .lock()
            .unwrap()
            .entry(file.to_path_buf())
            .or_default() += 1;
        Running(file.to_path_buf())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut running = RUNNING.lock().unwrap();
        if let Some(count) = running.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                running.remove(&self.0);
            }
        }
    }
}

/// The lines every selection of the current view covers, from 0, normalized.
fn selected_lines(editor: &Editor) -> Vec<Range<u32>> {
    let (view, doc) = vortex_view::current_ref!(editor);
    let text = doc.text().slice(..);
    normalize(
        doc.selection(view.id)
            .iter()
            .map(|range| {
                let (start, end) = range.line_range(text);
                start as u32..end as u32 + 1
            })
            .collect(),
    )
}

/// Opens the line history of the lines of the current buffer every selection covers (the
/// cursor's line in normal mode). Says why in the status line when it can't: a buffer without a
/// file, outside a repository, a file git doesn't track.
pub(crate) fn open(editor: &mut Editor) -> Option<CommitLog> {
    let doc = vortex_view::doc!(editor);
    let Some(path) = doc.path().map(Path::to_path_buf) else {
        editor.set_error("The buffer has no file: line history needs one in a git repository");
        return None;
    };
    let dir = path.parent().unwrap_or(&path).to_path_buf();
    let trust_full = git_log::trust_full(editor, &dir);
    let buffer = doc.text().to_string();
    let lines = selected_lines(editor);

    // the buffer's diff against HEAD, the one of the gutter
    let doc = vortex_view::doc!(editor);
    let diff = doc.diff_handle().map(|handle| {
        let diff = handle.load();
        let hunks: Vec<LineHunk> = (0..diff.len())
            .map(|i| {
                let hunk = diff.nth_hunk(i);
                LineHunk {
                    before: hunk.before,
                    after: hunk.after,
                }
            })
            .collect();
        (diff.diff_base().to_string(), hunks)
    });
    let diff = diff.or_else(|| {
        let base = editor.diff_providers.get_diff_base(&path, trust_full)?;
        let base = String::from_utf8_lossy(&base).into_owned();
        let hunks = line_hunks(&base, &buffer);
        Some((base, hunks))
    });
    let Some((base, hunks)) = diff else {
        let name = relative(&path);
        let why = if editor.diff_providers.repo_state(&dir, trust_full).is_err() {
            format!("{name} isn't in a git repository")
        } else if matches!(
            editor.diff_providers.index_file(&path, trust_full),
            Ok(Some(_))
        ) {
            format!("{name} has no commits yet")
        } else {
            format!("{name} isn't tracked by git")
        };
        editor.set_error(why);
        return None;
    };

    let in_head = map_back(&lines, &hunks);
    let touched: Vec<LineHunk> = touching(&hunks, &lines).cloned().collect();
    let uncommitted = (!touched.is_empty()).then(|| LineRow {
        path: path.clone(),
        old_path: None,
        lines: lines.clone(),
        added: false,
        hunks: previews(&path, &base, &buffer, &touched),
    });

    let label = label(&path, &lines);
    let search = (!in_head.parent.is_empty()).then(|| Arc::new(Search::default()));
    let history = LineHistory {
        label,
        search: search.clone(),
    };
    // paths are shown from the working directory, or the file's working tree outside of it
    let cwd = vortex_stdx::env::current_working_dir();
    let cwd = if path.starts_with(&cwd) {
        cwd
    } else {
        editor
            .diff_providers
            .repo_state(&dir, trust_full)
            .map_or(dir.clone(), |state| state.work_dir)
    };
    let source = git_log::Source {
        cwd: cwd.clone(),
        providers: editor.diff_providers.clone(),
        trust_full,
    };
    let requests = git_log::Requests::default();
    let open = {
        move |cx: &mut compositor::Context, row: &LogRow| match (row.kind(), row.line()) {
            (RowKind::Uncommitted, Some(line)) => open_uncommitted(cx, line),
            (RowKind::Commit(commit), Some(line)) => {
                open_diff(cx, cwd.clone(), CommitRange::single(commit), line, line)
            }
            _ => {}
        }
    };
    let first_rows: Vec<_> = uncommitted
        .map(|row| LogRow::uncommitted(Some(row)))
        .into_iter()
        .collect();
    let first = first_rows.len();
    let log = CommitLog::new(
        editor,
        source,
        Default::default(),
        requests,
        open,
        first_rows,
        Some(history),
    );
    if let Some(search) = search {
        let walk = Walk {
            path,
            dir: dir.clone(),
            trust_full,
            lines: in_head.parent,
            first,
        };
        start(editor, &log, walk, search);
    }
    Some(log)
}

/// What a walk of the line history follows.
struct Walk {
    /// The file (absolute), and the directory its repository is found from.
    path: PathBuf,
    dir: PathBuf,
    trust_full: bool,
    /// The lines in HEAD.
    lines: Vec<Range<u32>>,
    /// The rows before the commits: the position of the first one.
    first: usize,
}

/// Starts the walk of the line history, filling `log`.
fn start(editor: &Editor, log: &CommitLog, walk: Walk, search: Arc<Search>) {
    let injector = log.injector();
    let providers = editor.diff_providers.clone();
    let Walk {
        path,
        dir,
        trust_full,
        lines,
        first,
    } = walk;

    // the progress on the prompt row, while nothing else draws
    tokio::spawn({
        let (search, injector) = (search.clone(), injector.clone());
        async move {
            while !search.done.load(Ordering::Relaxed) && !injector.is_shut_down() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                vortex_event::request_redraw();
            }
        }
    });

    tokio::task::spawn_blocking(move || {
        let _running = Running::start(&path);
        // a row's position is its index among the rows
        let mut pos = first;
        let result =
            providers.for_each_line_commit(&dir, trust_full, &path, lines, |event| match event {
                LineEvent::Searched(searched) => {
                    search.searched.store(searched, Ordering::Relaxed);
                    !injector.is_shut_down()
                }
                LineEvent::Divider(label) => {
                    pos += 1;
                    injector
                        .push(LogRow::divider(pos - 1, label.to_string()))
                        .is_ok()
                }
                LineEvent::Commit(commit) => {
                    let (info, row) = line_row(*commit);
                    pos += 1;
                    let pushed = injector.push(LogRow::line_commit(pos - 1, info, row));
                    search.found.fetch_add(1, Ordering::Relaxed);
                    pushed.is_ok()
                }
            });
        if let Err(err) = result {
            log::debug!("line history of {}: {err:#}", path.display());
            *search.error.lock().unwrap() = Some(err.to_string());
        }
        search.done.store(true, Ordering::Relaxed);
        vortex_event::request_redraw();
    });
}

fn line_row(commit: LineCommit<'_>) -> (CommitInfo, LineRow) {
    let hunks = previews(&commit.path, commit.before, commit.after, &commit.hunks);
    let row = LineRow {
        path: commit.path,
        old_path: commit.old_path,
        lines: commit.lines,
        added: commit.added,
        hunks,
    };
    (commit.info, row)
}

/// The preview of a row of the line history: the hunks of the commit that touch the lines, each
/// under its place and counts.
pub(crate) fn draw_preview(
    row: &LogRow,
    style: &CommitStyle,
    _editor: &Editor,
    area: Rect,
    surface: &mut Surface,
) {
    let Some(line) = row.line() else {
        return;
    };
    let mut y = area.y;
    for (i, (title, preview)) in line.hunks.iter().enumerate() {
        if i > 0 && y < area.bottom() {
            y += 1;
        }
        changes::draw_hunk(surface, area, &mut y, title, Some(preview), &style.preview);
    }
    if line.hunks.is_empty() && y < area.bottom() {
        let note = "No hunk of the first parent's diff touches these lines (a merge).";
        surface.set_stringn(area.x, y, note, area.width as usize, style.preview.quiet);
    }
}

/// Enter on a commit or a range of commits of the line history: the diff of the file over
/// `range`, from the first parent of its oldest commit (whose lines are `oldest`) to its newest
/// (`newest`), at the first hunk that touches the lines.
pub(crate) fn open_diff(
    cx: &mut compositor::Context,
    cwd: PathBuf,
    range: CommitRange,
    oldest: &LineRow,
    newest: &LineRow,
) {
    let change = oldest.change(newest);
    let lines = newest.lines.clone();
    cx.jobs.callback(async move {
        let call = move |editor: &mut Editor, compositor: &mut Compositor| {
            let commits = (&range.oldest, &range.newest);
            let opened = super::diff_view::open_lines_in_commits(
                editor, compositor, &cwd, commits, &change, &lines,
            );
            if let Err(err) = opened {
                editor.set_error(err.to_string());
            }
        };
        Ok(Callback::EditorCompositor(Box::new(call)))
    });
}

/// Enter on the uncommitted changes of the line history: the diff of the file against HEAD at
/// the first hunk that touches the lines.
fn open_uncommitted(cx: &mut compositor::Context, line: &LineRow) {
    let (path, lines) = (line.path.clone(), line.lines.clone());
    cx.jobs.callback(async move {
        let call = move |editor: &mut Editor, compositor: &mut Compositor| {
            super::diff_view::open_lines_uncommitted(editor, compositor, &path, &lines);
        };
        Ok(Callback::EditorCompositor(Box::new(call)))
    });
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let path = Path::new("/nowhere/src/main.rs");
        assert!(label(path, &[11..14]).ends_with("main.rs:12-14"));
        assert!(label(path, &[2..3, 11..14]).ends_with("main.rs:3,12-14"));
        assert_eq!(thousands(1240), "1,240");
        assert_eq!(thousands(50000), "50,000");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(commits(1), "1 commit");
        assert_eq!(commits(1240), "1,240 commits");
    }
}
