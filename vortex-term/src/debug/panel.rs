//! The panel's keys, mouse and focus. Keys are modal, like the fork's pickers: they act on the
//! row under the cursor of the current section. `Space` plays and pauses (spectator mode): there
//! is no space menu while the panel has the keys; Esc, `q` or `C-g` give them back to the
//! editor first.

use std::path::{Path, PathBuf};

use vortex_core::{Position, Selection};
use vortex_view::{
    align_view,
    debug::{
        self,
        history::{format_bytes, Setting},
        spectator::ThreadKey,
        Role, Row, Section, StackSession, Target, Tree, VarPath, Watch,
    },
    editor::Action,
    input::{KeyEvent, MouseButton, MouseEvent, MouseEventKind},
    keyboard::{KeyCode, KeyModifiers},
    Align, Editor,
};

use super::{breakpoints, load, playback, rewind, threads};
use crate::{
    compositor::{self, Compositor},
    ui::{self, Prompt, PromptEvent},
};

/// `C-g`: focuses the debug panel, or goes back to the editor.
pub const FOCUS_KEY: KeyEvent = KeyEvent {
    code: KeyCode::Char('g'),
    modifiers: KeyModifiers::CONTROL,
};

/// Shows the panel; the terminal panel, which shares its place, hides.
pub fn show(editor: &mut Editor) {
    editor.terminal.hide();
    editor.debug.show();
}

/// Shows or hides the panel.
pub fn toggle(editor: &mut Editor) {
    if editor.debug.visible {
        editor.debug.hide();
    } else {
        show(editor);
    }
}

/// `C-g`: from the editor, shows and focuses the panel; from the panel, back to the editor.
pub fn toggle_focus(editor: &mut Editor) {
    if editor.debug.is_focused() {
        editor.debug.unfocus();
    } else {
        show(editor);
        enter(editor);
    }
}

/// Shows and focuses `section`.
pub fn focus_section(editor: &mut Editor, section: Section) {
    show(editor);
    enter(editor);
    editor.debug.select(section);
}

/// Focuses the panel: a screen, so going to it from the editor is a jump (this fork: history).
fn enter(editor: &mut Editor) {
    if editor.debug.is_focused() {
        return;
    }
    let from = editor.place();
    editor.debug.focus();
    editor.record_jump(vortex_view::history::Jump::Screen, from);
}

/// The rows of `section`, searched.
pub fn rows(editor: &Editor, section: Section) -> Vec<Row> {
    let state = &editor.debug;
    let rows = match section {
        Section::Variables => state.var_rows(false, super::memory::shown(editor)),
        Section::Watch => state.var_rows(true, super::memory::shown(editor)),
        Section::Stack => match state.spectator.history.current() {
            Some(snapshot) => debug::rewind_stack_rows(snapshot, state.spectator.history.frame),
            None => {
                let active = editor.debug_adapters.get_active_client().map(|c| c.id());
                let sessions: Vec<StackSession> = state
                    .threads
                    .iter()
                    .filter_map(|entry| {
                        let client = editor.debug_adapters.get_client(entry.session)?;
                        Some(StackSession {
                            session: entry.session,
                            name: &client.name,
                            threads: &entry.threads,
                            frames: &client.stack_frames,
                            states: &client.thread_states,
                            current: (active == Some(entry.session))
                                .then_some(client.thread_id.map(|t| (t, client.active_frame)))
                                .flatten(),
                        })
                    })
                    .collect();
                debug::stack_rows(&sessions, &state.expanded_threads, &state.spectator)
            }
        },
        Section::Breakpoints => debug::breakpoint_rows(
            &editor.breakpoints,
            &vortex_stdx::env::current_working_dir(),
        ),
        Section::Console => state.console_rows(),
    };
    debug::filter_rows(rows, state.filters.get(&section).map(String::as_str))
}

/// The thread of the row under the cursor in the call stack.
pub fn current_thread(editor: &Editor) -> Option<ThreadKey> {
    match current_target_in(editor, Section::Stack)? {
        Target::Thread(key) | Target::Frame { key, .. } => Some(key),
        _ => None,
    }
}

fn current_target_in(editor: &Editor, section: Section) -> Option<Target> {
    let rows = rows(editor, section);
    let row = editor
        .debug
        .cursor(section)
        .row
        .min(rows.len().saturating_sub(1));
    rows.into_iter().nth(row).map(|row| row.target)
}

pub fn current_target(editor: &Editor) -> Option<Target> {
    let section = editor.debug.section;
    let rows = rows(editor, section);
    let row = editor
        .debug
        .cursor(section)
        .row
        .min(rows.len().saturating_sub(1));
    rows.into_iter().nth(row).map(|row| row.target)
}

/// Moves the cursor of the current section by `by` rows (clamped).
fn move_cursor(editor: &mut Editor, by: isize) {
    let section = editor.debug.section;
    let count = rows(editor, section).len();
    let cursor = editor.debug.cursor_mut(section);
    let row = (cursor.row as isize + by).clamp(0, count.saturating_sub(1) as isize);
    cursor.row = row as usize;
}

fn move_to(editor: &mut Editor, row: usize) {
    let section = editor.debug.section;
    let count = rows(editor, section).len();
    editor.debug.cursor_mut(section).row = row.min(count.saturating_sub(1));
}

/// Rows a page moves: half the panel.
fn half_page(editor: &Editor) -> isize {
    (editor.debug.area.height.saturating_sub(1) / 2).max(1) as isize
}

/// What a key in the focused panel did.
pub enum Handled {
    /// Not the panel's: the editor's keymap gets it.
    No,
    Yes,
    /// The panel's, and it opens a prompt (or another overlay, such as the key help).
    Prompt(Box<dyn crate::compositor::Component>),
}

/// Handles a key while the panel is focused.
pub fn handle_key(editor: &mut Editor, key: KeyEvent) -> Handled {
    use KeyCode::*;
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    let ctrl = key.modifiers == KeyModifiers::CONTROL;
    let section = editor.debug.section;

    if std::mem::take(&mut editor.debug.pending_g) && key.code == Char('g') && plain {
        move_to(editor, 0);
        return Handled::Yes;
    }
    if editor.debug.spectator.settings.is_some() {
        settings_key(editor, key);
        return Handled::Yes;
    }

    match key.code {
        // playback's keys are debug mode's (`Space`, `f`, `r`, `e`, `<` `>`...): the keys the
        // panel doesn't take go to debug mode while it is on (see the end)
        Char('u') if plain => cycle_granularity(editor),
        Char('z') if plain => {
            if let Some(key) = threads::selected_or_spectated(editor) {
                threads::toggle_freeze(editor, key);
            }
        }
        Char('o') if plain => threads::toggle_solo(editor),
        Char('p') if plain => {
            if let Some(key) = threads::selected_or_spectated(editor) {
                threads::pause_or_continue(editor, key);
            }
        }
        Char('{') if plain => rewind::back(editor, 10),
        Char('}') if plain => rewind::forward(editor, 10),
        Char('*') if plain => toggle_mark(editor),
        Char('M') if plain => super::memory::open_selected(editor),
        Char('?') if plain => return Handled::Prompt(Box::new(key_help())),
        // the editor's: the command line, the terminal panel
        Char(':') if plain => return Handled::No,
        Char('t') if ctrl => return Handled::No,
        Char('g') if ctrl => editor.debug.unfocus(),
        Esc => {
            if editor
                .debug
                .filters
                .get(&section)
                .is_some_and(|filter| !filter.is_empty())
            {
                editor.debug.filters.remove(&section);
            } else {
                editor.debug.unfocus();
            }
        }
        Char('q') if plain => editor.debug.unfocus(),
        Tab if key.modifiers == KeyModifiers::SHIFT => editor.debug.select(section.cycle(-1)),
        // in debug mode, `Tab` brought you here and takes you back
        Tab if plain && editor.debug.mode_on() => editor.debug.unfocus(),
        Tab | Char('w') if plain => editor.debug.select(section.cycle(1)),
        Char('b') if plain => editor.debug.select(section.cycle(-1)),
        Char(digit @ '1'..='5') if plain => editor
            .debug
            .select(Section::ALL[digit as usize - '1' as usize]),
        Char('j') | Down if plain => move_cursor(editor, 1),
        Char('k') | Up if plain => move_cursor(editor, -1),
        Char('n') if ctrl => move_cursor(editor, 1),
        Char('p') if ctrl => move_cursor(editor, -1),
        Char('d') if ctrl => move_cursor(editor, half_page(editor)),
        Char('u') if ctrl => move_cursor(editor, -half_page(editor)),
        PageDown => move_cursor(editor, half_page(editor) * 2),
        PageUp => move_cursor(editor, -half_page(editor) * 2),
        Home => move_to(editor, 0),
        End | Char('G') if plain || key.code == End => move_to(editor, usize::MAX),
        Char('g') if plain => editor.debug.pending_g = true,
        Enter => return activate(editor),
        Char('l') | Right if plain => set_open(editor, true),
        Char('h') | Left if plain => set_open(editor, false),
        Char('s') if plain => return Handled::Prompt(Box::new(search_prompt(editor, section))),
        Char('i') if plain && editor.debug.is_rewinding() => editor.set_error(rewind::READ_ONLY),
        Char('c') if plain && editor.debug.is_rewinding() => editor.set_error(rewind::READ_ONLY),
        Char('i') if plain => {
            if editor.debug.area.width < super::render::WIDE {
                editor.debug.select(Section::Console);
            }
            return Handled::Prompt(Box::new(evaluate_prompt()));
        }
        Char('c') if plain => return change(editor),
        Char('m') if plain => return log_message(editor),
        Char('a') if plain => return add_watch(editor),
        Char('d') if plain => delete(editor),
        Char('t') if plain => toggle_breakpoint(editor),
        Char('y') if plain => yank(editor),
        // not the panel's: debug mode's, while it is on
        _ if editor.debug.mode_on() => return Handled::No,
        _ => {}
    }
    Handled::Yes
}

/// The keys of the panel, for the key help overlay (`?`).
fn key_help() -> crate::ui::keyhelp::KeyHelp {
    crate::ui::keyhelp::KeyHelp::new(
        "Debug panel",
        &[
            (
                "panel",
                vec![
                    ("j k, gg G, C-d C-u", "move"),
                    (
                        "Tab S-Tab, w b, 1-5",
                        "next, previous, a given section (in debug mode Tab goes back)",
                    ),
                    ("Enter", "open or close, select a frame, go to a breakpoint"),
                    ("l h", "open, close"),
                    ("s", "search the section (Esc clears)"),
                    ("c", "change a value, watch, or condition"),
                    ("m", "a breakpoint's log message"),
                    ("a", "add a watch, or watch a variable"),
                    ("d", "delete the watch or breakpoint"),
                    ("t", "turn a breakpoint off or on"),
                    ("y", "yank the value"),
                    ("i", "evaluate an expression"),
                    ("M", "the memory of the value"),
                    (":, C-t", "command line, terminal"),
                    ("q Esc C-g", "back to the editor"),
                ],
            ),
            (
                "playback (debug mode's keys go through: Space, f, r, e, < >...)",
                vec![
                    ("z o p", "freeze, solo, pause or continue a thread"),
                    ("u", "the step's unit"),
                    ("*", "mark a variable for fast forward"),
                    ("{ }", "rewind ten stops back, forward"),
                ],
            ),
            ("help", vec![("?", "these keys")]),
        ],
    )
}

/// Enter: opens or closes a node, selects a frame or a thread, or goes to a breakpoint. In the
/// console it evaluates an expression.
fn activate(editor: &mut Editor) -> Handled {
    if editor.debug.section == Section::Console {
        return Handled::Prompt(Box::new(evaluate_prompt()));
    }
    match current_target(editor) {
        Some(Target::Var(path)) => {
            let open = editor.debug.is_expanded(&path);
            open_node(editor, path, !open);
        }
        Some(Target::Thread(key)) if !editor.debug.is_rewinding() => threads::spectate(editor, key),
        Some(Target::Thread(key)) => load::select_frame(editor, key, 0),
        Some(Target::Frame { key, index }) => load::select_frame(editor, key, index),
        Some(Target::Breakpoint { path, line }) => goto_line(editor, &path, line),
        _ => {}
    }
    Handled::Yes
}

fn open_node(editor: &mut Editor, path: VarPath, open: bool) {
    let has_children = editor
        .debug
        .node(&path)
        .is_some_and(|node| node.has_children());
    if !has_children {
        return;
    }
    editor.debug.set_expanded(&path, open);
    if open {
        load::expand(editor, path);
    }
}

/// `l` / `h`: opens or closes the node (or thread) under the cursor; `h` on a closed node goes to
/// its parent.
fn set_open(editor: &mut Editor, open: bool) {
    match current_target(editor) {
        Some(Target::Var(path)) => {
            let is_open = editor.debug.is_expanded(&path)
                && editor
                    .debug
                    .node(&path)
                    .is_some_and(|node| node.has_children());
            if open || is_open {
                open_node(editor, path, open);
            } else if path.names.len() > 1 {
                let parent = Target::Var(VarPath {
                    tree: path.tree.clone(),
                    names: path.names[..path.names.len() - 1].to_vec(),
                });
                let section = editor.debug.section;
                if let Some(row) = rows(editor, section)
                    .iter()
                    .position(|r| r.target == parent)
                {
                    move_to(editor, row);
                }
            }
        }
        Some(Target::Thread(key)) if !editor.debug.is_rewinding() => {
            let current = editor
                .debug_adapters
                .get_active_client()
                .and_then(|client| Some((client.id(), client.thread_id?)));
            if Some((key.session, key.thread)) != current {
                if open {
                    editor.debug.expanded_threads.insert(key);
                    load::load_thread(editor, key);
                } else {
                    editor.debug.expanded_threads.remove(&key);
                }
            }
        }
        Some(Target::Frame { key, .. }) if !open => {
            let section = editor.debug.section;
            if let Some(row) = rows(editor, section)
                .iter()
                .position(|r| r.target == Target::Thread(key))
            {
                move_to(editor, row);
            }
        }
        _ => {}
    }
}

/// Opens `path` at `line` (0-based) in the focused split.
pub fn goto_line(editor: &mut Editor, path: &Path, line: usize) {
    if let Err(err) = editor.open(path, Action::Replace) {
        editor.set_error(format!("Couldn't open {}: {err}", path.display()));
        return;
    }
    let (view, doc) = vortex_view::current!(editor);
    let text = doc.text();
    let line = line.min(text.len_lines().saturating_sub(1));
    let pos = vortex_core::pos_at_coords(text.slice(..), Position::new(line, 0), true);
    doc.set_selection(view.id, Selection::point(pos));
    align_view(doc, view, Align::Center);
}

/// `s`: searches the section as you type; Esc gives back the search before.
fn search_prompt(editor: &Editor, section: Section) -> Prompt {
    let before = editor.debug.filters.get(&section).cloned();
    let line = before.clone().unwrap_or_default();
    Prompt::new(
        "search: ".into(),
        None,
        ui::completers::none,
        move |cx: &mut compositor::Context, input: &str, event: PromptEvent| {
            let filters = &mut cx.editor.debug.filters;
            match event {
                PromptEvent::Update | PromptEvent::Validate => {
                    if input.is_empty() {
                        filters.remove(&section);
                    } else {
                        filters.insert(section, input.to_owned());
                    }
                    cx.editor.debug.cursor_mut(section).row = 0;
                }
                PromptEvent::Abort => match &before {
                    Some(before) => {
                        filters.insert(section, before.clone());
                    }
                    None => {
                        filters.remove(&section);
                    }
                },
            }
        },
    )
    .with_line(line, editor)
}

/// An input on the command line that calls `done` with what was typed.
fn input(
    label: &'static str,
    history: Option<char>,
    line: String,
    editor: &Editor,
    done: impl Fn(&mut Editor, &str) -> anyhow::Result<()> + 'static,
) -> Prompt {
    Prompt::new(
        label.into(),
        history,
        ui::completers::none,
        move |cx: &mut compositor::Context, input: &str, event: PromptEvent| {
            if event != PromptEvent::Validate {
                return;
            }
            if let Err(err) = done(cx.editor, input) {
                cx.editor.set_error(err.to_string());
            }
        },
    )
    .with_line(line, editor)
}

/// The console's input: evaluates in the selected frame. Its history is the `=` register.
pub fn evaluate_prompt() -> Prompt {
    Prompt::new(
        "evaluate: ".into(),
        Some('='),
        ui::completers::none,
        |cx: &mut compositor::Context, input: &str, event: PromptEvent| {
            if event == PromptEvent::Validate && !input.trim().is_empty() {
                load::evaluate(cx.editor, input.trim().to_owned());
            }
        },
    )
}

/// The input that adds a watch expression.
pub fn watch_prompt(editor: &Editor, line: String) -> Prompt {
    input("watch: ", None, line, editor, |editor, text| {
        let text = text.trim();
        if !text.is_empty() {
            editor.debug.watches.push(Watch {
                expression: text.to_owned(),
                node: None,
            });
            editor.debug.generation += 1;
            load::reload_frame(editor);
        }
        Ok(())
    })
}

/// `c`: changes the value under the cursor, the watch expression, or the breakpoint's
/// condition.
fn change(editor: &mut Editor) -> Handled {
    match current_target(editor) {
        Some(Target::Var(path)) => {
            if let (Tree::Watch(index), 1) = (&path.tree, path.names.len()) {
                if editor.debug.section == Section::Watch {
                    let index = *index;
                    let expression = editor.debug.watches[index].expression.clone();
                    return Handled::Prompt(Box::new(input(
                        "watch: ",
                        None,
                        expression,
                        editor,
                        move |editor, text| {
                            if let Some(watch) = editor.debug.watches.get_mut(index) {
                                watch.expression = text.trim().to_owned();
                                watch.node = None;
                            }
                            editor.debug.generation += 1;
                            load::reload_frame(editor);
                            Ok(())
                        },
                    )));
                }
            }
            let Some(node) = editor.debug.node(&path) else {
                return Handled::Yes;
            };
            if path.tree == Tree::Scopes && path.names.len() == 1 {
                return Handled::Yes;
            }
            let value = node.value.clone();
            Handled::Prompt(Box::new(input(
                "value: ",
                None,
                value,
                editor,
                move |editor, text| load::set_value(editor, path.clone(), text.to_owned()),
            )))
        }
        Some(Target::Breakpoint { path, line }) => {
            let condition = breakpoint(editor, &path, line)
                .and_then(|b| b.condition.clone())
                .unwrap_or_default();
            Handled::Prompt(Box::new(input(
                "condition: ",
                None,
                condition,
                editor,
                move |editor, text| {
                    breakpoints::edit(editor, &path, line, |b| {
                        b.condition = (!text.is_empty()).then(|| text.to_owned())
                    })
                    .map(|_| ())
                },
            )))
        }
        _ => Handled::Yes,
    }
}

fn breakpoint<'a>(
    editor: &'a Editor,
    path: &Path,
    line: usize,
) -> Option<&'a vortex_view::editor::Breakpoint> {
    editor
        .breakpoints
        .get(path)?
        .iter()
        .find(|b| b.line == line)
}

/// `m`: the breakpoint's log message (a logpoint logs it instead of stopping).
fn log_message(editor: &mut Editor) -> Handled {
    let Some(Target::Breakpoint { path, line }) = current_target(editor) else {
        return Handled::Yes;
    };
    let message = breakpoint(editor, &path, line)
        .and_then(|b| b.log_message.clone())
        .unwrap_or_default();
    Handled::Prompt(Box::new(input(
        "log message: ",
        None,
        message,
        editor,
        move |editor, text| {
            breakpoints::edit(editor, &path, line, |b| {
                b.log_message = (!text.is_empty()).then(|| text.to_owned())
            })
            .map(|_| ())
        },
    )))
}

/// `a`: adds a watch expression; on a variable, watches it.
fn add_watch(editor: &mut Editor) -> Handled {
    let line = match (editor.debug.section, current_target(editor)) {
        (Section::Variables, Some(Target::Var(path))) => match editor.debug.node(&path) {
            Some(node) if path.names.len() > 1 => {
                let expression = node
                    .evaluate_name
                    .clone()
                    .unwrap_or_else(|| node.name.clone());
                editor.debug.watches.push(Watch {
                    expression,
                    node: None,
                });
                editor.debug.generation += 1;
                load::reload_frame(editor);
                crate::debug::mode::say(editor, "Watching it");
                return Handled::Yes;
            }
            _ => String::new(),
        },
        _ => String::new(),
    };
    Handled::Prompt(Box::new(watch_prompt(editor, line)))
}

/// `d`: deletes the watch or the breakpoint under the cursor; in the console, clears it.
fn delete(editor: &mut Editor) {
    match (editor.debug.section, current_target(editor)) {
        (Section::Watch, Some(Target::Var(path))) => {
            if let Tree::Watch(index) = path.tree {
                if index < editor.debug.watches.len() {
                    editor.debug.watches.remove(index);
                    // the paths of the watches after it moved up
                    editor
                        .debug
                        .expanded
                        .retain(|p| !matches!(p.tree, Tree::Watch(_)));
                    move_cursor(editor, 0);
                }
            }
        }
        (_, Some(Target::Breakpoint { path, line })) => {
            breakpoints::remove(editor, &path, line);
            move_cursor(editor, 0);
        }
        (Section::Console, _) => {
            editor.debug.console.clear();
            move_to(editor, 0);
        }
        _ => {}
    }
}

/// `t`: turns the breakpoint under the cursor off or on.
fn toggle_breakpoint(editor: &mut Editor) {
    if let Some(Target::Breakpoint { path, line }) = current_target(editor) {
        if let Err(err) = breakpoints::edit(editor, &path, line, |b| b.disabled = !b.disabled) {
            editor.set_error(err.to_string());
        }
    }
}

/// `y`: yanks the value under the cursor (the line, in the console).
fn yank(editor: &mut Editor) {
    let text = match current_target(editor) {
        Some(Target::Var(path)) => editor.debug.node(&path).map(|node| node.value.clone()),
        Some(Target::ConsoleLine(index)) => editor
            .debug
            .console
            .lines
            .get(index)
            .map(|line| line.text.clone()),
        Some(Target::Breakpoint { path, line }) => Some(format!("{}:{}", path.display(), line + 1)),
        Some(Target::Frame { key, index }) => match editor.debug.spectator.history.current() {
            Some(snapshot) => snapshot.frames.get(index).map(|frame| frame.name.clone()),
            None => editor
                .debug_adapters
                .get_client(key.session)
                .and_then(|client| client.stack_frames.get(&key.thread)?.get(index))
                .map(|frame| frame.name.clone()),
        },
        _ => None,
    };
    if let Some(text) = text {
        let register = editor.config().default_yank_register;
        match editor.registers.write(register, vec![text]) {
            Ok(()) => crate::debug::mode::say(editor, format!("Yanked to register {register}")),
            Err(err) => editor.set_error(err.to_string()),
        }
    }
}

/// The mouse over the panel: a click focuses it and puts the cursor on the row (a second click
/// on the same row acts like Enter, a click on a tab switches to it), the wheel scrolls. A click
/// elsewhere gives the keys back to the editor. Returns whether the event was the panel's.
pub fn handle_mouse(mouse: &MouseEvent, editor: &mut Editor, compositor: &mut Compositor) -> bool {
    // a picker or a popup over the panel gets its clicks
    if !editor.debug.visible || compositor.layer_count() > 1 {
        return false;
    }
    let area = editor.debug.area;
    let over = mouse.column >= area.x
        && mouse.column < area.right()
        && mouse.row >= area.y
        && mouse.row < area.bottom();
    if !over {
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            editor.debug.unfocus();
        }
        return false;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            enter(editor);
            let Some(hit) = super::render::hit(editor, mouse.column, mouse.row) else {
                return true;
            };
            match hit {
                super::render::Hit::Tab(section) => editor.debug.select(section),
                super::render::Hit::Row(section, row) => {
                    let again =
                        editor.debug.section == section && editor.debug.cursor(section).row == row;
                    editor.debug.select(section);
                    move_to(editor, row);
                    if again {
                        let _ = activate(editor);
                    }
                }
            }
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let by = if mouse.kind == MouseEventKind::ScrollDown {
                3
            } else {
                -3
            };
            if let Some(super::render::Hit::Row(section, _)) =
                super::render::hit(editor, mouse.column, mouse.row)
            {
                editor.debug.select(section);
            }
            move_cursor(editor, by);
        }
        _ => {}
    }
    true
}

/// The file and 0-based line of the cursor in the focused split.
pub fn cursor_line(editor: &Editor) -> Option<(PathBuf, usize)> {
    let (view, doc) = vortex_view::current_ref!(editor);
    let line = doc
        .selection(view.id)
        .primary()
        .cursor_line(doc.text().slice(..));
    Some((doc.path()?.to_owned(), line))
}

/// `u`: the step granularity, when the adapter offers one.
fn cycle_granularity(editor: &mut Editor) {
    let session = playback::spectated(editor)
        .map(|key| key.session)
        .or_else(|| editor.debug_adapters.get_active_client().map(|c| c.id()));
    if !session.is_some_and(|session| playback::supports_granularity(editor, session)) {
        editor.set_error("This debug adapter steps by line only (no stepping granularity)");
        return;
    }
    let spectator = &mut editor.debug.spectator;
    spectator.granularity = spectator.granularity.next();
    let label = spectator.granularity.label();
    crate::debug::mode::say(editor, format!("Steps go by {label}"));
}

/// `*`: marks the variable under the cursor for fast forward (or unmarks it).
fn toggle_mark(editor: &mut Editor) {
    let Some(Target::Var(path)) = current_target(editor) else {
        return;
    };
    if path.tree != Tree::Scopes || path.names.len() < 2 {
        editor.set_error(
            "Mark a variable (not a scope or a watch): fast forward stops when it changes",
        );
        return;
    }
    let marked = &mut editor.debug.spectator.marked;
    if !marked.remove(&path.names) {
        marked.insert(path.names.clone());
        crate::debug::mode::say(editor, "Marked: fast forward stops when it changes");
    } else {
        crate::debug::mode::say(editor, "Unmarked");
    }
}

/// The rows of the capture settings: the caps, then what a stop costs.
pub fn settings_rows(editor: &Editor) -> Vec<Row> {
    let spectator = &editor.debug.spectator;
    let caps = spectator.caps;
    let mut rows = vec![Row {
        depth: 0,
        marker: debug::Marker::None,
        parts: vec![
            ("Capture settings".into(), Role::Heading),
            ("  what each stop records for rewind".into(), Role::Dim),
        ],
        target: Target::None,
    }];
    for (index, setting) in Setting::ALL.iter().enumerate() {
        rows.push(Row {
            depth: 1,
            marker: debug::Marker::None,
            parts: vec![
                (format!("{:<17}", setting.title()), Role::Name),
                (caps.show(*setting), Role::Value),
            ],
            target: Target::ConsoleLine(index),
        });
    }
    let cost = match spectator.history.cost() {
        Some((requests, bytes)) => format!(
            "{requests:.0} requests per stop · {} per stop · history {} ({} stops), measured",
            format_bytes(bytes),
            format_bytes(spectator.history.bytes()),
            spectator.history.stops.len()
        ),
        None => {
            let (requests, bytes) = debug::history::estimate(&caps, 8, 2, 12);
            // each memory view: its window, read a page at a time
            let views = super::memory::open_views(editor);
            let window = caps.memory.unwrap_or(16 * 1024);
            let requests = requests + views * window.div_ceil(1024);
            let bytes = bytes + views * window * 2;
            let views = match views {
                0 => String::new(),
                1 => ", 1 memory view".into(),
                n => format!(", {n} memory views"),
            };
            format!(
                "about {requests} requests and {} per stop (8 frames{views}, estimated)",
                format_bytes(bytes)
            )
        }
    };
    rows.push(Row {
        depth: 1,
        marker: debug::Marker::None,
        parts: vec![("cost             ".into(), Role::Name), (cost, Role::Dim)],
        target: Target::None,
    });
    if caps.is_unlimited() {
        rows.push(Row {
            depth: 1,
            marker: debug::Marker::None,
            parts: vec![(
                "unlimited: a deep or big value can take thousands of requests per stop and slow playback down a lot".into(),
                Role::Error,
            )],
            target: Target::None,
        });
    }
    rows
}

/// Keys while the capture settings are open: `j`/`k` choose, `h`/`l` (or `-`/`+`) change, Esc,
/// `q` or `R` close.
fn settings_key(editor: &mut Editor, key: KeyEvent) {
    use KeyCode::*;
    let Some(row) = editor.debug.spectator.settings else {
        return;
    };
    let setting = Setting::ALL[row.min(Setting::ALL.len() - 1)];
    match key.code {
        Esc | Char('q') | Char('R') => editor.debug.spectator.settings = None,
        Char('j') | Down => {
            editor.debug.spectator.settings = Some((row + 1).min(Setting::ALL.len() - 1))
        }
        Char('k') | Up => editor.debug.spectator.settings = Some(row.saturating_sub(1)),
        Char('l') | Right | Char('+') | Char('=') => change_setting(editor, setting, 1),
        Char('h') | Left | Char('-') => change_setting(editor, setting, -1),
        Char('g') if key.modifiers == KeyModifiers::CONTROL => {
            editor.debug.spectator.settings = None;
            editor.debug.unfocus();
        }
        _ => {}
    }
}

fn change_setting(editor: &mut Editor, setting: Setting, by: isize) {
    let spectator = &mut editor.debug.spectator;
    spectator.caps.change(setting, by);
    if setting == Setting::History {
        let limit = spectator.caps.history;
        spectator.history.trim(limit);
    }
}
