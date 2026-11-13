//! Watch mode (fork feature): follow the changes made to the working tree as they happen, while
//! an agent (or a formatter, a script, another editor) is at work.
//!
//! - `:watch` (or `vx --watch`) starts a session: the baseline is taken (see
//!   `vortex_view::watch`), the working tree is watched with the shared [`RepoWatch`] machinery
//!   (gitignore-aware, debounced, the open buffers reloaded with the stale-buffer rule) and every
//!   batch of changed files is diffed against the baseline on the watcher's thread.
//! - While following, a change opens its file (or switches to it), jumps to its first new hunk,
//!   centers it and flashes it. Moving pauses following: new changes are only counted. `n` to
//!   the newest change follows again.
//! - Any other action ends watch mode; the session's changes stay (gutter markers, `n`/`N`,
//!   `:watch-changes`) until the next watch or `:watch-clear`.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use tui::text::Span;
use vortex_core::Selection;
use vortex_event::register_hook;
use vortex_vcs::FileChange;
use vortex_view::{
    editor::{Action, ChangesConfig},
    events::DocumentDidOpen,
    graphics::{Color, Modifier, Style},
    watch::{Baseline, FileKind, FileUpdate, Flash, HunkLines, Quiet, Session, Target, Wrapped},
    Editor, Theme,
};

use crate::commands::changes::{self, ChangeRow, What};
use crate::compositor::{Compositor, Context};
use crate::job::Callback;
use crate::live::RepoWatch;
use crate::ui::{overlay::overlaid, picker};

/// How long the hunk following jumped to stays highlighted, fading out.
const FLASH: Duration = Duration::from_millis(900);
/// How often the flash is redrawn while it fades.
const FLASH_FRAME: Duration = Duration::from_millis(45);

static SESSIONS: AtomicU64 = AtomicU64::new(0);

fn trust_full(editor: &Editor, cwd: &Path) -> bool {
    editor
        .workspace_trust
        .query(
            &vortex_loader::find_workspace_in(cwd).0,
            vortex_loader::workspace_trust::TrustQuery::Git,
        )
        .is_trusted()
}

/// `~/code/app` for the status line.
fn display(path: &Path) -> String {
    vortex_stdx::path::fold_home_dir(path).display().to_string()
}

/// `:watch`: starts watch mode, or ends it when it is on.
pub fn toggle(editor: &mut Editor) -> anyhow::Result<()> {
    if editor.watch.is_active() {
        end(editor);
        Ok(())
    } else {
        start(editor)
    }
}

/// Starts a watch session in the repository of the working directory, dropping the previous
/// session's changes.
pub fn start(editor: &mut Editor) -> anyhow::Result<()> {
    let cwd = vortex_stdx::env::current_working_dir();
    let trust_full = trust_full(editor, &cwd);
    let state = editor
        .diff_providers
        .repo_state(&cwd, trust_full)
        .map_err(|_| anyhow::anyhow!("not in a git repository: nothing to watch"))?;
    // canonical, like the paths of the watcher's events and of the documents
    let work_dir = vortex_stdx::path::canonicalize(&state.work_dir);
    let git_dir = vortex_stdx::path::canonicalize(&state.git_dir);
    let canonical = |path: &Path| match path.strip_prefix(&state.work_dir) {
        Ok(relative) => work_dir.join(relative),
        Err(_) => vortex_stdx::path::canonicalize(path),
    };

    // the files already dirty: their content now is their baseline, so that earlier work
    // never counts as a change of the session
    let changes = editor
        .diff_providers
        .changed_files(&cwd, trust_full)
        .context("can't read the status of the repository")?;
    let mut dirty = HashMap::new();
    for change in &changes {
        let paths = match change {
            FileChange::Renamed { from_path, to_path } => vec![from_path.as_path(), to_path],
            change => vec![change.path()],
        };
        for path in paths {
            let path = canonical(path);
            let content = std::fs::read(&path).ok().map(Arc::from);
            dirty.insert(path, content);
        }
    }

    clear(editor);
    let baseline = Arc::new(Baseline::new(
        work_dir.clone(),
        git_dir,
        state.head,
        trust_full,
        editor.diff_providers.clone(),
        dirty,
    ));
    let id = SESSIONS.fetch_add(1, Ordering::Relaxed) + 1;
    let config = editor.config().changes.clone();
    let reader = baseline.clone();
    let tap: crate::live::Tap = Box::new(move |paths: &[PathBuf]| {
        let updates = read_updates(&reader, &config, paths);
        Box::new(move |editor: &mut Editor| apply(editor, id, updates))
    });
    let Some(watcher) = RepoWatch::start_tapped(editor, &work_dir, Some(tap)) else {
        bail!("can't watch {}", display(&work_dir));
    };
    let mut session = Session::new(id, baseline);
    session.watcher = Some(Box::new(watcher));
    editor.watch.session = Some(session);
    editor.set_status(format!(
        "Watching {}: following changes ({} already uncommitted, not counted)",
        display(&work_dir),
        changes.len()
    ));
    Ok(())
}

/// Ends watch mode: the working tree is no longer watched. The changes stay.
pub fn end(editor: &mut Editor) {
    let Some(session) = editor.watch.active() else {
        return;
    };
    session.end();
    let message = match session.change_count() {
        0 => "Watch ended: no changes".to_string(),
        count => format!(
            "Watch ended: n/N go through its {count} change{}, :watch-clear forgets them",
            if count == 1 { "" } else { "s" }
        ),
    };
    editor.set_status(message);
}

/// Forgets the session: its markers, and `n`/`N` search again.
pub fn clear(editor: &mut Editor) {
    if editor.watch.session.take().is_none() {
        return;
    }
    for doc in editor.documents_mut() {
        doc.clear_watch_base();
    }
}

/// The hash of some lines, or of a file's content.
fn hash(parts: &[&[&str]]) -> u64 {
    let mut hasher = DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// Diffs the changed files against their baseline. Runs on the watcher's thread.
fn read_updates(baseline: &Baseline, config: &ChangesConfig, paths: &[PathBuf]) -> Vec<FileUpdate> {
    let mut updates = Vec::new();
    for path in paths {
        if !baseline.covers(path) || path.is_dir() {
            continue;
        }
        let now = match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            // being written, or unreadable: the next event will tell
            Err(_) => continue,
        };
        let before = baseline.get(path);
        let kind = match (&before, &now) {
            (before, now) if before.as_deref() == now.as_deref() => FileKind::Unchanged,
            (None, Some(_)) => FileKind::Created,
            (Some(_), None) => FileKind::Deleted,
            _ => FileKind::Modified,
        };
        let texts = (
            changes::text(before.as_deref()),
            changes::text(now.as_deref()),
        );
        let quiet = if config.is_lockfile(path) {
            Quiet::Lockfile
        } else if texts.0.is_err() || texts.1.is_err() {
            Quiet::Binary
        } else {
            Quiet::No
        };
        let mut hunks = Vec::new();
        if let (Quiet::No, FileKind::Modified | FileKind::Created, Ok(old), Ok(new)) =
            (quiet, kind, texts.0, texts.1)
        {
            let (old, new) = (old.unwrap_or_default(), new.unwrap_or_default());
            let change = FileChange::Modified { path: path.clone() };
            let (old_lines, new_lines) = (lines(old), lines(new));
            let slice = |lines: &[&'_ str], range: &std::ops::Range<u32>| -> Vec<String> {
                let end = (range.end as usize).min(lines.len());
                let start = (range.start as usize).min(end);
                lines[start..end]
                    .iter()
                    .map(|line| line.to_string())
                    .collect()
            };
            for (line_hunk, _) in changes::hunks(&change, old, new) {
                let removed = slice(&old_lines, &line_hunk.before);
                let added = slice(&new_lines, &line_hunk.after);
                let removed: Vec<&str> = removed.iter().map(String::as_str).collect();
                let added: Vec<&str> = added.iter().map(String::as_str).collect();
                hunks.push(HunkLines {
                    key: hash(&[&removed, &added]),
                    before: line_hunk.before,
                    after: line_hunk.after,
                });
            }
        }
        let content = {
            let mut hasher = DefaultHasher::new();
            now.hash(&mut hasher);
            hasher.finish()
        };
        updates.push(FileUpdate {
            path: path.clone(),
            kind,
            quiet,
            hunks,
            content,
            modified: std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok(),
        });
    }
    updates
}

/// Takes in a batch of the session `id`, on the main thread, after the open buffers of the
/// changed files were reloaded: marks their buffers, and follows the newest change.
fn apply(editor: &mut Editor, id: u64, updates: Vec<FileUpdate>) {
    let Some(session) = editor.watch.active().filter(|session| session.id == id) else {
        return;
    };
    let applied = session.apply(updates);
    let baseline = session.baseline.clone();
    let following = session.following;
    for path in &applied.changed {
        if let Some(doc) = editor.document_by_path_mut(path) {
            if doc.watch_diff().is_none() {
                doc.set_watch_base(baseline.get(path).as_deref());
            }
        }
    }
    if !following {
        return;
    }
    // the newest file, but a buffer with unsaved edits of its own is never overwritten: its file
    // is skipped (the statusline warns that it changed on disk)
    let target = applied.to_follow.into_iter().rev().find(|target| {
        !editor
            .document_by_path(&target.path)
            .is_some_and(|doc| doc.is_modified())
    });
    if let Some(target) = target {
        go_to(editor, &target, Action::Replace);
        if let Some(session) = editor.watch.session.as_mut() {
            session.followed(&target);
        }
    }
    if let Some(session) = editor.watch.session.as_mut() {
        session.see_all();
    }
}

/// Opens the file of `target` (or switches to the split that shows it), puts the cursor on the
/// first line of the hunk, centers it and flashes it.
fn go_to(editor: &mut Editor, target: &Target, action: Action) {
    // the session keeps its own order of changes (`n`/`N`, `:watch-changes`): following and `n`
    // are not history's jumps (a pick in `:watch-changes` is, as any picker's)
    editor.without_history(|editor| go_to_unrecorded(editor, target, action));
}

fn go_to_unrecorded(editor: &mut Editor, target: &Target, action: Action) {
    let shown = editor.document_by_path(&target.path).and_then(|doc| {
        let doc_id = doc.id();
        editor
            .tree
            .views()
            .find(|(view, _)| view.doc == doc_id)
            .map(|(view, _)| view.id)
    });
    match shown {
        Some(view_id) if matches!(action, Action::Replace) => editor.focus(view_id),
        _ => {
            if let Err(err) = editor.open(&target.path, action) {
                editor.set_error(format!("unable to open {}: {err}", target.path.display()));
                return;
            }
        }
    }
    let (view, doc) = vortex_view::current!(editor);
    if doc.watch_diff().is_none() {
        if let Some(session) = editor.watch.session.as_ref() {
            doc.set_watch_base(session.baseline.get(&target.path).as_deref());
        }
    }
    let text = doc.text();
    let last = text.len_lines().saturating_sub(1);
    let first = (target.lines.start as usize).min(last);
    let end = (target.lines.end as usize).clamp(first + 1, last + 1);
    doc.set_selection(view.id, Selection::point(text.line_to_char(first)));
    vortex_view::align_view(doc, view, vortex_view::Align::Center);
    let doc_id = doc.id();
    if let Some(session) = editor.watch.session.as_mut() {
        session.flash = Some(Flash {
            doc: doc_id,
            lines: first..end,
            start: Instant::now(),
        });
        tokio::spawn(async {
            let frames = FLASH.as_millis() / FLASH_FRAME.as_millis() + 1;
            for _ in 0..frames {
                tokio::time::sleep(FLASH_FRAME).await;
                vortex_event::request_redraw();
            }
        });
    }
}

/// `n` (`forward`) or `N`: goes to the next or previous change of the session, in the order
/// they happened. `false` when `n`/`N` should search instead: there is no session, or it ended
/// and a search was made since, or it ended without changes.
pub(crate) fn jump(editor: &mut Editor, forward: bool) -> bool {
    let Some(session) = editor.watch.session.as_ref() else {
        return false;
    };
    if !session.active && session.search_owns_n {
        return false;
    }
    let (view, doc) = vortex_view::current_ref!(editor);
    let cursor = doc.path().map(|path| {
        let text = doc.text().slice(..);
        let line = doc.selection(view.id).primary().cursor_line(text) as u32;
        (path, line)
    });
    let Some((target, wrapped)) = session.step(cursor, forward) else {
        if session.active {
            editor.set_status("No changes yet this session");
            return true;
        }
        return false;
    };
    go_to(editor, &target, Action::Replace);
    let Some(session) = editor.watch.session.as_mut() else {
        return true;
    };
    if session.went_to(&target) {
        editor.set_status("Following changes again");
    } else {
        match wrapped {
            Wrapped::ToFirst => editor.set_status("Wrapped around to the first change"),
            Wrapped::ToLast => editor.set_status("Wrapped around to the last change"),
            Wrapped::No => {}
        }
    }
    true
}

/// What a command does to watch mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// Nothing: `Esc`, the command line, `n`/`N` (see [`jump`]), focusing the terminal panel.
    None,
    /// Moving (and selecting, scrolling, copying): following pauses.
    Move,
    /// Searching: following pauses; once watch mode ended, `n`/`N` repeat the search.
    Search,
    /// Anything else (editing, opening a file yourself, a picker...) ends watch mode.
    End,
}

/// The static commands that change nothing.
const NONE: &[&str] = &[
    "no_op",
    "normal_mode",
    "command_mode",
    "command_palette",
    "select_register",
    "search_next",
    "search_prev",
    "toggle_terminal",
    // setting or removing a bookmark (history and bookmarks) neither moves nor opens anything
    "toggle_bookmark",
];

/// The static commands that search.
const SEARCH: &[&str] = &[
    "search",
    "rsearch",
    "extend_search_next",
    "extend_search_prev",
    "search_selection",
    "search_selection_detect_word_boundaries",
    "make_search_word_bounded",
];

/// The static commands that move the cursor, the selections or the view, without opening anything
/// (besides the `move_`, `extend_`, `find_`, `till_`, `rotate_selections_` and `goto_` ones).
const MOVE: &[&str] = &[
    "repeat_last_motion",
    "page_up",
    "page_down",
    "half_page_up",
    "half_page_down",
    "page_cursor_up",
    "page_cursor_down",
    "page_cursor_half_up",
    "page_cursor_half_down",
    "select_all",
    "select_regex",
    "split_selection",
    "split_selection_on_newline",
    "merge_selections",
    "merge_consecutive_selections",
    "select_line_above",
    "select_line_below",
    "shrink_to_line_bounds",
    "collapse_selection",
    "flip_selections",
    "ensure_selections_forward",
    "select_mode",
    "exit_select_mode",
    "trim_selections",
    "keep_selections",
    "remove_selections",
    "keep_primary_selection",
    "remove_primary_selection",
    "copy_selection_on_next_line",
    "copy_selection_on_prev_line",
    "expand_selection",
    "shrink_selection",
    "select_next_sibling",
    "select_prev_sibling",
    "select_all_siblings",
    "select_all_children",
    "match_brackets",
    "select_textobject_around",
    "select_textobject_inner",
    "jump_view_right",
    "jump_view_left",
    "jump_view_up",
    "jump_view_down",
    "rotate_view",
    "rotate_view_reverse",
    "align_view_middle",
    "align_view_top",
    "align_view_center",
    "align_view_bottom",
    "scroll_up",
    "scroll_down",
    "yank",
    "yank_to_clipboard",
    "yank_to_primary_clipboard",
    "yank_joined",
    "yank_joined_to_clipboard",
    "yank_main_selection_to_clipboard",
    "yank_joined_to_primary_clipboard",
    "yank_main_selection_to_primary_clipboard",
    "goto_word",
    "extend_to_word",
];

/// The `goto_*` static commands that may open another buffer: they end watch mode. History
/// (`history_toggle`, `history_back`, `history_forward`) and the bookmarks' `next_bookmark`,
/// `prev_bookmark` and `bookmark_picker` end it too, as any command in no list does.
const OPENS: &[&str] = &[
    "goto_definition",
    "goto_declaration",
    "goto_type_definition",
    "goto_implementation",
    "goto_reference",
    "goto_file",
    "goto_file_hsplit",
    "goto_file_vsplit",
    "goto_last_accessed_file",
    "goto_last_modified_file",
    "goto_next_buffer",
    "goto_previous_buffer",
    "goto_next_tabstop",
    "goto_prev_tabstop",
];

/// What a static command does to watch mode, by name.
fn effect(name: &str) -> Effect {
    if NONE.contains(&name) {
        Effect::None
    } else if SEARCH.contains(&name) {
        Effect::Search
    } else if MOVE.contains(&name)
        || name.starts_with("move_") && !matches!(name, "move_lines_up" | "move_lines_down")
        || name.starts_with("extend_")
        || name.starts_with("find_")
        || name.starts_with("till_")
        || name.starts_with("rotate_selections_")
        || name.starts_with("goto_") && !OPENS.contains(&name)
    {
        Effect::Move
    } else {
        Effect::End
    }
}

fn take_effect(editor: &mut Editor, effect: Effect) {
    let Some(session) = editor.watch.session.as_mut() else {
        return;
    };
    match effect {
        Effect::None => {}
        Effect::Move if session.active => session.pause(),
        Effect::Search if session.active => session.pause(),
        Effect::Search => session.search_owns_n = true,
        Effect::End if session.active => end(editor),
        Effect::Move | Effect::End => {}
    }
}

/// Called before a static command runs.
pub(crate) fn before_command(editor: &mut Editor, name: &str) {
    if editor.watch.session.is_some() {
        take_effect(editor, effect(name));
    }
}

/// Called before a typed command (`:...`) runs.
pub(crate) fn before_typed(editor: &mut Editor, name: &str) {
    if editor.watch.session.is_none() {
        return;
    }
    let effect = match name {
        "watch" | "watch-stop" | "watch-clear" | "watch-changes" => Effect::None,
        "goto" => Effect::Move,
        _ => Effect::End,
    };
    take_effect(editor, effect);
}

/// The mouse moved the cursor or scrolled: following pauses.
pub(crate) fn moved(editor: &mut Editor) {
    if let Some(session) = editor.watch.active() {
        session.pause();
    }
}

/// The statusline element: `WATCH following`, `WATCH 3 new`, `WATCH paused`; nothing outside
/// watch mode.
pub(crate) fn statusline(editor: &Editor) -> Option<Span<'static>> {
    let session = editor.watch.session.as_ref().filter(|s| s.active)?;
    let theme = &editor.theme;
    let style = |scope: &str, fallback: Color| {
        theme
            .try_get_exact(scope)
            .or_else(|| theme.try_get_exact("ui.statusline.watch"))
            .unwrap_or_else(|| {
                Style::default()
                    .fg(Color::Black)
                    .bg(fallback)
                    .add_modifier(Modifier::BOLD)
            })
    };
    let span = if session.following {
        Span::styled(
            " WATCH following ",
            style("ui.statusline.watch.following", Color::Green),
        )
    } else {
        let text = match session.new_count() {
            0 => " WATCH paused ".to_string(),
            new => format!(" WATCH {new} new "),
        };
        Span::styled(text, style("ui.statusline.watch.paused", Color::Yellow))
    };
    Some(span)
}

/// The style of the flash on line `line` of `doc`, fading out; `None` when it isn't flashing.
pub(crate) fn flash_style(
    editor: &Editor,
    doc: vortex_view::DocumentId,
) -> Option<(std::ops::Range<usize>, Style)> {
    let flash = editor.watch.session.as_ref()?.flash.as_ref()?;
    if flash.doc != doc {
        return None;
    }
    let elapsed = flash.start.elapsed();
    if elapsed >= FLASH {
        return None;
    }
    let theme: &Theme = &editor.theme;
    let color = theme.get_or("ui.highlight.watch", "ui.selection").bg;
    let background = theme.get("ui.background").bg;
    let left = 1.0 - elapsed.as_secs_f32() / FLASH.as_secs_f32();
    let bg = match crate::ui::diff_view::blend(background, color, left) {
        Some(color) => Some(color),
        // without RGB colors to blend, the flash shows whole, then not at all
        None => color.filter(|_| left > 0.4),
    }?;
    Some((flash.lines.clone(), Style::default().bg(bg)))
}

/// Sets the baseline of the documents opened while their file is a change of the session, for
/// their gutter markers.
pub(crate) fn register_hooks() {
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        let editor = &mut *event.editor;
        let Some(session) = editor.watch.session.as_ref() else {
            return Ok(());
        };
        let baseline = session.baseline.clone();
        let Some(doc) = editor.documents.get_mut(&event.doc) else {
            return Ok(());
        };
        let Some(path) = doc.path() else {
            return Ok(());
        };
        if session.file(path).is_some() && doc.watch_diff().is_none() {
            let base = baseline.get(path);
            doc.set_watch_base(base.as_deref());
        }
        Ok(())
    });
}

/// The rows of the session's changes: the most recently changed file first, and in a file its
/// newest hunk first.
fn session_rows(editor: &Editor, session: &Session) -> Vec<ChangeRow> {
    let config = &editor.config().changes;
    let mut rows = Vec::new();
    for (path, file) in session.files_newest_first() {
        let before = session.baseline.get(path);
        let now = match file.kind {
            FileKind::Deleted => None,
            _ => std::fs::read(path).ok(),
        };
        let change = match file.kind {
            FileKind::Created => FileChange::Added { path: path.clone() },
            FileKind::Deleted => FileChange::Deleted { path: path.clone() },
            _ => FileChange::Modified { path: path.clone() },
        };
        let mut group = changes::commit_group(&change, before.as_deref(), now.as_deref(), config);
        let seq = |row: &ChangeRow| match &row.what {
            What::Hunk(hunk) => file
                .hunks
                .iter()
                .find(|session_hunk| session_hunk.after.start == hunk.lines.start)
                .map_or(0, |session_hunk| session_hunk.seq),
            What::File(_) => u64::MAX,
        };
        // the divider stays first
        group.sort_by_key(|row| std::cmp::Reverse(seq(row)));
        rows.extend(group);
    }
    rows
}

/// `:watch-changes`: the picker of the session's changes, one row per hunk under a divider per
/// file, the newest first. Enter goes to the change.
pub fn open_changes(editor: &mut Editor, compositor: &mut Compositor) {
    let Some(session) = editor.watch.session.as_mut() else {
        editor.set_status("No watch session: :watch starts one");
        return;
    };
    if session.active {
        session.pause();
    }
    let session = editor.watch.session.as_ref().unwrap();
    if session.is_empty() {
        editor.set_status("No changes this session");
        return;
    }
    let rows = session_rows(editor, session);
    let totals = changes::Totals::of(&rows);
    editor.set_status(format!("This session: {}", totals.describe()));
    let cwd = vortex_stdx::env::current_working_dir();
    let enter = |cx: &mut Context, row: &ChangeRow| {
        if !open_row(cx.editor, row, Action::Replace) {
            return;
        }
        cx.jobs.callback(async move {
            let call = |_: &mut Editor, compositor: &mut Compositor| {
                compositor.remove(picker::ID);
            };
            Ok(Callback::EditorCompositor(Box::new(call)))
        });
    };
    let open = |cx: &mut Context, row: &ChangeRow, action: Action| {
        open_row(cx.editor, row, action);
    };
    let picker = changes::picker(editor, cwd, rows, false, enter, open);
    compositor.push(Box::new(overlaid(picker)));
}

/// Goes to the change of `row`. `false` when it can't (a deleted file).
fn open_row(editor: &mut Editor, row: &ChangeRow, action: Action) -> bool {
    let path = row.change.path().to_path_buf();
    if matches!(row.change, FileChange::Deleted { .. }) {
        editor.set_error(format!("{} was deleted", display(&path)));
        return false;
    }
    let lines = match &row.what {
        What::Hunk(hunk) => hunk.lines.clone(),
        What::File(_) => row.line()..row.line() + 1,
    };
    let seq = editor.watch.session.as_ref().and_then(|session| {
        let file = session.file(&path)?;
        file.hunks
            .iter()
            .find(|hunk| hunk.after.start == lines.start)
            .map(|hunk| hunk.seq)
    });
    let target = Target {
        path,
        seq: seq.unwrap_or(0),
        lines,
    };
    go_to(editor, &target, action);
    if seq.is_some() {
        if let Some(session) = editor.watch.session.as_mut() {
            session.went_to(&target);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects() {
        assert_eq!(effect("move_char_left"), Effect::Move);
        assert_eq!(effect("goto_line_start"), Effect::Move);
        assert_eq!(effect("goto_file"), Effect::End);
        assert_eq!(effect("goto_next_buffer"), Effect::End);
        assert_eq!(effect("move_lines_up"), Effect::End);
        assert_eq!(effect("extend_line_below"), Effect::Move);
        assert_eq!(effect("search"), Effect::Search);
        assert_eq!(effect("search_next"), Effect::None);
        assert_eq!(effect("insert_mode"), Effect::End);
        assert_eq!(effect("file_picker"), Effect::End);
        assert_eq!(effect("delete_selection"), Effect::End);
        assert_eq!(effect("toggle_terminal"), Effect::None);
        // history and bookmarks
        assert_eq!(effect("toggle_bookmark"), Effect::None);
        for name in [
            "history_toggle",
            "history_back",
            "history_forward",
            "next_bookmark",
            "prev_bookmark",
            "bookmark_picker",
            "goto_last_accessed_file",
        ] {
            assert_eq!(effect(name), Effect::End, "{name}");
        }
    }

    /// A renamed or removed command left in a list would silently fall to "ends watch mode".
    #[test]
    fn every_listed_command_is_a_static_command() {
        let names: Vec<&str> = crate::commands::MappableCommand::STATIC_COMMAND_LIST
            .iter()
            .map(|command| command.name())
            .collect();
        for (list, commands) in [
            ("NONE", NONE),
            ("SEARCH", SEARCH),
            ("MOVE", MOVE),
            ("OPENS", OPENS),
        ] {
            for command in commands {
                assert!(
                    names.contains(command),
                    "{command} in {list} is not a static command"
                );
            }
        }
    }
}
