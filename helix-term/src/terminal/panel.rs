//! Starting the shell, focus, and the events that go to the panel instead of the editor.

use helix_core::{pos_at_coords, Position, Selection};
use helix_view::{
    align_view,
    editor::Action,
    graphics::Rect,
    input::{Event, KeyEvent, MouseButton, MouseEventKind},
    keyboard::{KeyCode, KeyModifiers},
    terminal::{Shell, ShellEvent, ShellOptions, Size, Waiter},
    Align, Editor,
};
use tokio::sync::mpsc::unbounded_channel;

use super::{
    input::{encode_key, encode_mouse, encode_paste, MouseAction},
    shim,
};
use crate::{compositor::Compositor, job};

/// `C-t`: focuses the panel, or goes back to the editor. In the panel it's the one key that
/// doesn't go to the shell.
pub const TOGGLE_KEY: KeyEvent = KeyEvent {
    code: KeyCode::Char('t'),
    modifiers: KeyModifiers::CONTROL,
};

/// The height of the panel's screen in an editor area of `height` rows, leaving the splits at
/// least a few rows.
pub fn screen_height(editor: &Editor, height: u16) -> u16 {
    let percent = editor.config().terminal.height.clamp(10, 90);
    let panel = (height as u32 * percent as u32 / 100) as u16;
    // the splits keep at least 3 rows (a line and a statusline)
    panel.min(height.saturating_sub(3)).max(1)
}

/// Starts the shell when none runs. An error (the shell can't start) is shown in the statusline.
pub fn ensure_shell(editor: &mut Editor) -> bool {
    if editor.terminal.shell.is_some() {
        return true;
    }
    match spawn(editor) {
        Ok(()) => true,
        Err(err) => {
            editor.set_error(format!("Couldn't start the shell: {err}"));
            false
        }
    }
}

fn spawn(editor: &mut Editor) -> anyhow::Result<()> {
    let config = editor.config().terminal.clone();
    let area = editor.tree.area();
    let size = Size {
        rows: screen_height(editor, area.height).max(1),
        cols: area.width.max(1),
    };

    // `hx` in the shell talks to this editor; started first, so its directory goes on PATH
    let server = match shim::Server::start(|request, reply| {
        tokio::spawn(job::dispatch(move |editor, _| {
            handle_shim_request(editor, request, reply)
        }));
    }) {
        Ok(server) => Some(server),
        Err(err) => {
            log::warn!("terminal: no `hx` shim: {err}");
            None
        }
    };

    let options = ShellOptions {
        command: config.shell,
        cwd: Some(helix_stdx::env::current_working_dir()),
        env: server
            .as_ref()
            .map(|server| server.env())
            .unwrap_or_default(),
        path_prefix: server.as_ref().map(|server| server.bin_dir().to_owned()),
        integration: config.shell_integration,
        size,
        scrollback: config.scrollback,
    };
    let (sender, mut receiver) = unbounded_channel();
    let shell = Shell::spawn(options, sender)?;
    let pid = shell.pid();

    tokio::spawn(async move {
        // the server lives as long as the shell: `hx` can't reach a dead one
        let _server = server;
        while let Some(event) = receiver.recv().await {
            match event {
                ShellEvent::Output | ShellEvent::Status(_) | ShellEvent::Title(_) => {
                    helix_event::request_redraw()
                }
                ShellEvent::Exited(_) => {
                    job::dispatch(move |editor, _| {
                        // a shell started since doesn't close
                        if editor.terminal.shell.as_ref().map(Shell::pid) == Some(pid) {
                            editor.terminal.shell_exited();
                        }
                    })
                    .await;
                    break;
                }
            }
        }
    });

    editor.terminal.shell = Some(shell);
    Ok(())
}

/// `C-t`: from the editor, shows and focuses the panel (starting the shell when needed); from
/// the panel, goes back to the editor.
pub fn toggle_focus(editor: &mut Editor) {
    if editor.terminal.is_focused() {
        editor.terminal.unfocus();
    } else if ensure_shell(editor) {
        editor.terminal.focus();
    }
}

/// `:sh command`: types the command in the shell and presses Enter, and focuses the panel.
pub fn run(editor: &mut Editor, command: &str) {
    if !ensure_shell(editor) {
        return;
    }
    editor.terminal.focus();
    if let Some(shell) = &editor.terminal.shell {
        if !command.is_empty() {
            shell.write(format!("{command}\r").as_bytes());
        }
    }
}

/// Sends the event to the panel when it is the panel's: keys and pastes while it is focused,
/// the mouse over it. Returns whether it was.
pub fn handle_event(event: &Event, editor: &mut Editor, compositor: &mut Compositor) -> bool {
    let _ = compositor;
    let scroll_lines = editor.config().scroll_lines.unsigned_abs().max(1) as u16;
    let panel = &mut editor.terminal;
    let Some(shell) = &panel.shell else {
        return false;
    };
    match event {
        Event::Key(key) if panel.is_focused() => {
            if *key == TOGGLE_KEY {
                panel.unfocus();
            } else if let Some(bytes) = encode_key(*key, &shell.input_modes()) {
                shell.write(&bytes);
            }
            true
        }
        Event::Paste(text) if panel.is_focused() => {
            shell.write(&encode_paste(text, &shell.input_modes()));
            true
        }
        Event::Mouse(mouse) if panel.is_visible() => {
            let area = panel.area;
            let over = mouse.column >= area.x
                && mouse.column < area.right()
                && mouse.row >= area.y
                && mouse.row < area.bottom();
            if !over {
                if matches!(mouse.kind, MouseEventKind::Down(_)) {
                    panel.unfocus();
                }
                return false;
            }
            let clicked = matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left));
            if clicked && !panel.focused {
                panel.focus();
            }
            let Some(shell) = &panel.shell else {
                return true;
            };
            let col = mouse.column - area.x;
            let row = mouse.row - area.y;
            match encode_mouse(mouse, col, row, scroll_lines, &shell.input_modes()) {
                MouseAction::Send(bytes) => shell.write(&bytes),
                MouseAction::Scroll(lines) => shell.scroll(lines),
                MouseAction::None => {}
            }
            true
        }
        _ => false,
    }
}

/// `hx` in the shell: opens files, or just gives the editor the keys.
fn handle_shim_request(editor: &mut Editor, request: shim::Request, reply: shim::Reply) {
    let (files, wait) = match request {
        shim::Request::Focus => {
            editor.terminal.unfocus();
            return reply.send(shim::Response::Done);
        }
        shim::Request::Open { files, wait } => (files, wait),
    };
    let mut opened = Vec::new();
    for file in files {
        match editor.open(&file.path, Action::Replace) {
            Ok(doc) => {
                let (view, document) = helix_view::current!(editor);
                if let Some(line) = file.line {
                    let position = Position::new(
                        line.saturating_sub(1),
                        file.column.unwrap_or(1).saturating_sub(1),
                    );
                    let pos = pos_at_coords(document.text().slice(..), position, true);
                    document.set_selection(view.id, Selection::point(pos));
                    align_view(document, view, Align::Center);
                }
                opened.push(doc);
            }
            Err(err) => {
                let message = format!("{}: {err}", file.path.display());
                editor.set_error(message.clone());
                return reply.send(shim::Response::Error(message));
            }
        }
    }
    editor.terminal.unfocus();
    if !wait || opened.is_empty() {
        return reply.send(shim::Response::Done);
    }
    // `$EDITOR`: done when all of them are closed
    let reply = std::sync::Arc::new(parking_lot::Mutex::new((opened.len(), Some(reply))));
    for doc in opened {
        let reply = reply.clone();
        editor.terminal.waiters.push(Waiter {
            doc,
            done: Box::new(move || {
                let mut reply = reply.lock();
                reply.0 -= 1;
                if reply.0 == 0 {
                    if let Some(reply) = reply.1.take() {
                        reply.send(shim::Response::Done);
                    }
                }
            }),
        });
    }
    editor.set_status("Close the buffer (:q or :wq) to go back to the terminal");
}

/// The screen of the panel in the area left below the splits, when the panel is shown: the
/// splits get `area` without it.
pub fn split_area(editor: &Editor, area: Rect) -> (Rect, Option<Rect>) {
    if !editor.terminal.is_visible() || area.height < 6 {
        return (area, None);
    }
    let screen = screen_height(editor, area.height);
    let editor_area = area.clip_bottom(screen);
    let panel = Rect::new(area.x, editor_area.bottom(), area.width, screen);
    (editor_area, Some(panel))
}
