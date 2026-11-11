//! The keys and commands of the hex view (this fork). A document in the hex view shows its bytes
//! (see `ui/hex_view.rs` and `vortex_view::hex`); its keys are handled here, before the keymap,
//! which still gets the ones the view leaves alone: `:`, `space`, and the keys with `Ctrl` or
//! `Alt` that it doesn't use (`C-w` for the windows, `C-o`/`C-i` for the jumplist...). The text of
//! the document can't change meanwhile (`Document::apply` refuses), so no command reaching the
//! keymap can edit it unseen.
//!
//! Read-only (`RO`, the default): typing a character only says how to overwrite. `i` enters
//! overwrite mode (`OVR`), where typing overwrites the byte under the cursor, in the column the
//! cursor is in; `Esc` leaves it.

use vortex_core::movement::Direction;
use vortex_view::{
    document::Mode,
    editor::Editor,
    graphics::Rect,
    hex::{self, Column, HexBuffer, HexCursor, Layout, Rows},
    input::{KeyEvent, MouseButton, MouseEvent, MouseEventKind},
    keyboard::{KeyCode, KeyModifiers},
    ViewId,
};

use super::Context;
use crate::{
    compositor::{self, EventResult},
    ui::{self, hex_view, PromptEvent},
};

/// The most bytes `y` yanks (their hex text is three times as long).
const MAX_YANK: u64 = 16 * 1024 * 1024;

/// What a typed character does in read-only mode.
pub const READ_ONLY: &str = "read-only, i to overwrite";

/// The key state of the hex view kept between keys: the first key of `g` sequences.
#[derive(Default)]
pub struct HexKeys {
    pending: Option<char>,
}

/// The focused view, if its document is in the hex view, with what moving in it needs.
struct Target<'a> {
    hex: &'a mut HexBuffer,
    view: ViewId,
    rows: Rows,
    /// The number of rows on screen.
    height: u64,
    scrolloff: u64,
}

impl Target<'_> {
    fn of(editor: &mut Editor) -> Option<Target<'_>> {
        let scrolloff = editor.config().scrolloff as u64;
        let (view, doc) = current!(editor);
        let area = hex_view::rows_area(view);
        let view = view.id;
        let hex = doc.hex_mut()?;
        let layout = hex.cursor(view).layout;
        let rows = Rows::new(layout, hex.len(), area.width);
        Some(Target {
            hex,
            view,
            rows,
            height: (area.height as u64).max(1),
            scrolloff,
        })
    }

    fn cursor(&self) -> HexCursor {
        self.hex.cursor(self.view)
    }

    fn last(&self) -> u64 {
        self.hex.len().saturating_sub(1)
    }

    fn per_row(&self) -> u64 {
        self.rows.bytes_per_row as u64
    }

    /// Moves the cursor to `offset` (at most the last byte), extending the selection while
    /// selecting, and scrolls to it.
    fn move_to(&mut self, offset: u64) {
        let last = self.last();
        let cursor = self.hex.cursor_mut(self.view);
        cursor.offset = offset.min(last);
        cursor.digit = 0;
        self.scroll_to_cursor();
    }

    /// Moves by `rows` rows (down when positive), staying in the file.
    fn move_rows(&mut self, rows: i64) {
        let cursor = self.cursor();
        let row = self.rows.row(cursor.offset) as i64;
        let count = self.rows.count(self.hex.len()) as i64;
        let target = (row + rows).clamp(0, count - 1);
        if target == row {
            return;
        }
        let offset = cursor.offset as i64 + (target - row) * self.per_row() as i64;
        self.move_to(offset as u64);
    }

    /// Scrolls so that the cursor is on screen, `scrolloff` rows from its edges when it can.
    fn scroll_to_cursor(&mut self) {
        let height = self.height;
        let scrolloff = self.scrolloff.min(height.saturating_sub(1) / 2);
        let count = self.rows.count(self.hex.len());
        let rows = self.rows;
        let cursor = self.hex.cursor_mut(self.view);
        let row = rows.row(cursor.offset);
        let mut top = rows.row(cursor.top);
        if row < top + scrolloff {
            top = row.saturating_sub(scrolloff);
        }
        if row + scrolloff >= top + height {
            top = row + scrolloff + 1 - height;
        }
        top = top.min(count.saturating_sub(height));
        cursor.top = top * rows.bytes_per_row as u64;
    }

    /// Scrolls by `rows` rows (down when positive), the cursor following when it would leave
    /// the screen.
    fn scroll(&mut self, rows: i64) {
        let count = self.rows.count(self.hex.len());
        let max_top = count.saturating_sub(self.height) as i64;
        let per_row = self.per_row();
        let height = self.height;
        let scrolloff = self.scrolloff.min(height.saturating_sub(1) / 2);
        let layout = self.rows;
        let last = self.last();
        let cursor = self.hex.cursor_mut(self.view);
        let top = (layout.row(cursor.top) as i64 + rows).clamp(0, max_top) as u64;
        cursor.top = top * per_row;
        let row = layout.row(cursor.offset);
        // (at the edges of the file, the cursor can go to the edge of the screen)
        let first = if top == 0 { 0 } else { top + scrolloff };
        let last_row = if top as i64 == max_top {
            top + height - 1
        } else {
            (top + height - 1).saturating_sub(scrolloff).max(first)
        };
        let target = row.clamp(first, last_row);
        if target != row {
            let column = cursor.offset % per_row;
            cursor.offset = (target * per_row + column).min(last);
            cursor.digit = 0;
        }
    }
}

/// Handles a key for a focused document in the hex view. Returns false for the keys the view
/// leaves to the keymap (and for documents not in the hex view).
pub fn handle_key(cx: &mut Context, key: KeyEvent, keys: &mut HexKeys) -> bool {
    if !doc!(cx.editor).is_hex() {
        keys.pending = None;
        return false;
    }
    // the view has no insert or select mode of the editor's
    if cx.editor.mode != Mode::Normal {
        cx.editor.mode = Mode::Normal;
    }
    let Some(target) = Target::of(cx.editor) else {
        return false;
    };
    if target.cursor().overwrite {
        keys.pending = None;
        return overwrite_key(cx, key);
    }
    if let Some(prefix) = keys.pending.take() {
        goto_key(cx, prefix, key);
        return true;
    }
    read_only_key(cx, key, keys)
}

fn read_only_key(cx: &mut Context, key: KeyEvent, keys: &mut HexKeys) -> bool {
    use KeyCode::*;
    let Some(mut target) = Target::of(cx.editor) else {
        return false;
    };
    let cursor = target.cursor();
    let per_row = target.per_row();
    let half = (target.height / 2).max(1) as i64;
    let page = target.height as i64;
    let ctrl = key.modifiers == KeyModifiers::CONTROL;
    let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();

    match key.code {
        Char('d') if ctrl => {
            target.scroll(half);
            target.move_rows(half);
        }
        Char('u') if ctrl => {
            target.scroll(-half);
            target.move_rows(-half);
        }
        Char('f') if ctrl => {
            target.scroll(page);
            target.move_rows(page);
        }
        Char('b') if ctrl => {
            target.scroll(-page);
            target.move_rows(-page);
        }
        PageDown => {
            target.scroll(page);
            target.move_rows(page);
        }
        PageUp => {
            target.scroll(-page);
            target.move_rows(-page);
        }
        _ if !plain => return false,
        Char('h') | Left => target.move_to(cursor.offset.saturating_sub(1)),
        Char('l') | Right => target.move_to(cursor.offset + 1),
        Char('j') | Down => target.move_rows(1),
        Char('k') | Up => target.move_rows(-1),
        // a word is 4 bytes, from a multiple of 4
        Char('w') => target.move_to((cursor.offset / 4 + 1) * 4),
        Char('b') => target.move_to(cursor.offset.saturating_sub(1) / 4 * 4),
        Home => target.move_to(cursor.offset / per_row * per_row),
        End => target.move_to(cursor.offset / per_row * per_row + per_row - 1),
        Char('g') => keys.pending = Some('g'),
        Tab => {
            let cursor = target.hex.cursor_mut(target.view);
            cursor.column = match cursor.column {
                Column::Bytes => Column::Ascii,
                Column::Ascii => Column::Bytes,
            };
        }
        Char('~') => {
            let cursor = target.hex.cursor_mut(target.view);
            cursor.layout = match cursor.layout {
                Layout::Hex => Layout::Bin,
                Layout::Bin => Layout::Hex,
            };
            // the rows change width
            if let Some(mut target) = Target::of(cx.editor) {
                target.scroll_to_cursor();
            }
        }
        Char('v') => {
            let cursor = target.hex.cursor_mut(target.view);
            cursor.anchor = match cursor.anchor {
                Some(_) => None,
                None => Some(cursor.offset),
            };
        }
        Char(';') | Esc => target.hex.cursor_mut(target.view).anchor = None,
        Char('y') => yank(cx),
        Char('n') => search_next(cx.editor, Direction::Forward),
        Char('N') => search_next(cx.editor, Direction::Backward),
        Char('/') => search_prompt(cx, Direction::Forward),
        Char('?') => search_prompt(cx, Direction::Backward),
        Char('G') => goto_prompt(cx),
        Char('u') => match target.hex.undo() {
            Some(offset) => target.move_to(offset),
            None => cx.editor.set_status("Already at oldest change"),
        },
        Char('U') => match target.hex.redo() {
            Some(offset) => target.move_to(offset),
            None => cx.editor.set_status("Already at newest change"),
        },
        Char('i') => {
            if target.hex.is_empty() {
                cx.editor
                    .set_status("the file is empty: no byte to overwrite");
            } else {
                let cursor = target.hex.cursor_mut(target.view);
                cursor.overwrite = true;
                cursor.anchor = None;
                cursor.digit = 0;
            }
        }
        // the command line, the space menu and the register for `y`
        Char(':') | Char(' ') | Char('"') => return false,
        Char(_) => cx.editor.set_status(READ_ONLY),
        _ => return false,
    }
    true
}

/// The second key of a `g` sequence.
fn goto_key(cx: &mut Context, prefix: char, key: KeyEvent) {
    let Some(mut target) = Target::of(cx.editor) else {
        return;
    };
    let offset = target.cursor().offset;
    let per_row = target.per_row();
    match (prefix, key.code) {
        ('g', KeyCode::Char('g')) => target.move_to(0),
        ('g', KeyCode::Char('e')) => target.move_to(u64::MAX),
        ('g', KeyCode::Char('h')) => target.move_to(offset / per_row * per_row),
        ('g', KeyCode::Char('l')) => target.move_to(offset / per_row * per_row + per_row - 1),
        _ => (),
    }
}

/// A key in overwrite mode: typing overwrites bytes, the arrows move by digit.
fn overwrite_key(cx: &mut Context, key: KeyEvent) -> bool {
    use KeyCode::*;
    let Some(mut target) = Target::of(cx.editor) else {
        return false;
    };
    let cursor = target.cursor();
    let digits = cursor.layout.digits();
    let page = target.height as i64;
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return false;
    }
    match key.code {
        Esc => {
            target.hex.commit();
            let cursor = target.hex.cursor_mut(target.view);
            cursor.overwrite = false;
            cursor.digit = 0;
        }
        Left | Backspace => {
            if cursor.column == Column::Bytes && cursor.digit > 0 {
                target.hex.cursor_mut(target.view).digit -= 1;
            } else if cursor.offset > 0 {
                target.move_to(cursor.offset - 1);
                if cursor.column == Column::Bytes {
                    target.hex.cursor_mut(target.view).digit = digits - 1;
                }
            }
        }
        Right => {
            if cursor.column == Column::Bytes && cursor.digit + 1 < digits {
                target.hex.cursor_mut(target.view).digit += 1;
            } else if cursor.offset < target.last() {
                target.move_to(cursor.offset + 1);
            }
        }
        Down => target.move_rows(1),
        Up => target.move_rows(-1),
        PageDown => {
            target.scroll(page);
            target.move_rows(page);
        }
        PageUp => {
            target.scroll(-page);
            target.move_rows(-page);
        }
        Home => target.move_to(cursor.offset / target.per_row() * target.per_row()),
        End => target
            .move_to(cursor.offset / target.per_row() * target.per_row() + target.per_row() - 1),
        Tab => {
            let cursor = target.hex.cursor_mut(target.view);
            cursor.column = match cursor.column {
                Column::Bytes => Column::Ascii,
                Column::Ascii => Column::Bytes,
            };
            cursor.digit = 0;
        }
        Char(c) => {
            if let Err(message) = type_char(&mut target, c) {
                cx.editor.set_status(message);
            }
        }
        _ => (),
    }
    true
}

/// Overwrites with a typed character, in the column of the cursor: a hex digit overwrites a
/// nibble, a bit a bit, and in the ASCII column a character its byte (or bytes, past ASCII).
fn type_char(target: &mut Target, c: char) -> Result<(), &'static str> {
    let cursor = target.cursor();
    let last = target.last();
    let Some(byte) = target.hex.byte(cursor.offset) else {
        return Ok(());
    };
    match (cursor.column, cursor.layout) {
        (Column::Bytes, layout) => {
            let value = match layout {
                Layout::Hex => {
                    let nibble = c
                        .to_digit(16)
                        .ok_or("type hex digits here (0-9, a-f), or Tab for the ASCII column")?
                        as u8;
                    if cursor.digit == 0 {
                        (nibble << 4) | (byte & 0x0f)
                    } else {
                        (byte & 0xf0) | nibble
                    }
                }
                Layout::Bin => {
                    let mask = 0x80 >> cursor.digit;
                    match c {
                        '0' => byte & !mask,
                        '1' => byte | mask,
                        _ => return Err("type 0 or 1 here, or Tab for the ASCII column"),
                    }
                }
            };
            target.hex.overwrite(cursor.offset, value);
            let digits = layout.digits();
            let cursor = target.hex.cursor_mut(target.view);
            if cursor.digit + 1 < digits {
                cursor.digit += 1;
            } else if cursor.offset < last {
                cursor.offset += 1;
                cursor.digit = 0;
            }
        }
        (Column::Ascii, _) => {
            let mut buf = [0; 4];
            let encoded = c.encode_utf8(&mut buf).as_bytes();
            // all of the character or nothing: half of it would be a broken UTF-8 sequence
            if cursor.offset + encoded.len() as u64 - 1 > last {
                return Err("no room for this character before the end of the file");
            }
            for (i, &byte) in encoded.iter().enumerate() {
                target.hex.overwrite(cursor.offset + i as u64, byte);
            }
            target.hex.cursor_mut(target.view).offset =
                (cursor.offset + encoded.len() as u64).min(last);
        }
    }
    target.scroll_to_cursor();
    Ok(())
}

/// `y`: yanks the selection (or the byte under the cursor) as hex text, `de ad be ef`.
fn yank(cx: &mut Context) {
    // (`"a` before `y` goes through the keymap, which leaves the register in the editor)
    let register = cx
        .register
        .or_else(|| cx.editor.selected_register.take())
        .unwrap_or(cx.editor.config().default_yank_register);
    let Some(target) = Target::of(cx.editor) else {
        return;
    };
    let cursor = target.cursor();
    if target.hex.is_empty() {
        cx.editor.set_status("the file is empty: nothing to yank");
        return;
    }
    let selection = cursor.selection();
    let len = selection.end - selection.start;
    if len > MAX_YANK {
        cx.editor.set_error(format!(
            "too many bytes to yank: {}, at most {}",
            hex_view::size(len),
            hex_view::size(MAX_YANK)
        ));
        return;
    }
    let text = hex::hex_text(&target.hex.bytes(selection));
    target.hex.cursor_mut(target.view).anchor = None;
    match cx.editor.registers.write(register, vec![text]) {
        Ok(()) => cx.editor.set_status(format!(
            "yanked {} {} as hex to register {register}",
            len,
            if len == 1 { "byte" } else { "bytes" }
        )),
        Err(err) => cx.editor.set_error(err.to_string()),
    }
}

// Searching

fn search_prompt(cx: &mut Context, direction: Direction) {
    let prompt = ui::Prompt::new(
        match direction {
            Direction::Forward => "search bytes:".into(),
            Direction::Backward => "reverse search bytes:".into(),
        },
        Some('/'),
        ui::completers::none,
        move |cx: &mut compositor::Context, input: &str, event: PromptEvent| {
            if event != PromptEvent::Validate || input.is_empty() {
                return;
            }
            match hex::parse_pattern(input) {
                Ok(pattern) => {
                    if let Some(hex) = doc_mut!(cx.editor).hex_mut() {
                        hex.search = Some(pattern);
                    }
                    search_next(cx.editor, direction);
                }
                Err(message) => cx.editor.set_error(message),
            }
        },
    );
    cx.push_layer(Box::new(prompt));
}

/// Moves to the next match of the last search, after the cursor (forward) or before it.
fn search_next(editor: &mut Editor, direction: Direction) {
    let Some(mut target) = Target::of(editor) else {
        return;
    };
    let Some(pattern) = target.hex.search.clone() else {
        editor.set_status("no search yet: / searches hex bytes or \"text\"");
        return;
    };
    let offset = target.cursor().offset;
    let found = match direction {
        Direction::Forward => target.hex.find(&pattern, offset + 1, true),
        Direction::Backward => target.hex.find(&pattern, offset, false),
    };
    match found {
        Some((at, wrapped)) => {
            target.move_to(at);
            if wrapped {
                editor.set_status("Wrapped around document");
            }
        }
        None => editor.set_error(format!("not found: {}", hex::hex_text(&pattern))),
    }
}

// Going to an offset

fn goto_prompt(cx: &mut Context) {
    let prompt = ui::Prompt::new(
        "goto offset:".into(),
        None,
        ui::completers::none,
        |cx: &mut compositor::Context, input: &str, event: PromptEvent| {
            if event == PromptEvent::Validate && !input.trim().is_empty() {
                if let Err(err) = goto(cx.editor, input) {
                    cx.editor.set_error(err.to_string());
                }
            }
        },
    );
    cx.push_layer(Box::new(prompt));
}

/// Moves the cursor of the focused view to an offset, hex after `0x` (`0x1a4`), else decimal.
/// An offset past the end goes to the last byte. Fails for a document not in the hex view.
pub fn goto(editor: &mut Editor, input: &str) -> anyhow::Result<()> {
    let offset = hex::parse_offset(input).map_err(anyhow::Error::msg)?;
    let Some(mut target) = Target::of(editor) else {
        anyhow::bail!("not in the hex view");
    };
    let len = target.hex.len();
    target.move_to(offset);
    if offset >= len {
        editor.set_status(format!(
            "0x{offset:X} is past the end ({}): at the last byte",
            hex_view::size(len)
        ));
    }
    Ok(())
}

// The mouse

/// Handles a mouse event over a view whose document is in the hex view: the wheel scrolls it,
/// a click puts the cursor on a byte (in the column clicked), dragging selects. `None` for the
/// events over other views, which the editor handles.
pub fn handle_mouse(editor: &mut Editor, event: &MouseEvent) -> Option<EventResult> {
    let MouseEvent {
        kind, row, column, ..
    } = *event;
    let view_id = match kind {
        // dragging goes on in the view where it started
        MouseEventKind::Drag(MouseButton::Left) => view!(editor).id,
        _ => editor.tree.views().find_map(|(view, _)| {
            let area = hex_view::rows_area(view);
            let inside = (area.left()..area.right()).contains(&column)
                && (area.top()..area.bottom()).contains(&row);
            inside.then_some(view.id)
        })?,
    };
    let doc_id = editor.tree.get(view_id).doc;
    if !editor.documents[&doc_id].is_hex() {
        return None;
    }
    let lines = editor.config().scroll_lines.max(1) as i64;
    match kind {
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let focused = editor.tree.focus;
            editor.tree.focus = view_id;
            if let Some(mut target) = Target::of(editor) {
                target.scroll(if kind == MouseEventKind::ScrollDown {
                    lines
                } else {
                    -lines
                });
            }
            editor.tree.focus = focused;
        }
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
            if matches!(kind, MouseEventKind::Down(_)) {
                editor.focus(view_id);
            }
            let area = hex_view::rows_area(editor.tree.get(view_id));
            let mut target = Target::of(editor)?;
            let Some((offset, clicked)) = byte_at(&target, area, column, row) else {
                return Some(EventResult::Consumed(None));
            };
            let cursor = target.hex.cursor_mut(target.view);
            if matches!(kind, MouseEventKind::Down(_)) {
                cursor.anchor = None;
                cursor.column = clicked;
            } else if cursor.anchor.is_none() {
                cursor.anchor = Some(cursor.offset);
            }
            target.move_to(offset);
        }
        _ => return None,
    }
    Some(EventResult::Consumed(None))
}

/// The byte drawn at a screen position, and its column.
fn byte_at(target: &Target, area: Rect, column: u16, row: u16) -> Option<(u64, Column)> {
    let rows = target.rows;
    let x = column.checked_sub(area.x)? as usize;
    let y = row.checked_sub(area.y)? as u64;
    let row_start = (rows.row(target.cursor().top) + y) * rows.bytes_per_row as u64;
    let (i, clicked) = if x >= rows.ascii_x() {
        (x - rows.ascii_x(), Column::Ascii)
    } else {
        let i = (0..rows.bytes_per_row)
            .rev()
            .find(|&i| rows.byte_x(i) <= x)?;
        (i, Column::Bytes)
    };
    let offset = row_start + i.min(rows.bytes_per_row - 1) as u64;
    (offset < target.hex.len()).then_some((offset, clicked))
}

/// `:hex`: shows the current document in the hex view, or back as text.
pub fn toggle(cx: &mut compositor::Context) -> anyhow::Result<()> {
    let trust_full = super::typed::doc_trust_full(cx.editor);
    let scrolloff = cx.editor.config().scrolloff;
    let (view, doc) = current!(cx.editor);
    if doc.is_hex() {
        doc.leave_hex()?;
        // the text as the file is now (empty until now for a file opened in the hex view)
        doc.reload(view, &cx.editor.diff_providers, trust_full)?;
        view.ensure_cursor_in_view(doc, scrolloff);
    } else {
        doc.enter_hex()?;
    }
    Ok(())
}
