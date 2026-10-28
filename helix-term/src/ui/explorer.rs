//! The file tree, an overlay that looks and works like the file picker (`space f`): a prompt on
//! top, the rows of the tree on the left and a preview of the selected file or directory on the
//! right. It is built on [`Picker`], which draws the overlay, the prompt and the preview.
//!
//! The tree computes the rows itself (directories first, then files, by name regardless of case)
//! and hands them to the picker in that order, which doesn't filter or reorder them
//! ([`Picker::with_external_filter`]). Files ignored by git are dimmed (or hidden with
//! `editor.explorer.hide-gitignored`), files changed in git are marked (see [`git_status`]), and
//! the tree can create, rename, move (cut and paste) and delete files.
//!
//! Helix pickers send the letters typed to their prompt, but the tree needs them as keys, so it has
//! two modes. In tree mode the keys move in the tree and act on its entries. `/` enters filter
//! mode, where the keys go to the prompt of the picker and fuzzy filter the whole workspace (see
//! [`filter`]); `Esc` goes back to tree mode and keeps the filter.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::bail;
use helix_core::movement::Direction;
use helix_core::Position;
use helix_view::editor::Action;
use helix_view::graphics::{CursorKind, Rect};
use helix_view::input::KeyEvent;
use helix_view::{doc, DocumentId, Editor};
use tui::buffer::Buffer as Surface;
use tui::widgets::Cell;

use crate::compositor::{self, Component, Compositor, Context, Event, EventResult};
use crate::job::Callback;
use crate::ui::editor::canonicalize_key;
use crate::ui::overlay::{overlaid, Overlay};
use crate::ui::{self, Picker, PickerColumn, Prompt, PromptEvent};
use crate::{alt, commands, ctrl, key, shift};

mod filter;
mod git_status;
mod gitignore;
mod row;
mod tree;

use filter::{Found, Hit, Walk};
use git_status::GitStatus;
use row::{Entry, Look, Row};
use tree::Tree;

pub const ID: &str = "explorer";

/// Shown in the status line when the tree opens.
const HINT: &str = "/ filter, a add, r rename, x/p move, d del, ? help";

const HELP: &str = "/: filter  l/h: expand/collapse  enter: open  a: new (end with / for a \
                    directory)  r: rename  x: cut  p: paste  d: delete  R: refresh  \
                    H: show/hide git ignored  q: close";

/// Where the keys go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    /// The keys move in the tree and act on its entries.
    Tree,
    /// The keys edit the query of the picker, which filters the tree.
    Filter,
}

/// What `rebuild` selects once the rows have changed.
enum Select {
    /// The entry that was selected, or the row at the same place if it is gone.
    Keep,
    Path(PathBuf),
    /// The entry that matches the filter best.
    Best,
}

pub struct ExplorerPicker {
    picker: Picker<Row, ()>,
    tree: Tree,
    /// The rows of the picker.
    entries: Vec<Entry>,
    git: GitStatus,
    focus: Focus,
    /// The query the entries were filtered with.
    query: String,
    /// The walk of the workspace that filtering needs, started with the first query.
    walk: Option<Walk>,
    /// What the walk found, kept until the tree is read again.
    found: Option<Vec<Found>>,
    /// What matches the query, when `found` is there.
    hits: Vec<Hit>,
    /// Directories folded in the filtered tree, whose hits are hidden.
    collapsed: HashSet<PathBuf>,
    /// `g` was pressed, the next key completes the command (`gg`, `ge`).
    goto: bool,
    /// Path marked with `x`, moved by `p`.
    cut: Option<PathBuf>,
    /// `H` flips `editor.explorer.hide-gitignored` for this session.
    flip_hidden: bool,
    /// Path waiting for the `y` that confirms its deletion.
    confirm_delete: Option<PathBuf>,
}

/// Opens the tree on the workspace root. With `reveal`, the file of the current buffer is
/// expanded to and selected.
pub fn open(cx: &mut commands::Context, reveal: bool) {
    let explorer = ExplorerPicker::new(cx.editor, reveal);
    cx.editor.set_status(HINT);
    cx.push_layer(Box::new(overlaid(explorer)));
}

/// The directory the tree is rooted at: the workspace of the working directory, like the
/// `file_explorer` picker.
fn workspace_root() -> PathBuf {
    helix_loader::find_workspace().0
}

impl ExplorerPicker {
    pub fn new(editor: &Editor, reveal: bool) -> Self {
        let columns = [PickerColumn::new("tree", |row: &Row, _: &()| {
            Cell::from(row.line())
        })];
        let picker = Picker::new(columns, 0, Vec::new(), (), |_, _, _| {})
            .with_preview(|_editor, row| Some((row.path.as_path().into(), None)))
            .with_external_filter()
            .with_lookup(find_picker)
            // The indentation matters more than the start of the name.
            .truncate_start(false);
        let mut explorer = Self {
            picker,
            tree: Tree::new(workspace_root()),
            entries: Vec::new(),
            git: GitStatus::default(),
            focus: Focus::Tree,
            query: String::new(),
            walk: None,
            found: None,
            hits: Vec::new(),
            collapsed: HashSet::new(),
            goto: false,
            cut: None,
            flip_hidden: false,
            confirm_delete: None,
        };
        explorer.git.request(&explorer.tree.root, editor);
        let current = doc!(editor).path().filter(|_| reveal);
        if let Some(path) = current {
            explorer.tree.reveal(path, true);
        }
        let select = current.map_or(Select::Keep, |path| Select::Path(path.to_path_buf()));
        explorer.rebuild(editor, select);
        explorer
    }

    fn hide_ignored(&self, editor: &Editor) -> bool {
        editor.config().explorer.hide_gitignored != self.flip_hidden
    }

    /// Whether the entries are the hits of the query, not the tree.
    fn filtering(&self) -> bool {
        !self.query.trim().is_empty() && self.found.is_some()
    }

    fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.picker.selected_index() as usize)
    }

    fn position(&self, path: &Path) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.node.path == path)
    }

    /// The entries to show: the hits of the query when filtering, the tree otherwise. With the
    /// index of the best hit.
    fn compute(&mut self, editor: &Editor) -> (Vec<Entry>, Option<usize>) {
        if self.filtering() {
            return filter::tree(&self.tree.root, &self.hits, &self.collapsed);
        }
        let nodes = self.tree.nodes(self.hide_ignored(editor));
        let entries = nodes
            .into_iter()
            .map(|node| {
                let expanded = node.is_dir && self.tree.expanded.contains(&node.path);
                Entry::new(node, expanded)
            })
            .collect();
        (entries, None)
    }

    /// Computes the entries again and gives them to the picker.
    fn rebuild(&mut self, editor: &Editor, select: Select) {
        let previous = self.selected().map(|entry| entry.node.path.clone());
        let index = self.picker.selected_index() as usize;
        let (entries, best) = self.compute(editor);
        self.entries = entries;
        let selected = match select {
            Select::Best => best,
            Select::Path(path) => self.position(&path),
            Select::Keep => None,
        }
        .or_else(|| previous.and_then(|path| self.position(&path)))
        .unwrap_or(index);

        let look = Look::new(&editor.theme);
        let current = doc!(editor).path();
        let show_git = editor.config().explorer.git_status;
        let rows: Vec<Row> = self
            .entries
            .iter()
            .map(|entry| {
                let path = &entry.node.path;
                let mark = show_git.then(|| self.git.get(path)).flatten();
                let cut = self.cut.as_ref() == Some(path);
                Row::new(entry, mark, current == Some(path.as_path()), cut, &look)
            })
            .collect();
        self.picker.replace_items(rows, selected as u32);
    }

    /// Catches up with what happened in the background since the last render: the git status and
    /// the walk of the workspace.
    fn update(&mut self, editor: &Editor) {
        let git_changed = self.git.update(&self.tree.root, editor);
        if let Some(found) = self.walk.as_ref().and_then(Walk::take) {
            self.found = Some(found);
            self.refilter();
            self.rebuild(editor, Select::Best);
        } else if git_changed {
            self.rebuild(editor, Select::Keep);
        }
    }

    /// Matches the query against what the walk found.
    fn refilter(&mut self) {
        self.hits = match &self.found {
            Some(found) if !self.query.trim().is_empty() => filter::search(found, &self.query),
            _ => Vec::new(),
        };
        self.collapsed.clear();
    }

    /// Reacts to an edit of the query of the picker.
    fn query_changed(&mut self, editor: &Editor) {
        if self.picker.query_line() == self.query {
            return;
        }
        self.query = self.picker.query_line().to_string();
        if self.query.trim().is_empty() {
            self.leave_filter(editor);
            return;
        }
        if self.walk.is_none() {
            self.walk = Some(Walk::start(&self.tree.root));
        }
        self.refilter();
        self.rebuild(editor, Select::Best);
    }

    /// Back to the tree: the selected entry is shown in it, with its directories expanded.
    fn leave_filter(&mut self, editor: &Editor) {
        let selected = self.selected().map(|entry| entry.node.path.clone());
        self.hits.clear();
        self.collapsed.clear();
        if let Some(path) = &selected {
            self.tree.reveal(path, true);
        }
        self.rebuild(editor, selected.map_or(Select::Keep, Select::Path));
    }

    /// Empties the query.
    fn clear_query(&mut self, editor: &Editor) {
        self.picker.set_query_line(String::new(), editor);
        self.query_changed(editor);
        self.focus = Focus::Tree;
    }

    /// The walk is out of date: it starts again with the next query, or now if there is one.
    fn forget_walk(&mut self) {
        self.walk = None;
        self.found = None;
        self.hits.clear();
        if !self.query.trim().is_empty() {
            self.walk = Some(Walk::start(&self.tree.root));
        }
    }

    /// Reads the tree from disk again after a change, keeping the expanded directories and the
    /// selection.
    fn refresh(&mut self, editor: &Editor) {
        self.tree.refresh();
        self.forget_walk();
        self.rebuild(editor, Select::Keep);
        self.git.request(&self.tree.root, editor);
    }

    /// Reads every directory again, for the changes `refresh` can't see (global git excludes,
    /// coarse modification times).
    fn reload(&mut self, editor: &Editor) {
        self.tree.reload();
        self.refresh(editor);
    }

    /// Shows `path` after it was created or moved (from `moved_from`): the filter is dropped, so
    /// that it can't hide it.
    fn changed(&mut self, editor: &Editor, path: &Path, moved_from: Option<&Path>) {
        if let Some(old) = moved_from {
            follow_rename(&mut self.cut, old, path);
        }
        self.tree.refresh();
        self.forget_walk();
        self.git.request(&self.tree.root, editor);
        self.clear_query(editor);
        self.tree.reveal(path, true);
        self.rebuild(editor, Select::Path(path.to_path_buf()));
    }

    /// Directory that new and pasted files go to: the selected directory, or the parent of the
    /// selected file.
    fn target_dir(&self) -> PathBuf {
        match self.selected() {
            Some(entry) if entry.node.is_dir => entry.node.path.clone(),
            Some(entry) => entry
                .node
                .path
                .parent()
                .unwrap_or(&self.tree.root)
                .to_path_buf(),
            None => self.tree.root.clone(),
        }
    }

    fn relative<'a>(&self, path: &'a Path) -> std::borrow::Cow<'a, str> {
        path.strip_prefix(&self.tree.root)
            .unwrap_or(path)
            .to_string_lossy()
    }

    /// Expands or collapses the selected directory.
    fn toggle(&mut self, editor: &Editor) {
        let Some(entry) = self.selected() else {
            return;
        };
        let path = entry.node.path.clone();
        let expanded = entry.expanded;
        if self.filtering() {
            if expanded {
                self.collapsed.insert(path);
            } else {
                self.collapsed.remove(&path);
            }
        } else if !self.tree.expanded.remove(&path) {
            self.tree.expanded.insert(path);
        }
        self.rebuild(editor, Select::Keep);
    }

    /// Opens the selected file, or expands/collapses the selected directory.
    fn activate(&mut self, editor: &mut Editor) -> EventResult {
        match self.selected() {
            Some(entry) if entry.node.is_dir => {
                self.toggle(editor);
                EventResult::Consumed(None)
            }
            Some(_) => self.open_selected(editor, Action::Replace, true),
            None => EventResult::Consumed(None),
        }
    }

    /// Opens the selected file, and closes the tree when `close`.
    fn open_selected(&mut self, editor: &mut Editor, action: Action, close: bool) -> EventResult {
        let Some(entry) = self.selected().filter(|entry| !entry.node.is_dir) else {
            return EventResult::Consumed(None);
        };
        let path = entry.node.path.clone();
        match editor.open(&path, action) {
            Ok(_) if close => close_callback(),
            Ok(_) => EventResult::Consumed(None),
            Err(err) => {
                editor.set_error(format!("Failed to open {}: {err}", path.display()));
                EventResult::Consumed(None)
            }
        }
    }

    fn expand(&mut self, editor: &mut Editor) -> EventResult {
        match self.selected() {
            Some(entry) if entry.node.is_dir && entry.expanded => {
                self.picker.move_by(1, Direction::Forward);
                EventResult::Consumed(None)
            }
            Some(_) => self.activate(editor),
            None => EventResult::Consumed(None),
        }
    }

    /// Collapses the selected directory, or goes to the parent directory.
    fn collapse(&mut self, editor: &Editor) {
        let Some(entry) = self.selected() else {
            return;
        };
        if entry.node.is_dir && entry.expanded {
            self.toggle(editor);
        } else if let Some(index) = entry.node.path.parent().and_then(|dir| self.position(dir)) {
            self.picker.set_selected_index(index as u32);
        }
    }

    fn handle_key(&mut self, mut key: KeyEvent, cx: &mut Context) -> EventResult {
        canonicalize_key(&mut key);
        match self.focus {
            Focus::Tree => self.tree_key(key, cx),
            Focus::Filter => self.filter_key(key, cx),
        }
    }

    /// A key in filter mode: the ones that act on the tree, the others are for the picker (the
    /// prompt, and the keys that move in the list).
    fn filter_key(&mut self, key: KeyEvent, cx: &mut Context) -> EventResult {
        match key {
            key!(Esc) => self.focus = Focus::Tree,
            key!(Enter) => return self.activate(cx.editor),
            alt!(Enter) => return self.open_selected(cx.editor, Action::Replace, false),
            ctrl!('s') => return self.open_selected(cx.editor, Action::HorizontalSplit, true),
            ctrl!('v') => return self.open_selected(cx.editor, Action::VerticalSplit, true),
            ctrl!('c') => return close_callback(),
            _ => {
                self.picker.handle_event(&Event::Key(key), cx);
                self.query_changed(cx.editor);
            }
        }
        EventResult::Consumed(None)
    }

    fn tree_key(&mut self, key: KeyEvent, cx: &mut Context) -> EventResult {
        if let Some(path) = self.confirm_delete.take() {
            if key == key!('y') {
                if let Err(err) = self.delete(&path, cx.editor) {
                    cx.editor.set_error(format!("Failed to delete: {err}"));
                }
            } else {
                cx.editor.set_status("Delete cancelled");
            }
            return EventResult::Consumed(None);
        }
        if std::mem::take(&mut self.goto) {
            match key {
                key!('g') => self.picker.to_start(),
                key!('e') => self.picker.to_end(),
                _ => {}
            }
            return EventResult::Consumed(None);
        }

        match key {
            key!('j') => self.picker.move_by(1, Direction::Forward),
            key!('k') => self.picker.move_by(1, Direction::Backward),
            key!('g') => self.goto = true,
            key!('G') => self.picker.to_end(),
            // Moving and the preview are the picker's.
            key!(Up)
            | key!(Down)
            | key!(Tab)
            | shift!(Tab)
            | ctrl!('n')
            | ctrl!('p')
            | key!(PageUp)
            | key!(PageDown)
            | ctrl!('u')
            | ctrl!('d')
            | key!(Home)
            | key!(End)
            | ctrl!('t') => {
                self.picker.handle_event(&Event::Key(key), cx);
            }
            key!('l') | key!(Right) => return self.expand(cx.editor),
            key!('h') | key!(Left) => self.collapse(cx.editor),
            key!(Enter) => return self.activate(cx.editor),
            alt!(Enter) => return self.open_selected(cx.editor, Action::Replace, false),
            ctrl!('s') => return self.open_selected(cx.editor, Action::HorizontalSplit, true),
            ctrl!('v') => return self.open_selected(cx.editor, Action::VerticalSplit, true),
            key!('/') => self.focus = Focus::Filter,
            key!('a') => return self.prompt_create(),
            key!('r') => return self.prompt_rename(),
            key!('x') => {
                if let Some(entry) = self.selected() {
                    cx.editor.set_status(format!(
                        "Cut {}, press p on the destination to move it",
                        entry.node.name
                    ));
                    self.cut = Some(entry.node.path.clone());
                    self.rebuild(cx.editor, Select::Keep);
                }
            }
            key!('p') => {
                if let Err(err) = self.paste(cx.editor) {
                    cx.editor.set_error(format!("Failed to move: {err}"));
                }
            }
            key!('d') => {
                if let Some(entry) = self.selected() {
                    cx.editor
                        .set_status(format!("Delete {}? (y/n)", self.relative(&entry.node.path)));
                    self.confirm_delete = Some(entry.node.path.clone());
                }
            }
            key!('R') => {
                self.reload(cx.editor);
                cx.editor.set_status("Explorer refreshed");
            }
            key!('H') => {
                self.flip_hidden = !self.flip_hidden;
                self.rebuild(cx.editor, Select::Keep);
            }
            key!('?') => cx.editor.set_status(HELP),
            key!(Esc) if !self.query.is_empty() => self.clear_query(cx.editor),
            key!(Esc) | key!('q') | ctrl!('c') => return close_callback(),
            _ => {}
        }
        EventResult::Consumed(None)
    }

    fn prompt_create(&self) -> EventResult {
        let dir = self.target_dir();
        let shown = match self.relative(&dir) {
            relative if relative.is_empty() => String::new(),
            relative => format!("{relative}/"),
        };
        let prompt = Prompt::new(
            format!("new: {shown}").into(),
            None,
            ui::completers::none,
            move |cx, input, event| {
                let input = input.trim();
                if event != PromptEvent::Validate || input.is_empty() {
                    return;
                }
                let is_dir = input.ends_with('/');
                let path = helix_stdx::path::normalize(dir.join(input.trim_end_matches('/')));
                // Not `exists`, which follows symlinks: writing to a broken one would create its
                // target, wherever it points.
                if path.symlink_metadata().is_ok() {
                    cx.editor
                        .set_error(format!("{} already exists", path.display()));
                    return;
                }
                match cx.editor.create_path(&path, is_dir) {
                    Ok(()) => changed_later(cx, path, None),
                    Err(err) => cx.editor.set_error(format!("Failed to create: {err}")),
                }
            },
        );
        push_callback(prompt)
    }

    fn prompt_rename(&self) -> EventResult {
        let Some(entry) = self.selected() else {
            return EventResult::Consumed(None);
        };
        let old = entry.node.path.clone();
        let name = entry.node.name.clone();
        let prompt = Prompt::new(
            "rename: ".into(),
            None,
            ui::completers::none,
            move |cx, input, event| {
                let input = input.trim();
                if event != PromptEvent::Validate || input.is_empty() {
                    return;
                }
                let new = old.parent().unwrap_or(Path::new("")).join(input);
                match move_entry(cx.editor, &old, &new) {
                    Ok(new) => changed_later(cx, new, Some(old.clone())),
                    Err(err) => cx.editor.set_error(format!("Failed to rename: {err}")),
                }
            },
        );
        // The line is set when the prompt is pushed, which has an editor at hand.
        EventResult::Consumed(Some(Box::new(move |compositor, cx| {
            compositor.push(Box::new(prompt.with_line(name, cx.editor)));
        })))
    }

    fn paste(&mut self, editor: &mut Editor) -> anyhow::Result<()> {
        let Some(cut) = self.cut.clone() else {
            bail!("nothing to paste, mark a file with x first");
        };
        let Some(name) = cut.file_name() else {
            bail!("can't move {}", cut.display());
        };
        let new = match move_entry(editor, &cut, &self.target_dir().join(name)) {
            Ok(new) => new,
            Err(err) => {
                if cut.symlink_metadata().is_err() {
                    self.cut = None;
                }
                return Err(err);
            }
        };
        self.cut = None;
        self.changed(editor, &new, None);
        Ok(())
    }

    fn delete(&mut self, path: &Path, editor: &mut Editor) -> anyhow::Result<()> {
        let documents: Vec<DocumentId> = editor
            .documents()
            .filter(|doc| {
                doc.path()
                    .is_some_and(|doc_path| doc_path.starts_with(path))
            })
            .map(|doc| doc.id())
            .collect();
        if let Some(doc) = documents
            .iter()
            .filter_map(|id| editor.document(*id))
            .find(|doc| doc.is_modified())
        {
            bail!("{} has unsaved changes", doc.display_name());
        }
        // Delete first: if that fails, the buffers stay open.
        editor.delete_path(path, true)?;
        if self.cut.as_ref().is_some_and(|cut| cut.starts_with(path)) {
            self.cut = None;
        }
        self.refresh(editor);
        let closed = documents.into_iter().fold(true, |closed, id| {
            editor.close_document(id, true).is_ok() && closed
        });
        if !closed {
            bail!("deleted, but couldn't close its buffers");
        }
        editor.set_status(format!("Deleted {}", self.relative(path)));
        Ok(())
    }
}

impl Component for ExplorerPicker {
    fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        match event {
            Event::Key(key) => self.handle_key(*key, cx),
            Event::Paste(_) if self.focus == Focus::Filter => {
                self.picker.handle_event(event, cx);
                self.query_changed(cx.editor);
                EventResult::Consumed(None)
            }
            // There is no prompt to paste into in tree mode.
            Event::Paste(_) => EventResult::Consumed(None),
            _ => self.picker.handle_event(event, cx),
        }
    }

    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        self.update(cx.editor);
        self.picker.render(area, surface, cx);
    }

    /// The cursor is in the prompt in filter mode only.
    fn cursor(&self, area: Rect, editor: &Editor) -> (Option<Position>, CursorKind) {
        match self.focus {
            Focus::Filter => self.picker.cursor(area, editor),
            Focus::Tree => (None, CursorKind::Hidden),
        }
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        self.picker.required_size(viewport)
    }

    fn id(&self) -> Option<&'static str> {
        Some(ID)
    }
}

/// Keeps the cut path pointing to the same entry after `old` was renamed to `new`.
fn follow_rename(cut: &mut Option<PathBuf>, old: &Path, new: &Path) {
    if let Some(cut) = cut {
        if let Ok(relative) = cut.strip_prefix(old) {
            *cut = new.join(relative);
        }
    }
}

/// Finds the picker of the tree in the compositor.
fn find_picker(compositor: &mut Compositor) -> Option<&mut Picker<Row, ()>> {
    let overlay = compositor.find_id::<Overlay<ExplorerPicker>>(ID)?;
    Some(&mut overlay.content.picker)
}

fn close_callback() -> EventResult {
    EventResult::Consumed(Some(Box::new(|compositor: &mut Compositor, _| {
        compositor.remove(ID);
    })))
}

fn push_callback(prompt: Prompt) -> EventResult {
    EventResult::Consumed(Some(Box::new(move |compositor, _| {
        compositor.push(Box::new(prompt));
    })))
}

/// Shows `path` in the tree once the prompt that created or moved it has closed. When `path` was
/// renamed from `moved_from`, a cut path inside it follows.
fn changed_later(cx: &mut compositor::Context, path: PathBuf, moved_from: Option<PathBuf>) {
    cx.jobs.callback(async move {
        Ok(Callback::EditorCompositor(Box::new(
            move |editor, compositor| {
                let explorer = compositor.find_id::<Overlay<ExplorerPicker>>(ID);
                if let Some(explorer) = explorer {
                    explorer
                        .content
                        .changed(editor, &path, moved_from.as_deref());
                }
            },
        )))
    });
}

/// Moves or renames `old` to `new`, creating the missing parent directories, and returns where it
/// went: `new`, normalized. Open buffers follow the file, including the ones inside a moved
/// directory.
fn move_entry(editor: &mut Editor, old: &Path, new: &Path) -> anyhow::Result<PathBuf> {
    // A rename to `../name` would otherwise leave `..` in the paths of the buffers below.
    let new = helix_stdx::path::normalize(new);
    if old == new {
        return Ok(new);
    }
    // `Editor::move_path` quietly does nothing when `old` is gone.
    if old.symlink_metadata().is_err() {
        bail!("{} no longer exists", old.display());
    }
    if new.symlink_metadata().is_ok() && !same_entry(old, &new) {
        bail!("{} already exists", new.display());
    }
    if new.starts_with(old) {
        bail!("can't move a directory into itself");
    }
    // Missing parent directories, deepest first, removed again if the move fails.
    let created: Vec<PathBuf> = new
        .ancestors()
        .skip(1)
        .take_while(|dir| !dir.exists())
        .map(Path::to_path_buf)
        .collect();
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // `Editor::move_path` only updates the buffer of `old` itself.
    let inside: Vec<(DocumentId, PathBuf)> = if old.is_dir() {
        editor
            .documents()
            .filter_map(|doc| {
                let relative = doc.path()?.strip_prefix(old).ok()?;
                Some((doc.id(), new.join(relative)))
            })
            .collect()
    } else {
        Vec::new()
    };
    // `Editor::move_path` checks that `old` exists with `exists`, which follows symlinks, and skips
    // the rename of a broken one.
    let moved = if old.exists() {
        editor.move_path(old, &new)
    } else {
        std::fs::rename(old, &new)
    };
    if let Err(err) = moved {
        for dir in created {
            let _ = std::fs::remove_dir(dir);
        }
        return Err(err.into());
    }
    for (id, path) in inside {
        editor.set_doc_path(id, &path);
    }
    Ok(new)
}

/// Whether `new`, found on disk, is `old` itself under another case: on a case-insensitive
/// filesystem, renaming `readme.md` to `README.md` finds the destination already there.
fn same_entry(old: &Path, new: &Path) -> bool {
    let (Some(old_name), Some(new_name)) = (old.file_name(), new.file_name()) else {
        return false;
    };
    let lowercase = |name: &std::ffi::OsStr| name.to_string_lossy().to_lowercase();
    if old.parent() != new.parent() || lowercase(old_name) != lowercase(new_name) {
        return false;
    }
    // The directory doesn't hold an entry by the new name, so the match was the old one.
    let Some(Ok(entries)) = new.parent().map(std::fs::read_dir) else {
        return false;
    };
    !entries
        .filter_map(Result::ok)
        .any(|entry| entry.file_name() == new_name)
}

// See `benches/explorer.rs`.
#[cfg(feature = "bench")]
pub mod bench {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use super::{filter, Tree};

    /// A tree with some directories expanded.
    pub struct Bench(Tree);

    impl Bench {
        pub fn new(root: &Path, expanded: &[PathBuf]) -> Self {
            let mut tree = Tree::new(root.to_path_buf());
            tree.expanded.extend(expanded.iter().cloned());
            tree.nodes(false);
            Self(tree)
        }

        /// Lists `dir` again, like expanding it. The directories above it are expected to be
        /// expanded.
        pub fn list(&mut self, dir: &Path) -> usize {
            self.0.bench_list(dir)
        }

        /// What opening the tree again does: the visible entries, reading only the directories
        /// that changed.
        pub fn refresh(&mut self) -> usize {
            self.0.refresh();
            self.0.nodes(false).len()
        }

        /// Reads every directory again, like `R`.
        pub fn reload(&mut self) -> usize {
            self.0.reload();
            self.refresh()
        }
    }

    /// Opens a tree on `root` and reveals `path`, like `space E` does for the current buffer.
    pub fn reveal(root: &Path, path: &Path) -> usize {
        let mut tree = Tree::new(root.to_path_buf());
        tree.reveal(path, false);
        tree.nodes(false).len()
    }

    /// Everything the filter finds below `root`.
    pub struct Found(Vec<filter::Found>);

    /// Walks the workspace, which the first query does in the background.
    pub fn walk(root: &Path) -> Found {
        Found(filter::walk(root, &Default::default()))
    }

    impl Found {
        pub fn len(&self) -> usize {
            self.0.len()
        }

        pub fn is_empty(&self) -> bool {
            self.0.is_empty()
        }

        /// Matches `query` and builds the filtered tree, like each key typed in the filter.
        pub fn filter(&self, root: &Path, query: &str) -> usize {
            let hits = filter::search(&self.0, query);
            filter::tree(root, &hits, &HashSet::new()).0.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helix_view::Theme;

    use crate::ui::explorer::git_status::Mark;
    use crate::ui::explorer::tree::test_repo as repo;

    /// What the tree shows for `expanded`, as text.
    fn tree_rows(
        tree: &mut Tree,
        hide_ignored: bool,
        marks: &[(&Path, Mark)],
        current: Option<&Path>,
    ) -> Vec<String> {
        let theme = Theme::default();
        let look = Look::new(&theme);
        tree.nodes(hide_ignored)
            .into_iter()
            .map(|node| {
                let expanded = node.is_dir && tree.expanded.contains(&node.path);
                let mark = marks
                    .iter()
                    .find(|(path, _)| *path == node.path)
                    .map(|(_, mark)| *mark);
                let current = current == Some(node.path.as_path());
                Row::new(&Entry::new(node, expanded), mark, current, false, &look).text()
            })
            .collect()
    }

    fn filtered_rows(root: &Path, query: &str, collapsed: &HashSet<PathBuf>) -> Vec<String> {
        let found = filter::walk(root, &Default::default());
        let hits = filter::search(&found, query);
        let theme = Theme::default();
        let look = Look::new(&theme);
        let (entries, _) = filter::tree(root, &hits, collapsed);
        entries
            .iter()
            .map(|entry| Row::new(entry, None, false, false, &look).text())
            .collect()
    }

    #[test]
    fn rows_are_in_tree_order_with_indentation_and_signs() {
        let dir = repo();
        let root = dir.path();
        let mut tree = Tree::new(root.to_path_buf());
        // Directories first, then files, regardless of case. `.git` is never listed.
        assert_eq!(
            tree_rows(&mut tree, false, &[], None),
            [
                "+ src/",
                "+ target/",
                "  .gitignore",
                "  build.log",
                "  README.md"
            ]
        );

        tree.expanded
            .extend(["src", "src/nested", "target"].map(|dir| root.join(dir)));
        assert_eq!(
            tree_rows(&mut tree, false, &[], None),
            [
                "- src/",
                "  - nested/",
                "      .gitignore",
                "      public.txt",
                "      secret.txt",
                "    main.rs",
                "- target/",
                "  + debug/",
                "  .gitignore",
                "  build.log",
                "  README.md",
            ]
        );
    }

    #[test]
    fn hide_gitignored_leaves_out_ignored_entries() {
        let dir = repo();
        let root = dir.path();
        let mut tree = Tree::new(root.to_path_buf());
        tree.expanded
            .extend(["src", "src/nested"].map(|dir| root.join(dir)));
        assert_eq!(
            tree_rows(&mut tree, true, &[], None),
            [
                "- src/",
                "  - nested/",
                "      .gitignore",
                "      public.txt",
                "    main.rs",
                "  .gitignore",
                "  README.md",
            ]
        );
    }

    #[test]
    fn git_marks_follow_the_name() {
        let dir = repo();
        let root = dir.path();
        let mut tree = Tree::new(root.to_path_buf());
        let src = root.join("src");
        let readme = root.join("README.md");
        let rows = tree_rows(
            &mut tree,
            false,
            &[(&src, Mark::Modified), (&readme, Mark::Untracked)],
            Some(&readme),
        );
        assert_eq!(rows[0], "+ src/ ●");
        assert_eq!(rows[4], "  README.md U");
    }

    #[test]
    fn the_filter_keeps_the_directories_above_the_hits() {
        let dir = repo();
        let root = dir.path();
        let rows = filtered_rows(root, "public", &HashSet::new());
        assert_eq!(rows, ["- src/", "  - nested/", "      public.txt"]);

        // Matching is fuzzy, over the whole path.
        let rows = filtered_rows(root, "srcmain", &HashSet::new());
        assert_eq!(rows, ["- src/", "    main.rs"]);
        assert!(filtered_rows(root, "zzz", &HashSet::new()).is_empty());
    }

    #[test]
    fn the_filter_skips_ignored_entries_and_git() {
        let dir = repo();
        let root = dir.path();
        // `target/debug/app`, `build.log` and `secret.txt` are ignored, `.git` is not a match.
        for query in ["app", "build", "secret", ".git/", "debug"] {
            let rows = filtered_rows(root, query, &HashSet::new());
            assert!(
                rows.iter().all(|row| !row.contains("app")
                    && !row.contains("build.log")
                    && !row.contains("secret")
                    && !row.contains(".git/")
                    && !row.contains("debug")),
                "{query}: {rows:?}"
            );
        }
        // Hidden files are searched.
        let rows = filtered_rows(root, "gitignore", &HashSet::new());
        assert_eq!(
            rows,
            ["- src/", "  - nested/", "      .gitignore", "  .gitignore"]
        );
    }

    #[test]
    fn collapsing_a_directory_hides_its_hits() {
        let dir = repo();
        let root = dir.path();
        let collapsed: HashSet<PathBuf> = [root.join("src/nested")].into();
        assert_eq!(
            filtered_rows(root, "txt", &collapsed),
            ["- src/", "  + nested/"]
        );
        let collapsed: HashSet<PathBuf> = [root.join("src")].into();
        assert_eq!(filtered_rows(root, "txt", &collapsed), ["+ src/"]);
    }

    #[test]
    fn the_filter_selects_the_best_hit_and_highlights_what_matched() {
        let dir = repo();
        let root = dir.path();
        let found = filter::walk(root, &Default::default());
        let hits = filter::search(&found, "main");
        let (entries, best) = filter::tree(root, &hits, &HashSet::new());
        let best = best.unwrap();
        assert_eq!(entries[best].node.name, "main.rs");
        // Positions are in the name, not in the path.
        assert_eq!(entries[best].matched, [0, 1, 2, 3]);
    }

    #[test]
    fn the_cut_path_follows_a_rename() {
        let mut cut = Some(PathBuf::from("/project/src/main.rs"));
        follow_rename(
            &mut cut,
            Path::new("/project/lib"),
            Path::new("/project/core"),
        );
        assert_eq!(cut.as_deref(), Some(Path::new("/project/src/main.rs")));
        follow_rename(
            &mut cut,
            Path::new("/project/src"),
            Path::new("/project/app"),
        );
        assert_eq!(cut.as_deref(), Some(Path::new("/project/app/main.rs")));
    }

    #[test]
    fn a_case_only_rename_is_not_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("readme.md");
        let new = dir.path().join("README.md");
        std::fs::write(&old, "").unwrap();
        // What a case-insensitive filesystem finds at `new` is `old` itself.
        assert!(same_entry(&old, &new));
        std::fs::write(&new, "").unwrap();
        let case_sensitive = std::fs::read_dir(dir.path()).unwrap().count() == 2;
        if case_sensitive {
            // Here `new` really is another file.
            assert!(!same_entry(&old, &new));
        } else {
            // Writing `new` wrote `old` (macOS, Windows): still a case-only rename.
            assert!(same_entry(&old, &new));
        }
        assert!(!same_entry(&old, &dir.path().join("other.md")));
    }
}
