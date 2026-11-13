//! Draws the panel: the screen of the shell cell by cell, right below the statusline of the
//! splits above, in the colors the programs set (indexed colors stay indexed, so the outer terminal's palette applies),
//! or through the theme's phosphor filter ([`phosphor`](super::phosphor)) when it sets one.

use tui::buffer::Buffer as Surface;

use super::phosphor::Phosphor;
use vortex_view::{
    graphics::{Color, CursorKind, Modifier, Rect, Style, UnderlineStyle},
    terminal::{self, CursorShape},
    Editor,
};

/// Draws the panel's screen in `area` and remembers where it is.
pub fn render(editor: &mut Editor, area: Rect, surface: &mut Surface) {
    let theme = &editor.theme;
    let base = theme
        .try_get_exact("ui.terminal")
        .unwrap_or_else(|| theme.get("ui.background"));
    // the phosphor filter (this fork), only when the theme sets one
    let phosphor = theme
        .effects()
        .terminal_phosphor
        .and_then(|color| Phosphor::new(color, base));
    let base = match &phosphor {
        Some(phosphor) => phosphor.style(base),
        None => base,
    };
    surface.set_style(area, base);

    editor.terminal.area = area;
    let Some(shell) = &editor.terminal.shell else {
        return;
    };
    shell.resize(terminal::Size {
        rows: area.height,
        cols: area.width,
    });
    // cleared before reading the screen, so output that comes after sends a new redraw
    shell.take_output_pending();
    shell.visit_cells(|row, col, cell| {
        if row >= area.height || col >= area.width || cell.text.is_empty() {
            return;
        }
        let (x, y) = (area.x + col, area.y + row);
        let style = match &phosphor {
            Some(phosphor) => phosphor.style(base.patch(cell_style(cell))),
            None => base.patch(cell_style(cell)),
        };
        if cell.text == " " {
            surface[(x, y)].set_symbol(" ").set_style(style);
        } else {
            surface.set_stringn(x, y, &cell.text, (area.right() - x) as usize, style);
        }
    });
}

fn color(color: terminal::Color) -> Option<Color> {
    match color {
        terminal::Color::Default => None,
        terminal::Color::Indexed(index) => Some(Color::Indexed(index)),
        terminal::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

fn cell_style(cell: &terminal::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = color(cell.fg) {
        style = style.fg(fg);
    }
    if let Some(bg) = color(cell.bg) {
        style = style.bg(bg);
    }
    for (on, modifier) in [
        (cell.bold, Modifier::BOLD),
        (cell.dim, Modifier::DIM),
        (cell.italic, Modifier::ITALIC),
        (cell.strikeout, Modifier::CROSSED_OUT),
        (cell.inverse, Modifier::REVERSED),
        (cell.hidden, Modifier::HIDDEN),
    ] {
        if on {
            style = style.add_modifier(modifier);
        }
    }
    if cell.underline {
        style = style.underline_style(UnderlineStyle::Line);
    }
    style
}

/// The terminal's cursor while the panel is focused, where the program shows it.
pub fn cursor(editor: &Editor) -> Option<(vortex_core::Position, CursorKind)> {
    let panel = &editor.terminal;
    if !panel.is_focused() {
        return None;
    }
    let area = panel.area;
    let Some(cursor) = panel.shell.as_ref().and_then(|shell| shell.cursor()) else {
        return Some((
            vortex_core::Position::new(area.y as usize, area.x as usize),
            CursorKind::Hidden,
        ));
    };
    let kind = match cursor.shape {
        CursorShape::Block => CursorKind::Block,
        CursorShape::Underline => CursorKind::Underline,
        CursorShape::Beam => CursorKind::Bar,
    };
    let row = (area.y + cursor.row).min(area.bottom().saturating_sub(1));
    let col = (area.x + cursor.col).min(area.right().saturating_sub(1));
    Some((vortex_core::Position::new(row as usize, col as usize), kind))
}
