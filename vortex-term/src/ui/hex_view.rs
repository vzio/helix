//! Drawing the hex view (this fork): a document whose bytes are shown in rows of offsets, hex
//! digits (or bits) and ASCII characters, instead of its text. See `commands/hex.rs` for its
//! keys and `vortex_view::hex` for the bytes.
//!
//! ```text
//! 0x00000000  89 50 4e 47 0d 0a 1a 0a  00 00 00 0d 49 48 44 52  .PNG........IHDR
//! ```

use std::ops::Range;

use tui::buffer::Buffer as Surface;
use vortex_view::{
    graphics::{Modifier, Rect, UnderlineStyle},
    hex::{ascii_char, ByteKind, Column, HexBuffer, HexCursor, Layout, Rows},
    theme::Style,
    Document, Theme, View,
};

/// The area of a view the rows are drawn in: all of it but the statusline.
pub fn rows_area(view: &View) -> Rect {
    view.area.clip_bottom(1)
}

/// The rows of `view`, which depend on its width.
pub fn rows_of(hex: &HexBuffer, cursor: &HexCursor, view: &View) -> Rows {
    Rows::new(cursor.layout, hex.len(), rows_area(view).width)
}

/// The first style of `scopes` that the theme has, else the last one with its broader scopes,
/// as [`Theme::get`] does.
pub fn first_style(theme: &Theme, scopes: &[&str]) -> Style {
    let (last, others) = scopes.split_last().expect("at least one scope");
    others
        .iter()
        .find_map(|scope| theme.try_get_exact(scope))
        .unwrap_or_else(|| theme.get(last))
}

/// The styles the view draws with, looked up once per frame.
struct Styles {
    kinds: [Style; 5],
    prefix: Style,
    offset: Style,
    cursor: Style,
    selection: Style,
    found: Style,
    changed: Style,
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        let kind = |kind: ByteKind| first_style(theme, kind.scopes());
        let cursor = ["ui.cursor.primary", "ui.cursor"]
            .iter()
            .find_map(|scope| theme.try_get_exact(scope))
            .unwrap_or_else(|| Style::default().add_modifier(Modifier::REVERSED));
        Self {
            kinds: [
                kind(ByteKind::Null),
                kind(ByteKind::Printable),
                kind(ByteKind::Whitespace),
                kind(ByteKind::Control),
                kind(ByteKind::Other),
            ],
            prefix: first_style(theme, &["ui.hex.prefix", "ui.text.inactive", "comment"]),
            offset: first_style(theme, &["ui.hex.offset", "ui.linenr"]),
            cursor,
            selection: theme.get("ui.selection"),
            found: first_style(theme, &["ui.hex.match", "ui.highlight", "ui.selection"]),
            changed: theme.get("diff.delta"),
        }
    }

    fn kind(&self, byte: u8) -> Style {
        self.kinds[match ByteKind::of(byte) {
            ByteKind::Null => 0,
            ByteKind::Printable => 1,
            ByteKind::Whitespace => 2,
            ByteKind::Control => 3,
            ByteKind::Other => 4,
        }]
    }
}

/// Draws the rows of a document in the hex view, in its view (above its statusline).
pub fn render(theme: &Theme, doc: &Document, view: &View, surface: &mut Surface, focused: bool) {
    let Some(hex) = doc.hex() else {
        return;
    };
    let cursor = hex.cursor(view.id);
    render_rows(theme, hex, &cursor, rows_area(view), surface, focused);
}

/// Draws the rows of `hex` in `area`, with `cursor` (its `top` gives the first row).
pub fn render_rows(
    theme: &Theme,
    hex: &HexBuffer,
    cursor: &HexCursor,
    area: Rect,
    surface: &mut Surface,
    focused: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let styles = Styles::new(theme);
    let rows = Rows::new(cursor.layout, hex.len(), area.width);
    let per_row = rows.bytes_per_row as u64;
    let top_row = rows.row(cursor.top);
    let start = top_row * per_row;
    let shown = (area.height as u64) * per_row;
    let bytes = hex.bytes(start..start + shown);
    let found: Vec<Range<u64>> = match &hex.search {
        Some(pattern) => hex.matches_in(pattern, start..start + bytes.len() as u64),
        None => Vec::new(),
    };
    let selection = cursor.anchor.map(|_| cursor.selection());
    let is_found = |offset: u64| found.iter().any(|range| range.contains(&offset));
    let is_selected = |offset: u64| selection.as_ref().is_some_and(|sel| sel.contains(&offset));

    let right = area.x + area.width;
    let put = |surface: &mut Surface, x: usize, y: u16, text: &str, style: Style| {
        let x = area.x as usize + x;
        if x < right as usize {
            surface.set_stringn(x as u16, y, text, right as usize - x, style);
        }
    };

    for screen_row in 0..area.height {
        let row = top_row + screen_row as u64;
        if row >= rows.count(hex.len()) {
            break;
        }
        let y = area.y + screen_row;
        let row_start = row * per_row;

        // the offset, its `0x` grey
        put(surface, 0, y, "0x", styles.prefix);
        let digits = format!("{row_start:0width$x}", width = rows.offset_digits);
        put(surface, 2, y, &digits, styles.offset);

        for i in 0..rows.bytes_per_row {
            let offset = row_start + i as u64;
            let Some(&byte) = bytes.get((offset - start) as usize) else {
                break;
            };
            let mut style = styles.kind(byte);
            if hex.is_edited(offset) {
                style = style.patch(styles.changed);
            }
            if is_found(offset) {
                style = style.patch(styles.found);
            }
            if is_selected(offset) {
                style = style.patch(styles.selection);
            }

            // the gap after the byte takes the highlight that spans it
            let next = offset + 1;
            if i + 1 < rows.bytes_per_row && next < hex.len() {
                let gap = if is_selected(offset) && is_selected(next) {
                    Some(styles.selection)
                } else if is_found(offset) && is_found(next) {
                    Some(styles.found)
                } else {
                    None
                };
                if let Some(gap) = gap {
                    let from = rows.byte_x(i) + rows.byte_width();
                    let to = rows.byte_x(i + 1);
                    put(surface, from, y, &" ".repeat(to - from), gap);
                }
            }

            let digits = match rows.layout {
                Layout::Hex => format!("{byte:02x}"),
                Layout::Bin => format!("{byte:08b}"),
            };
            let x = rows.byte_x(i);
            let ascii_x = rows.ascii_x() + i;
            let mut ascii_style = style;
            if offset == cursor.offset {
                let mirror = style
                    .add_modifier(Modifier::BOLD)
                    .underline_style(UnderlineStyle::Line);
                if !focused {
                    put(surface, x, y, &digits, mirror);
                    ascii_style = mirror;
                } else if cursor.column == Column::Bytes {
                    if cursor.overwrite {
                        // the digit typing overwrites
                        put(surface, x, y, &digits, mirror);
                        let at = (cursor.digit as usize).min(digits.len() - 1);
                        put(surface, x + at, y, &digits[at..at + 1], styles.cursor);
                    } else {
                        put(surface, x, y, &digits, styles.cursor);
                    }
                    ascii_style = mirror;
                } else {
                    put(surface, x, y, &digits, mirror);
                    ascii_style = styles.cursor;
                }
            } else {
                put(surface, x, y, &digits, style);
            }
            put(
                surface,
                ascii_x,
                y,
                &ascii_char(byte).to_string(),
                ascii_style,
            );
        }
    }
}

/// What the statusline's position element shows for a document in the hex view: the cursor's
/// offset, the byte under it, and the size of the file.
///
/// `0x1A4 (420)  1a 26 00011010 '&'  of 4.0 GiB`
pub fn position(hex: &HexBuffer, cursor: &HexCursor) -> String {
    let mut text = format!("0x{:X} ({})", cursor.offset, cursor.offset);
    if let Some(byte) = hex.byte(cursor.offset) {
        text.push_str(&format!("  {byte:02x} {byte} {byte:08b}"));
        if let Some(char) = describe_char(byte) {
            text.push(' ');
            text.push_str(&char);
        }
    }
    if cursor.anchor.is_some() {
        let selection = cursor.selection();
        text.push_str(&format!(
            "  [{} sel]",
            size(selection.end - selection.start)
        ));
    }
    text.push_str(&format!("  of {}", size(hex.len())));
    text
}

/// The character of a byte, quoted, or its escape for the common control bytes.
fn describe_char(byte: u8) -> Option<String> {
    let escape = match byte {
        0 => "\\0",
        b'\t' => "\\t",
        b'\n' => "\\n",
        b'\r' => "\\r",
        0x20..=0x7e => return Some(format!("'{}'", byte as char)),
        _ => return None,
    };
    Some(format!("'{escape}'"))
}

/// A size in bytes, with a binary unit past 1 KiB: `420 B`, `4.0 GiB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while unit < UNITS.len() - 1 && size >= 1024.0 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// The mode of the view, for the statusline's mode element: `HEX RO`, `BIN OVR`...
pub fn mode_label(cursor: &HexCursor) -> &'static str {
    match (cursor.layout, cursor.overwrite) {
        (Layout::Hex, false) => "HEX RO",
        (Layout::Hex, true) => "HEX OVR",
        (Layout::Bin, false) => "BIN RO",
        (Layout::Bin, true) => "BIN OVR",
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use vortex_view::graphics::Color;

    fn screen(surface: &Surface) -> Vec<String> {
        (0..surface.area.height)
            .map(|y| {
                let row: String = (0..surface.area.width)
                    .map(|x| surface[(x, y)].symbol.as_str())
                    .collect();
                row.trim_end().to_string()
            })
            .collect()
    }

    fn draw(hex: &HexBuffer, cursor: &HexCursor, width: u16, height: u16) -> Surface {
        let area = Rect::new(0, 0, width, height);
        let mut surface = Surface::empty(area);
        render_rows(&Theme::default(), hex, cursor, area, &mut surface, true);
        surface
    }

    fn png() -> HexBuffer {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(b"\0\0\x01\0\0\0\x01\0\x08\x06\0\0\0\x5c\x72\xa8");
        bytes.extend_from_slice(b"f\0\0\0\tpHYs");
        HexBuffer::from_bytes(bytes)
    }

    #[test]
    fn rows_of_a_png() {
        let rows = screen(&draw(&png(), &HexCursor::default(), 100, 4));
        assert_eq!(
            rows,
            [
                "0x00000000  89 50 4e 47 0d 0a 1a 0a  00 00 00 0d 49 48 44 52  .PNG........IHDR",
                "0x00000010  00 00 01 00 00 00 01 00  08 06 00 00 00 5c 72 a8  .............\\r.",
                "0x00000020  66 00 00 00 09 70 48 59  73                       f....pHYs",
                "",
            ]
        );
    }

    #[test]
    fn narrow_rows_hold_8_bytes() {
        let rows = screen(&draw(&png(), &HexCursor::default(), 60, 2));
        assert_eq!(
            rows,
            [
                "0x00000000  89 50 4e 47 0d 0a 1a 0a  .PNG....",
                "0x00000008  00 00 00 0d 49 48 44 52  ....IHDR",
            ]
        );
    }

    #[test]
    fn rows_in_bin() {
        let cursor = HexCursor {
            layout: Layout::Bin,
            ..Default::default()
        };
        let rows = screen(&draw(&png(), &cursor, 100, 1));
        assert_eq!(
            rows,
            ["0x00000000  10001001 01010000 01001110 01000111  00001101 00001010 00011010 00001010  .PNG...."]
        );
    }

    #[test]
    fn an_empty_file_has_one_offset() {
        let rows = screen(&draw(
            &HexBuffer::from_bytes(Vec::new()),
            &HexCursor::default(),
            80,
            2,
        ));
        assert_eq!(rows, ["0x00000000", ""]);
    }

    #[test]
    fn colors_by_kind_in_both_columns() {
        let theme = Theme::from(
            toml::from_str::<toml::Value>(
                r##"
"ui.hex.null" = "#000001"
"ui.hex.printable" = "#000002"
"ui.hex.whitespace" = "#000003"
"ui.hex.control" = "#000004"
"ui.hex.other" = "#000005"
"ui.text.inactive" = "#000009"
"##,
            )
            .unwrap(),
        );
        let hex = HexBuffer::from_bytes(vec![0x00, b'A', b' ', 0x1b, 0xff]);
        let cursor = HexCursor {
            offset: 4,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 1);
        let mut surface = Surface::empty(area);
        render_rows(&theme, &hex, &cursor, area, &mut surface, false);
        let rows = Rows::new(Layout::Hex, 5, 100);
        let fg = |x: usize| surface[(x as u16, 0)].fg;
        for (i, color) in [1, 2, 3, 4, 5].into_iter().enumerate() {
            let color = Color::Rgb(0, 0, color);
            assert_eq!(fg(rows.byte_x(i)), color, "byte {i}");
            assert_eq!(fg(rows.byte_x(i) + 1), color, "byte {i}");
            assert_eq!(fg(rows.ascii_x() + i), color, "char {i}");
        }
        // the 0x is grey
        assert_eq!(fg(0), Color::Rgb(0, 0, 9));
        assert_eq!(fg(1), Color::Rgb(0, 0, 9));
    }

    #[test]
    fn status_texts() {
        let hex = png();
        let cursor = HexCursor {
            offset: 12,
            ..Default::default()
        };
        assert_eq!(
            position(&hex, &cursor),
            "0xC (12)  49 73 01001001 'I'  of 41 B"
        );
        let cursor = HexCursor {
            offset: 8,
            anchor: Some(11),
            ..Default::default()
        };
        assert_eq!(
            position(&hex, &cursor),
            "0x8 (8)  00 0 00000000 '\\0'  [4 B sel]  of 41 B"
        );
        assert_eq!(size(4 << 30), "4.0 GiB");
        assert_eq!(size(1536), "1.5 KiB");
    }
}
