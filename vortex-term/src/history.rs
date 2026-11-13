//! History and bookmarks in the terminal UI (this fork): `ga`, `gA` and its sticky state, showing
//! a screen again, and the bookmark commands and picker. What a jump records and how history
//! walks are in `vortex_view::history`; bookmarks are in `vortex_view::bookmarks`.

use std::path::PathBuf;

use vortex_core::Selection;
use vortex_view::{
    align_view,
    bookmarks::{At, Bookmark, Direction},
    history::{Jump, Place, Screen, ScreenCover, ScreenKind, Step},
    info::Info,
    input::KeyEvent,
    keyboard::{KeyCode, KeyModifiers},
    Align, DocumentId, Editor,
};

use crate::{
    commands::Context,
    compositor::{self, Compositor, EventResult},
    job,
    ui::{self, diff_view::DiffView, overlay::overlaid, Picker, PickerColumn},
};

/// The infobox of `gA`'s sticky state.
pub const MENU_TITLE: &str = "History";
pub const MENU_TEXT: &str = "A back · a forward · Esc done";

/// `ga`: to the last place jumped from, or back to where a walk with `gA` started.
pub fn toggle(cx: &mut Context) {
    let count = cx.count();
    cx.callback.push(Box::new(move |compositor, cx| {
        step(compositor, cx.editor, Step::Toggle, count, None);
    }));
}

/// `gA`: one place back, and the sticky state where `A` goes on back and `a` forward.
pub fn back(cx: &mut Context) {
    let count = cx.count();
    cx.callback.push(Box::new(move |compositor, cx| {
        if step(compositor, cx.editor, Step::Back, count, None) {
            open_menu(cx.editor);
        }
    }));
}

/// One place forward after `gA`; the last one is where the walk started.
pub fn forward(cx: &mut Context) {
    let count = cx.count();
    cx.callback.push(Box::new(move |compositor, cx| {
        step(compositor, cx.editor, Step::Forward, count, None);
    }));
}

fn open_menu(editor: &mut Editor) {
    editor.history_menu = true;
    editor.autoinfo = Some(menu_info());
}

pub fn menu_info() -> Info {
    let mut info = Info::new(MENU_TITLE, &[] as &[(&str, &str)]);
    info.text = MENU_TEXT.to_string();
    info.width = info.width.max(MENU_TEXT.chars().count() as u16);
    info
}

fn close_menu(editor: &mut Editor) {
    editor.history_menu = false;
    if editor
        .autoinfo
        .as_ref()
        .is_some_and(|info| info.title == MENU_TITLE)
    {
        editor.autoinfo = None;
    }
}

/// A key while `gA`'s sticky state is open: `A` back, `a` forward, `Esc` closes it; any other
/// key closes it and is not taken (`None`), so it does what it always does. `here` is the screen
/// on top that a step leaves (the diff view), `None` for the editor's place.
pub fn menu_key(
    key: KeyEvent,
    editor: &mut Editor,
    here: impl FnOnce(&mut Compositor) -> Option<Place> + 'static,
) -> Option<EventResult> {
    if !editor.history_menu {
        return None;
    }
    let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
    let step_of = match key.code {
        KeyCode::Char('A') if plain => Step::Back,
        KeyCode::Char('a') if plain => Step::Forward,
        KeyCode::Esc => {
            close_menu(editor);
            return Some(EventResult::Consumed(None));
        }
        _ => {
            close_menu(editor);
            return None;
        }
    };
    Some(EventResult::Consumed(Some(Box::new(
        move |compositor: &mut Compositor, cx: &mut compositor::Context| {
            let here = here(compositor);
            step(compositor, cx.editor, step_of, 1, here);
            // the menu stays open, even at either end
            open_menu(cx.editor);
        },
    ))))
}

/// A step from the screen on top, `here`, already off the compositor (the diff view's `ga` and
/// `gA`); `gA` opens the sticky state.
pub fn step_from_screen(
    compositor: &mut Compositor,
    editor: &mut Editor,
    step_of: Step,
    here: Option<Place>,
) {
    if step(compositor, editor, step_of, 1, here) && step_of == Step::Back {
        open_menu(editor);
    }
}

/// Takes a step through the focused view's history and shows where it lands; `here` is the
/// screen on top that it leaves, already off the compositor, `None` for the editor's place.
/// Returns whether it moved; it says why not.
pub fn step(
    compositor: &mut Compositor,
    editor: &mut Editor,
    step: Step,
    count: usize,
    here: Option<Place>,
) -> bool {
    // a step may open another buffer or screen: it ends watch mode, also from the keys that
    // don't go through the keymap (the sticky state, the hex view, a diff on top)
    let name = match step {
        Step::Toggle => "history_toggle",
        Step::Back => "history_back",
        Step::Forward => "history_forward",
    };
    crate::watch::before_command(editor, name);
    let left = here.clone();
    let here = here.unwrap_or_else(|| editor.place());
    match editor.history_step(step, count, here.clone()) {
        Ok(place) => {
            show(compositor, editor, place, &here);
            true
        }
        Err(message) => {
            // the screen it would have left stays
            if let Some(Place::Screen(screen)) = left {
                restore(compositor, editor, &screen);
            }
            editor.set_status(message);
            false
        }
    }
}

/// Shows `place`, leaving `here`, without recording a jump.
fn show(compositor: &mut Compositor, editor: &mut Editor, place: Place, here: &Place) {
    match place {
        Place::Text { doc, selection } => {
            editor.show_place(doc, selection);
            close_dashboard_buffer(editor, here);
        }
        Place::Screen(screen) => {
            if !matches!(here, Place::Screen(Screen { kind, .. }) if *kind == ScreenKind::DebugPanel)
            {
                editor.debug.unfocus();
            }
            editor.without_history(|editor| restore(compositor, editor, &screen));
        }
    }
}

/// Shows a screen again.
fn restore(compositor: &mut Compositor, editor: &mut Editor, screen: &Screen) {
    match screen.kind {
        ScreenKind::Dashboard | ScreenKind::Vortex => {
            if let Some(view) = compositor.find::<ui::EditorView>() {
                if screen.kind == ScreenKind::Vortex {
                    view.open_vortex(editor);
                } else {
                    view.open_dashboard(editor);
                }
            }
        }
        ScreenKind::Diff => {
            let Some(state) = screen.take() else {
                return;
            };
            if let Ok(mut diff) = state.downcast::<DiffView>() {
                diff.returned(editor, screen.stash.clone());
                compositor.push(diff);
            }
        }
        ScreenKind::DebugPanel => editor.debug.focus(),
    }
}

/// Closes the empty buffer the dashboard was drawn over once history leaves it for another
/// buffer, so that walking to and from the dashboard leaves no empty buffers behind.
fn close_dashboard_buffer(editor: &mut Editor, here: &Place) {
    if !matches!(
        here,
        Place::Screen(Screen {
            kind: ScreenKind::Dashboard | ScreenKind::Vortex,
            ..
        })
    ) {
        return;
    }
    let Some(ScreenCover { doc, .. }) = editor.screen_cover.take() else {
        return;
    };
    let shown = editor.tree.views().any(|(view, _)| view.doc == doc);
    let empty = editor
        .document(doc)
        .is_some_and(|doc| doc.path().is_none() && !doc.is_modified());
    if !shown && empty {
        let _ = editor.close_document(doc, false);
    }
}

/// Records the opening of a screen over the focused place, a jump.
pub fn opened_screen(editor: &mut Editor) {
    let from = editor.place();
    editor.record_jump(Jump::Screen, from);
}

/// Marks the dashboard just opened over the focused view, so that history knows the place is the
/// dashboard while it is up.
pub fn cover(editor: &mut Editor, kind: ScreenKind) {
    let view = editor.tree.get(editor.tree.focus);
    let doc = &editor.documents[&view.doc];
    editor.screen_cover = Some(ScreenCover {
        kind,
        view: view.id,
        doc: view.doc,
        version: doc.version(),
    });
}

// Bookmarks

/// `C-s`: sets a bookmark on the line of each cursor, or removes the one there.
pub fn toggle_bookmark(cx: &mut Context) {
    let (view, doc) = current_ref!(cx.editor);
    if doc.is_hex() {
        cx.editor
            .set_error("Bookmarks are lines of text: the hex view has none");
        return;
    }
    let text = doc.text().slice(..);
    let mut lines: Vec<usize> = doc
        .selection(view.id)
        .iter()
        .map(|range| range.cursor_line(text))
        .collect();
    lines.dedup();
    let doc_id = doc.id();
    let (set, removed) = cx.editor.toggle_bookmarks(doc_id, &lines);
    let total = cx.editor.bookmarks().len();
    let message = match (set, removed) {
        (1, 0) => format!("Bookmark {} set", number_of_last(cx.editor)),
        (0, 1) => "Bookmark removed".to_string(),
        (set, 0) => format!("{set} bookmarks set"),
        (0, removed) => format!("{removed} bookmarks removed"),
        (set, removed) => format!("{set} bookmarks set, {removed} removed"),
    };
    cx.editor.set_status(format!("{message} ({total} in all)"));
}

/// The number of the bookmark set last.
fn number_of_last(editor: &Editor) -> usize {
    let current = editor.bookmark_state.current;
    editor
        .bookmarks()
        .iter()
        .position(|bookmark| Some(bookmark.seq) == current)
        .map_or(0, |index| index + 1)
}

/// `C-i`: the next bookmark, in the order they were set.
pub fn next_bookmark(cx: &mut Context) {
    go_round(cx.editor, Direction::Next);
}

/// `C-o`: the previous bookmark.
pub fn prev_bookmark(cx: &mut Context) {
    go_round(cx.editor, Direction::Previous);
}

fn go_round(editor: &mut Editor, direction: Direction) {
    let Some(bookmark) = editor.bookmark_step(direction) else {
        editor.set_status("No bookmarks: C-s sets one on the cursor's line");
        return;
    };
    let total = editor.bookmarks().len();
    let number = editor
        .bookmarks()
        .iter()
        .position(|other| other.seq == bookmark.seq)
        .map_or(0, |index| index + 1);
    if go_to(editor, &bookmark) {
        editor.set_status(format!("Bookmark {number} of {total}"));
    }
}

/// Goes to a bookmark, a jump; returns whether it got there.
pub fn go_to(editor: &mut Editor, bookmark: &Bookmark) -> bool {
    editor.bookmark_state.current = Some(bookmark.seq);
    let from = editor.place();
    let opened = editor.without_history(|editor| match &bookmark.at {
        At::Buffer { doc, .. } => Ok(*doc),
        At::File { path, .. } => editor
            .open(path, vortex_view::editor::Action::Replace)
            .map_err(|err| format!("{}: {err}", path.display())),
    });
    let doc = match opened {
        Ok(doc) => doc,
        Err(err) => {
            editor.set_error(err);
            return false;
        }
    };
    // the line again: a file opened just now has its bookmarks back
    let line = editor
        .document(doc)
        .and_then(|document| {
            let text = document.text().slice(..);
            document
                .bookmarks
                .lines(text)
                .find(|&(seq, _)| seq == bookmark.seq)
        })
        .map_or(bookmark.line(), |(_, line)| line);
    editor.debug.unfocus();
    editor.without_history(|editor| {
        let view_id = editor.tree.focus;
        if editor.tree.get(view_id).doc != doc {
            editor.switch(doc, vortex_view::editor::Action::Replace);
        }
        let (view, document) = current!(editor);
        let text = document.text().slice(..);
        let line = line.min(text.len_lines().saturating_sub(1));
        let start = text.line_to_char(line);
        let pos = text
            .line(line)
            .chars()
            .position(|c| !c.is_whitespace())
            .map_or(start, |col| start + col);
        document.set_selection(view.id, Selection::point(pos));
        align_view(document, view, Align::Center);
    });
    editor.record_jump(Jump::Bookmark, from);
    true
}

/// A row of the bookmark picker.
#[derive(Debug, Clone)]
pub struct Row {
    pub number: usize,
    pub bookmark: Bookmark,
    pub path: Option<PathBuf>,
    pub name: String,
    pub text: String,
}

fn rows(editor: &Editor) -> Vec<Row> {
    editor
        .bookmarks()
        .into_iter()
        .enumerate()
        .map(|(index, bookmark)| {
            let line = bookmark.line();
            let (path, name, text) = match &bookmark.at {
                At::Buffer { doc, .. } => {
                    let document = &editor.documents[doc];
                    let text = document.text().slice(..);
                    let content = if line < text.len_lines() {
                        text.line(line).to_string()
                    } else {
                        String::new()
                    };
                    (
                        document.path().map(|path| path.to_path_buf()),
                        document.display_name().into_owned(),
                        content,
                    )
                }
                At::File { path, .. } => {
                    let content = std::fs::read_to_string(path)
                        .ok()
                        .and_then(|text| text.lines().nth(line).map(str::to_string))
                        .unwrap_or_default();
                    (Some(path.clone()), String::new(), content)
                }
            };
            let name = match &path {
                Some(path) => vortex_stdx::path::get_relative_path(path)
                    .display()
                    .to_string(),
                None => name,
            };
            Row {
                number: index + 1,
                bookmark,
                path,
                name,
                text: text.trim().to_string(),
            }
        })
        .collect()
}

/// The key of the bookmark picker that removes the selected bookmark.
pub const REMOVE_KEY: &str = "C-x";

/// `space j`: the bookmarks in the order they were set, with their number, file and line; `Enter`
/// goes to one, `C-x` removes it.
pub fn bookmark_picker(cx: &mut Context) {
    cx.callback.push(Box::new(|compositor, cx| {
        open_picker(compositor, cx.editor, 0)
    }));
}

fn open_picker(compositor: &mut Compositor, editor: &mut Editor, cursor: u32) {
    let rows = rows(editor);
    if rows.is_empty() {
        editor.set_status("No bookmarks: C-s sets one on the cursor's line");
        return;
    }
    let columns = [
        PickerColumn::new("#", |row: &Row, _| row.number.to_string().into()),
        PickerColumn::new("file", |row: &Row, _| {
            format!("{}:{}", row.name, row.bookmark.line() + 1).into()
        }),
        PickerColumn::new("line", |row: &Row, _| row.text.as_str().into()),
    ];
    let picker = Picker::new(columns, 1, rows, (), |cx, row: &Row, action| {
        if !matches!(action, vortex_view::editor::Action::Replace) {
            // a split first, then the bookmark in it
            let doc = match &row.bookmark.at {
                At::Buffer { doc, .. } => Some(*doc),
                At::File { .. } => None,
            };
            if let Some(doc) = doc {
                cx.editor.switch(doc, action);
            }
        }
        go_to(cx.editor, &row.bookmark);
    })
    .with_initial_cursor(cursor)
    .with_preview(|_editor, row: &Row| match &row.bookmark.at {
        At::Buffer { doc, line } => Some(((*doc).into(), Some((*line, *line)))),
        At::File { path, line } => Some((path.as_path().into(), Some((*line, *line)))),
    })
    .with_stacked_key_handler(
        vortex_view::input::KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::CONTROL,
        },
        |cx, row: &Row| {
            cx.editor.remove_bookmark(row.bookmark.seq);
            let cursor = (row.number - 1) as u32;
            cx.editor
                .set_status(format!("Bookmark {} removed", row.number));
            // the picker again, with the numbers closing the gap
            cx.jobs.callback(async move {
                let call: job::Callback = job::Callback::EditorCompositor(Box::new(
                    move |editor: &mut Editor, compositor: &mut Compositor| {
                        compositor.remove(ui::picker::ID);
                        let left = editor.bookmarks().len() as u32;
                        if left > 0 {
                            open_picker(compositor, editor, cursor.min(left - 1));
                        }
                    },
                ));
                Ok(call)
            });
        },
    );
    compositor.push(Box::new(overlaid(picker)));
}

/// The bookmark picker's rows as text, for tests: `number  file:line  text`.
pub fn picker_rows(editor: &Editor) -> Vec<String> {
    rows(editor)
        .iter()
        .map(|row| {
            format!(
                "{}  {}:{}  {}",
                row.number,
                row.name,
                row.bookmark.line() + 1,
                row.text
            )
        })
        .collect()
}

/// The documents with bookmarks, for tests.
pub fn bookmarked_docs(editor: &Editor) -> Vec<DocumentId> {
    let mut docs: Vec<DocumentId> = editor
        .bookmarks()
        .iter()
        .filter_map(|bookmark| match bookmark.at {
            At::Buffer { doc, .. } => Some(doc),
            At::File { .. } => None,
        })
        .collect();
    docs.dedup();
    docs
}
