//! Encodes Helix's key, mouse and paste events as the bytes a program in a terminal reads, in
//! the modes it asked for ([`InputModes`]).
//!
//! The encoding is that of xterm (`TERM=xterm-256color`): legacy control characters for
//! `Ctrl`, an `ESC` prefix for `Alt`, `ESC [ 1 ; <m> A`-style sequences for modified cursor
//! and function keys, and the X10, UTF-8 and SGR mouse protocols.

use helix_view::{
    input::{KeyEvent, MouseButton, MouseEvent, MouseEventKind},
    keyboard::{KeyCode, KeyModifiers},
    terminal::{InputModes, MouseTracking},
};

const ESC: u8 = 0x1b;

/// The xterm modifier parameter: 1 + shift 1 + alt 2 + ctrl 4 + super 8.
fn modifier_param(mods: KeyModifiers) -> u8 {
    1 + u8::from(mods.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(mods.contains(KeyModifiers::ALT))
        + 4 * u8::from(mods.contains(KeyModifiers::CONTROL))
        + 8 * u8::from(mods.contains(KeyModifiers::SUPER))
}

/// `ESC [ <final>`, `ESC O <final>` in application cursor mode, or `ESC [ 1 ; <m> <final>`
/// with modifiers.
fn cursor_key(final_byte: u8, mods: KeyModifiers, app_cursor: bool) -> Vec<u8> {
    if !mods.is_empty() {
        format!("\x1b[1;{}{}", modifier_param(mods), final_byte as char).into_bytes()
    } else if app_cursor {
        vec![ESC, b'O', final_byte]
    } else {
        vec![ESC, b'[', final_byte]
    }
}

/// `ESC [ <n> ~` or `ESC [ <n> ; <m> ~`.
fn tilde_key(n: u8, mods: KeyModifiers) -> Vec<u8> {
    if mods.is_empty() {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{}~", modifier_param(mods)).into_bytes()
    }
}

/// The byte a control character is sent as, for `Ctrl` plus `c`.
fn ctrl_char(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

/// The bytes for a key, or `None` for keys a terminal doesn't send (CapsLock, media keys...).
pub fn encode_key(key: KeyEvent, modes: &InputModes) -> Option<Vec<u8>> {
    let mut mods = key.modifiers;
    let alt = mods.contains(KeyModifiers::ALT);
    // The ESC prefix of Alt is added at the end, for the keys that use it.
    let mut prefix_alt = alt;
    let bytes = match key.code {
        KeyCode::Char(c) => {
            // The character already carries the effect of Shift.
            mods.remove(KeyModifiers::SHIFT);
            match ctrl_char(c).filter(|_| mods.contains(KeyModifiers::CONTROL)) {
                Some(b) => vec![b],
                None => c.to_string().into_bytes(),
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => b"\x1b[Z".to_vec(),
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace if mods.contains(KeyModifiers::CONTROL) => vec![0x08],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![ESC],
        KeyCode::Null => vec![0x00],
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Right
        | KeyCode::Left
        | KeyCode::Home
        | KeyCode::End => {
            prefix_alt = false;
            let final_byte = match key.code {
                KeyCode::Up => b'A',
                KeyCode::Down => b'B',
                KeyCode::Right => b'C',
                KeyCode::Left => b'D',
                KeyCode::Home => b'H',
                _ => b'F',
            };
            cursor_key(final_byte, mods, modes.app_cursor)
        }
        KeyCode::Insert | KeyCode::Delete | KeyCode::PageUp | KeyCode::PageDown => {
            prefix_alt = false;
            let n = match key.code {
                KeyCode::Insert => 2,
                KeyCode::Delete => 3,
                KeyCode::PageUp => 5,
                _ => 6,
            };
            tilde_key(n, mods)
        }
        KeyCode::F(n @ 1..=4) => {
            prefix_alt = false;
            let final_byte = b'O' + n; // P, Q, R, S
            if mods.is_empty() {
                vec![ESC, b'O', final_byte]
            } else {
                format!("\x1b[1;{}{}", modifier_param(mods), final_byte as char).into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => {
            prefix_alt = false;
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            tilde_key(code, mods)
        }
        _ => return None,
    };
    if prefix_alt {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.push(ESC);
        out.extend(bytes);
        Some(out)
    } else {
        Some(bytes)
    }
}

/// A paste: wrapped in `ESC [ 200 ~` ... `ESC [ 201 ~` in bracketed paste mode (with any
/// `ESC [ 201 ~` inside the text removed so it can't end the paste early); without it,
/// newlines (`\r\n` and `\n`) become `\r` as terminals do.
pub fn encode_paste(text: &str, modes: &InputModes) -> Vec<u8> {
    if modes.bracketed_paste {
        const END: &str = "\x1b[201~";
        let mut text = text.to_owned();
        // Removing one may join the pieces around it into another.
        while text.contains(END) {
            text = text.replace(END, "");
        }
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(END.as_bytes());
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// What to do about a mouse event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseAction {
    /// Write these bytes.
    Send(Vec<u8>),
    /// Scroll the scrollback view by this many lines, up when positive.
    Scroll(i32),
    /// Nothing.
    None,
}

fn button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// The report of a mouse event whose button code (without modifiers) is `code`.
fn mouse_report(
    code: u16,
    release: bool,
    mods: KeyModifiers,
    col: u16,
    row: u16,
    modes: &InputModes,
) -> MouseAction {
    let mut b = code;
    if mods.contains(KeyModifiers::SHIFT) {
        b += 4;
    }
    if mods.contains(KeyModifiers::ALT) {
        b += 8;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        b += 16;
    }
    let (x, y) = (col as u32 + 1, row as u32 + 1);
    if modes.sgr_mouse {
        let end = if release { 'm' } else { 'M' };
        return MouseAction::Send(format!("\x1b[<{b};{x};{y}{end}").into_bytes());
    }
    let mut out = vec![ESC, b'[', b'M'];
    for value in [b as u32, x, y] {
        let value = value + 32;
        if modes.utf8_mouse {
            let Some(c) = char::from_u32(value) else {
                return MouseAction::None;
            };
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else if value <= 255 {
            out.push(value as u8);
        } else {
            return MouseAction::None;
        }
    }
    MouseAction::Send(out)
}

/// The mouse event for the program, or a scroll of the scrollback. `col` and `row` are the
/// 0-based cell in the terminal's screen. `scroll_lines` is how many lines one wheel step
/// scrolls.
pub fn encode_mouse(
    event: &MouseEvent,
    col: u16,
    row: u16,
    scroll_lines: u16,
    modes: &InputModes,
) -> MouseAction {
    let reporting = modes.mouse != MouseTracking::None;
    let mods = event.modifiers;
    let wheel = match event.kind {
        MouseEventKind::ScrollUp => Some(64),
        MouseEventKind::ScrollDown => Some(65),
        MouseEventKind::ScrollLeft => Some(66),
        MouseEventKind::ScrollRight => Some(67),
        _ => None,
    };
    if let Some(code) = wheel {
        if reporting {
            return mouse_report(code, false, mods, col, row, modes);
        }
        let key = match event.kind {
            MouseEventKind::ScrollUp => KeyCode::Up,
            MouseEventKind::ScrollDown => KeyCode::Down,
            _ => return MouseAction::None,
        };
        if modes.alt_screen && modes.alternate_scroll {
            let arrow = encode_key(
                KeyEvent {
                    code: key,
                    modifiers: KeyModifiers::NONE,
                },
                modes,
            )
            .unwrap_or_default();
            return MouseAction::Send(arrow.repeat(scroll_lines as usize));
        }
        let lines = i32::from(scroll_lines);
        return MouseAction::Scroll(if key == KeyCode::Up { lines } else { -lines });
    }
    if !reporting {
        return MouseAction::None;
    }
    match event.kind {
        MouseEventKind::Down(button) => {
            mouse_report(button_code(button), false, mods, col, row, modes)
        }
        MouseEventKind::Up(button) => {
            let code = if modes.sgr_mouse {
                button_code(button)
            } else {
                3
            };
            mouse_report(code, true, mods, col, row, modes)
        }
        MouseEventKind::Drag(button)
            if matches!(modes.mouse, MouseTracking::Drag | MouseTracking::Motion) =>
        {
            mouse_report(button_code(button) + 32, false, mods, col, row, modes)
        }
        MouseEventKind::Moved if modes.mouse == MouseTracking::Motion => {
            mouse_report(3 + 32, false, mods, col, row, modes)
        }
        _ => MouseAction::None,
    }
}

/// `ESC [ I` / `ESC [ O` when the program asked for focus reports.
pub fn encode_focus(focused: bool, modes: &InputModes) -> Option<Vec<u8>> {
    modes.focus_reporting.then(|| {
        if focused {
            b"\x1b[I".to_vec()
        } else {
            b"\x1b[O".to_vec()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers }
    }

    fn enc(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
        encode_key(key(code, mods), &InputModes::default())
    }

    fn enc_app(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
        let modes = InputModes {
            app_cursor: true,
            ..Default::default()
        };
        encode_key(key(code, mods), &modes)
    }

    fn s(code: KeyCode, mods: KeyModifiers) -> String {
        String::from_utf8(enc(code, mods).unwrap()).unwrap()
    }

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const SUPER: KeyModifiers = KeyModifiers::SUPER;

    #[test]
    fn plain_chars_are_utf8() {
        assert_eq!(enc(KeyCode::Char('a'), NONE), Some(b"a".to_vec()));
        assert_eq!(enc(KeyCode::Char('é'), NONE), Some("é".as_bytes().to_vec()));
        assert_eq!(
            enc(KeyCode::Char('世'), NONE),
            Some("世".as_bytes().to_vec())
        );
        assert_eq!(
            enc(KeyCode::Char('🦀'), NONE),
            Some("🦀".as_bytes().to_vec())
        );
        assert_eq!(enc(KeyCode::Char(' '), NONE), Some(b" ".to_vec()));
    }

    #[test]
    fn uppercase_with_or_without_shift() {
        assert_eq!(enc(KeyCode::Char('A'), NONE), Some(b"A".to_vec()));
        assert_eq!(enc(KeyCode::Char('A'), SHIFT), Some(b"A".to_vec()));
        assert_eq!(enc(KeyCode::Char('!'), SHIFT), Some(b"!".to_vec()));
        assert_eq!(
            enc(KeyCode::Char('É'), SHIFT),
            Some("É".as_bytes().to_vec())
        );
        assert_eq!(
            enc(KeyCode::Char('A'), ALT | SHIFT),
            Some(b"\x1bA".to_vec())
        );
        assert_eq!(enc(KeyCode::Char('A'), CTRL | SHIFT), Some(vec![1]));
    }

    #[test]
    fn ctrl_letters() {
        for (i, c) in ('a'..='z').enumerate() {
            let want = Some(vec![i as u8 + 1]);
            assert_eq!(enc(KeyCode::Char(c), CTRL), want);
            assert_eq!(enc(KeyCode::Char(c.to_ascii_uppercase()), CTRL), want);
            assert_eq!(
                enc(KeyCode::Char(c.to_ascii_uppercase()), CTRL | SHIFT),
                want
            );
        }
    }

    #[test]
    fn ctrl_punctuation_and_digits() {
        let table: &[(&[char], u8)] = &[
            (&[' ', '@', '2'], 0x00),
            (&['[', '3'], 0x1b),
            (&['\\', '4'], 0x1c),
            (&[']', '5'], 0x1d),
            (&['^', '6'], 0x1e),
            (&['_', '/', '7'], 0x1f),
            (&['8', '?'], 0x7f),
        ];
        for (chars, byte) in table {
            for c in *chars {
                assert_eq!(enc(KeyCode::Char(*c), CTRL), Some(vec![*byte]), "ctrl+{c}");
            }
        }
        // No control code: the character itself.
        assert_eq!(enc(KeyCode::Char('1'), CTRL), Some(b"1".to_vec()));
        assert_eq!(enc(KeyCode::Char('é'), CTRL), Some("é".as_bytes().to_vec()));
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(enc(KeyCode::Char('x'), ALT), Some(b"\x1bx".to_vec()));
        assert_eq!(
            enc(KeyCode::Char('é'), ALT),
            Some("\x1bé".as_bytes().to_vec())
        );
        assert_eq!(enc(KeyCode::Char('a'), ALT | CTRL), Some(vec![0x1b, 0x01]));
        assert_eq!(enc(KeyCode::Char('['), ALT | CTRL), Some(vec![0x1b, 0x1b]));
        assert_eq!(enc(KeyCode::Char(' '), ALT | CTRL), Some(vec![0x1b, 0x00]));
        assert_eq!(enc(KeyCode::Enter, ALT), Some(b"\x1b\r".to_vec()));
        assert_eq!(enc(KeyCode::Backspace, ALT), Some(b"\x1b\x7f".to_vec()));
        assert_eq!(enc(KeyCode::Tab, ALT), Some(b"\x1b\t".to_vec()));
        assert_eq!(enc(KeyCode::Esc, ALT), Some(b"\x1b\x1b".to_vec()));
    }

    #[test]
    fn super_on_chars_is_ignored() {
        assert_eq!(enc(KeyCode::Char('a'), SUPER), Some(b"a".to_vec()));
    }

    #[test]
    fn simple_keys() {
        assert_eq!(enc(KeyCode::Enter, NONE), Some(b"\r".to_vec()));
        assert_eq!(enc(KeyCode::Tab, NONE), Some(b"\t".to_vec()));
        assert_eq!(enc(KeyCode::Tab, SHIFT), Some(b"\x1b[Z".to_vec()));
        assert_eq!(enc(KeyCode::Backspace, NONE), Some(vec![0x7f]));
        assert_eq!(enc(KeyCode::Backspace, CTRL), Some(vec![0x08]));
        assert_eq!(enc(KeyCode::Esc, NONE), Some(vec![0x1b]));
        assert_eq!(enc(KeyCode::Null, NONE), Some(vec![0]));
    }

    #[test]
    fn unsent_keys() {
        assert_eq!(enc(KeyCode::CapsLock, NONE), None);
        assert_eq!(enc(KeyCode::ScrollLock, NONE), None);
        assert_eq!(enc(KeyCode::NumLock, NONE), None);
        assert_eq!(enc(KeyCode::PrintScreen, NONE), None);
        assert_eq!(enc(KeyCode::Menu, NONE), None);
        assert_eq!(enc(KeyCode::F(13), NONE), None);
        assert_eq!(enc(KeyCode::F(0), NONE), None);
    }

    #[test]
    fn arrows_and_home_end() {
        let table = [
            (KeyCode::Up, "A"),
            (KeyCode::Down, "B"),
            (KeyCode::Right, "C"),
            (KeyCode::Left, "D"),
            (KeyCode::Home, "H"),
            (KeyCode::End, "F"),
        ];
        for (code, f) in table {
            assert_eq!(s(code, NONE), format!("\x1b[{f}"));
            assert_eq!(
                enc_app(code, NONE),
                Some(format!("\x1bO{f}").into_bytes()),
                "app cursor {code:?}"
            );
            assert_eq!(s(code, SHIFT), format!("\x1b[1;2{f}"));
            assert_eq!(s(code, ALT), format!("\x1b[1;3{f}"));
            assert_eq!(s(code, SHIFT | ALT), format!("\x1b[1;4{f}"));
            assert_eq!(s(code, CTRL), format!("\x1b[1;5{f}"));
            assert_eq!(s(code, CTRL | SHIFT), format!("\x1b[1;6{f}"));
            assert_eq!(s(code, CTRL | ALT), format!("\x1b[1;7{f}"));
            assert_eq!(s(code, CTRL | ALT | SHIFT), format!("\x1b[1;8{f}"));
            assert_eq!(s(code, SUPER), format!("\x1b[1;9{f}"));
            assert_eq!(s(code, SUPER | CTRL), format!("\x1b[1;13{f}"));
            // Modifiers win over application cursor mode.
            assert_eq!(
                enc_app(code, CTRL),
                Some(format!("\x1b[1;5{f}").into_bytes())
            );
        }
    }

    #[test]
    fn editing_keys() {
        let table = [
            (KeyCode::Insert, 2),
            (KeyCode::Delete, 3),
            (KeyCode::PageUp, 5),
            (KeyCode::PageDown, 6),
        ];
        for (code, n) in table {
            assert_eq!(s(code, NONE), format!("\x1b[{n}~"));
            // Not affected by application cursor mode.
            assert_eq!(enc_app(code, NONE), Some(format!("\x1b[{n}~").into_bytes()));
            assert_eq!(s(code, SHIFT), format!("\x1b[{n};2~"));
            assert_eq!(s(code, ALT), format!("\x1b[{n};3~"));
            assert_eq!(s(code, CTRL), format!("\x1b[{n};5~"));
            assert_eq!(s(code, CTRL | ALT), format!("\x1b[{n};7~"));
        }
    }

    #[test]
    fn function_keys() {
        for (i, f) in ["P", "Q", "R", "S"].iter().enumerate() {
            let code = KeyCode::F(i as u8 + 1);
            assert_eq!(s(code, NONE), format!("\x1bO{f}"));
            assert_eq!(enc_app(code, NONE), Some(format!("\x1bO{f}").into_bytes()));
            assert_eq!(s(code, SHIFT), format!("\x1b[1;2{f}"));
            assert_eq!(s(code, CTRL), format!("\x1b[1;5{f}"));
            assert_eq!(s(code, ALT | CTRL), format!("\x1b[1;7{f}"));
        }
        let codes = [
            (5, 15),
            (6, 17),
            (7, 18),
            (8, 19),
            (9, 20),
            (10, 21),
            (11, 23),
            (12, 24),
        ];
        for (n, code) in codes {
            assert_eq!(s(KeyCode::F(n), NONE), format!("\x1b[{code}~"));
            assert_eq!(s(KeyCode::F(n), SHIFT), format!("\x1b[{code};2~"));
            assert_eq!(s(KeyCode::F(n), CTRL), format!("\x1b[{code};5~"));
            assert_eq!(
                s(KeyCode::F(n), CTRL | SHIFT | ALT),
                format!("\x1b[{code};8~")
            );
        }
    }

    #[test]
    fn paste_bracketed() {
        let modes = InputModes {
            bracketed_paste: true,
            ..Default::default()
        };
        assert_eq!(
            encode_paste("hi\nthere\r\n", &modes),
            b"\x1b[200~hi\nthere\r\n\x1b[201~".to_vec()
        );
        assert_eq!(encode_paste("", &modes), b"\x1b[200~\x1b[201~".to_vec());
        assert_eq!(
            encode_paste("a\x1b[201~b", &modes),
            b"\x1b[200~ab\x1b[201~".to_vec()
        );
        // Removing one must not leave another behind.
        assert_eq!(
            encode_paste("a\x1b[2\x1b[201~01~b", &modes),
            b"\x1b[200~ab\x1b[201~".to_vec()
        );
        assert_eq!(
            encode_paste("é世", &modes),
            "\x1b[200~é世\x1b[201~".as_bytes().to_vec()
        );
    }

    #[test]
    fn paste_plain() {
        let modes = InputModes::default();
        assert_eq!(encode_paste("a\nb\r\nc\rd", &modes), b"a\rb\rc\rd".to_vec());
        assert_eq!(encode_paste("\n\n", &modes), b"\r\r".to_vec());
        assert_eq!(encode_paste("é", &modes), "é".as_bytes().to_vec());
        // Not stripped without bracketing: it is just text.
        assert_eq!(encode_paste("\x1b[201~", &modes), b"\x1b[201~".to_vec());
    }

    fn mouse(kind: MouseEventKind, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers,
        }
    }

    fn modes(tracking: MouseTracking, sgr: bool) -> InputModes {
        InputModes {
            mouse: tracking,
            sgr_mouse: sgr,
            ..Default::default()
        }
    }

    fn send(bytes: &[u8]) -> MouseAction {
        MouseAction::Send(bytes.to_vec())
    }

    use MouseButton::{Left, Middle, Right};
    use MouseEventKind::*;

    #[test]
    fn sgr_press_release() {
        let m = modes(MouseTracking::Click, true);
        let e = |k| mouse(k, NONE);
        assert_eq!(
            encode_mouse(&e(Down(Left)), 0, 0, 3, &m),
            send(b"\x1b[<0;1;1M")
        );
        assert_eq!(
            encode_mouse(&e(Down(Middle)), 9, 4, 3, &m),
            send(b"\x1b[<1;10;5M")
        );
        assert_eq!(
            encode_mouse(&e(Down(Right)), 299, 0, 3, &m),
            send(b"\x1b[<2;300;1M")
        );
        assert_eq!(
            encode_mouse(&e(Up(Left)), 0, 0, 3, &m),
            send(b"\x1b[<0;1;1m")
        );
        assert_eq!(
            encode_mouse(&e(Up(Right)), 1, 2, 3, &m),
            send(b"\x1b[<2;2;3m")
        );
    }

    #[test]
    fn mouse_modifiers() {
        let m = modes(MouseTracking::Click, true);
        let d = |mods| encode_mouse(&mouse(Down(Left), mods), 0, 0, 3, &m);
        assert_eq!(d(SHIFT), send(b"\x1b[<4;1;1M"));
        assert_eq!(d(ALT), send(b"\x1b[<8;1;1M"));
        assert_eq!(d(CTRL), send(b"\x1b[<16;1;1M"));
        assert_eq!(d(SHIFT | ALT | CTRL), send(b"\x1b[<28;1;1M"));
        assert_eq!(d(SUPER), send(b"\x1b[<0;1;1M"));
    }

    #[test]
    fn x10_encoding() {
        let m = modes(MouseTracking::Click, false);
        let e = |k| mouse(k, NONE);
        assert_eq!(
            encode_mouse(&e(Down(Left)), 0, 0, 3, &m),
            send(&[0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            encode_mouse(&e(Down(Right)), 9, 4, 3, &m),
            send(&[0x1b, b'[', b'M', 34, 42, 37])
        );
        // The release has no button.
        assert_eq!(
            encode_mouse(&e(Up(Right)), 0, 0, 3, &m),
            send(&[0x1b, b'[', b'M', 35, 33, 33])
        );
        assert_eq!(
            encode_mouse(&mouse(Up(Left), CTRL), 0, 0, 3, &m),
            send(&[0x1b, b'[', b'M', 32 + 3 + 16, 33, 33])
        );
        assert_eq!(
            encode_mouse(&e(ScrollDown), 0, 0, 3, &m),
            send(&[0x1b, b'[', b'M', 32 + 65, 33, 33])
        );
    }

    #[test]
    fn x10_coordinate_limits() {
        let m = modes(MouseTracking::Click, false);
        let e = mouse(Down(Left), NONE);
        // Column 222 is cell 223 (1-based): 32 + 223 = 255.
        assert_eq!(
            encode_mouse(&e, 222, 0, 3, &m),
            send(&[0x1b, b'[', b'M', 32, 255, 33])
        );
        assert_eq!(encode_mouse(&e, 223, 0, 3, &m), MouseAction::None);
        assert_eq!(encode_mouse(&e, 0, 223, 3, &m), MouseAction::None);
        assert_eq!(
            encode_mouse(&e, 0, 222, 3, &m),
            send(&[0x1b, b'[', b'M', 32, 33, 255])
        );
    }

    #[test]
    fn utf8_coordinates() {
        let m = InputModes {
            utf8_mouse: true,
            ..modes(MouseTracking::Click, false)
        };
        let e = mouse(Down(Left), NONE);
        let mut want = vec![0x1b, b'[', b'M', 32];
        want.extend("\u{ff}".as_bytes()); // 32 + 223
        want.push(33);
        assert_eq!(encode_mouse(&e, 222, 0, 3, &m), MouseAction::Send(want));
        let mut want = vec![0x1b, b'[', b'M', 32];
        want.extend(char::from_u32(32 + 301).unwrap().to_string().as_bytes());
        want.push(33);
        assert_eq!(encode_mouse(&e, 300, 0, 3, &m), MouseAction::Send(want));
    }

    #[test]
    fn sgr_has_no_coordinate_limit() {
        let m = modes(MouseTracking::Click, true);
        assert_eq!(
            encode_mouse(&mouse(Down(Left), NONE), 1000, 500, 3, &m),
            send(b"\x1b[<0;1001;501M")
        );
    }

    #[test]
    fn tracking_filters_motion() {
        let drag = mouse(Drag(Left), NONE);
        let moved = mouse(Moved, NONE);
        let press = mouse(Down(Left), NONE);

        let none = modes(MouseTracking::None, true);
        for e in [&drag, &moved, &press, &mouse(Up(Left), NONE)] {
            assert_eq!(encode_mouse(e, 0, 0, 3, &none), MouseAction::None);
        }

        let click = modes(MouseTracking::Click, true);
        assert_eq!(encode_mouse(&press, 0, 0, 3, &click), send(b"\x1b[<0;1;1M"));
        assert_eq!(encode_mouse(&drag, 0, 0, 3, &click), MouseAction::None);
        assert_eq!(encode_mouse(&moved, 0, 0, 3, &click), MouseAction::None);

        let dr = modes(MouseTracking::Drag, true);
        assert_eq!(encode_mouse(&drag, 0, 0, 3, &dr), send(b"\x1b[<32;1;1M"));
        assert_eq!(
            encode_mouse(&mouse(Drag(Right), NONE), 1, 1, 3, &dr),
            send(b"\x1b[<34;2;2M")
        );
        assert_eq!(encode_mouse(&moved, 0, 0, 3, &dr), MouseAction::None);
        assert_eq!(encode_mouse(&press, 0, 0, 3, &dr), send(b"\x1b[<0;1;1M"));

        let motion = modes(MouseTracking::Motion, true);
        assert_eq!(
            encode_mouse(&drag, 0, 0, 3, &motion),
            send(b"\x1b[<32;1;1M")
        );
        assert_eq!(
            encode_mouse(&moved, 4, 2, 3, &motion),
            send(b"\x1b[<35;5;3M")
        );
        assert_eq!(
            encode_mouse(&mouse(Moved, CTRL), 0, 0, 3, &motion),
            send(b"\x1b[<51;1;1M")
        );
    }

    #[test]
    fn x10_motion() {
        let m = modes(MouseTracking::Motion, false);
        assert_eq!(
            encode_mouse(&mouse(Moved, NONE), 0, 0, 3, &m),
            send(&[0x1b, b'[', b'M', 32 + 35, 33, 33])
        );
        assert_eq!(
            encode_mouse(&mouse(Drag(Middle), NONE), 0, 0, 3, &m),
            send(&[0x1b, b'[', b'M', 32 + 33, 33, 33])
        );
    }

    #[test]
    fn wheel_reported() {
        for tracking in [
            MouseTracking::Click,
            MouseTracking::Drag,
            MouseTracking::Motion,
        ] {
            let m = modes(tracking, true);
            let w = |k| encode_mouse(&mouse(k, NONE), 2, 3, 3, &m);
            assert_eq!(w(ScrollUp), send(b"\x1b[<64;3;4M"));
            assert_eq!(w(ScrollDown), send(b"\x1b[<65;3;4M"));
            assert_eq!(w(ScrollLeft), send(b"\x1b[<66;3;4M"));
            assert_eq!(w(ScrollRight), send(b"\x1b[<67;3;4M"));
        }
        let m = modes(MouseTracking::Click, true);
        assert_eq!(
            encode_mouse(&mouse(ScrollUp, SHIFT), 0, 0, 3, &m),
            send(b"\x1b[<68;1;1M")
        );
        // Reporting wins over the alternate screen.
        let m = InputModes {
            alt_screen: true,
            alternate_scroll: true,
            ..modes(MouseTracking::Click, true)
        };
        assert_eq!(
            encode_mouse(&mouse(ScrollUp, NONE), 0, 0, 3, &m),
            send(b"\x1b[<64;1;1M")
        );
    }

    #[test]
    fn wheel_as_arrows_on_alt_screen() {
        let m = InputModes {
            alt_screen: true,
            alternate_scroll: true,
            ..Default::default()
        };
        assert_eq!(
            encode_mouse(&mouse(ScrollUp, NONE), 0, 0, 3, &m),
            send(b"\x1b[A\x1b[A\x1b[A")
        );
        assert_eq!(
            encode_mouse(&mouse(ScrollDown, NONE), 0, 0, 2, &m),
            send(b"\x1b[B\x1b[B")
        );
        let app = InputModes {
            app_cursor: true,
            ..m
        };
        assert_eq!(
            encode_mouse(&mouse(ScrollUp, NONE), 0, 0, 2, &app),
            send(b"\x1bOA\x1bOA")
        );
        assert_eq!(
            encode_mouse(&mouse(ScrollDown, SHIFT), 0, 0, 1, &app),
            send(b"\x1bOB")
        );
        assert_eq!(
            encode_mouse(&mouse(ScrollLeft, NONE), 0, 0, 1, &app),
            MouseAction::None
        );
    }

    #[test]
    fn wheel_scrolls_scrollback() {
        let m = InputModes::default();
        assert_eq!(
            encode_mouse(&mouse(ScrollUp, NONE), 0, 0, 3, &m),
            MouseAction::Scroll(3)
        );
        assert_eq!(
            encode_mouse(&mouse(ScrollDown, NONE), 0, 0, 3, &m),
            MouseAction::Scroll(-3)
        );
        assert_eq!(
            encode_mouse(&mouse(ScrollLeft, NONE), 0, 0, 3, &m),
            MouseAction::None
        );
        // The alternate screen without alternate scroll, and alternate scroll without it.
        for (alt_screen, alternate_scroll) in [(true, false), (false, true)] {
            let m = InputModes {
                alt_screen,
                alternate_scroll,
                ..Default::default()
            };
            assert_eq!(
                encode_mouse(&mouse(ScrollUp, NONE), 0, 0, 5, &m),
                MouseAction::Scroll(5)
            );
        }
    }

    #[test]
    fn focus() {
        let on = InputModes {
            focus_reporting: true,
            ..Default::default()
        };
        assert_eq!(encode_focus(true, &on), Some(b"\x1b[I".to_vec()));
        assert_eq!(encode_focus(false, &on), Some(b"\x1b[O".to_vec()));
        let off = InputModes::default();
        assert_eq!(encode_focus(true, &off), None);
        assert_eq!(encode_focus(false, &off), None);
    }
}
