//! Draws the panel: a border line, then the screen of the shell cell by cell, in the colors the
//! programs set (indexed colors stay indexed, so the outer terminal's palette applies).

use vortex_view::{
    graphics::{Color, CursorKind, Modifier, Rect, Style, UnderlineStyle},
    terminal::{self, CursorShape},
    theme::Theme,
    Editor,
};
use tui::buffer::Buffer as Surface;

/// Draws the panel in `area` (its border is the first row) and remembers where its screen is.
pub fn render(editor: &mut Editor, area: Rect, surface: &mut Surface) {
    let theme = &editor.theme;
    let base = theme
        .try_get_exact("ui.terminal")
        .unwrap_or_else(|| theme.get("ui.background"));
    let border = theme
        .try_get_exact("ui.terminal.border")
        .or_else(|| theme.try_get_exact("ui.window"))
        .unwrap_or_else(|| theme.get("ui.statusline.inactive"));
    let screen = area.clip_top(1);
    surface.set_style(area, base);
    render_border(
        &editor.terminal,
        area.with_height(1),
        border,
        theme,
        surface,
    );

    editor.terminal.area = screen;
    let Some(shell) = &editor.terminal.shell else {
        return;
    };
    shell.resize(terminal::Size {
        rows: screen.height,
        cols: screen.width,
    });
    // cleared before reading the screen, so output that comes after sends a new redraw
    shell.take_output_pending();
    shell.visit_cells(|row, col, cell| {
        if row >= screen.height || col >= screen.width || cell.text.is_empty() {
            return;
        }
        let (x, y) = (screen.x + col, screen.y + row);
        let style = base.patch(cell_style(cell));
        if cell.text == " " {
            surface[(x, y)].set_symbol(" ").set_style(style);
        } else {
            surface.set_stringn(x, y, &cell.text, (screen.right() - x) as usize, style);
        }
    });
}

fn render_border(
    panel: &terminal::Panel,
    area: Rect,
    style: Style,
    theme: &Theme,
    surface: &mut Surface,
) {
    surface.set_style(area, style);
    surface.set_string(
        area.x,
        area.y,
        "─".repeat(area.width as usize).as_str(),
        style,
    );
    let title = panel
        .shell
        .as_ref()
        .and_then(|shell| shell.title())
        .filter(|title| !title.is_empty())
        .map_or_else(|| " Terminal ".to_owned(), |title| format!(" {title} "));
    let title_style = if panel.focused {
        style.patch(theme.get("ui.text.focus"))
    } else {
        style
    };
    surface.set_stringn(
        area.x + 1,
        area.y,
        &title,
        area.width.saturating_sub(2) as usize,
        title_style.add_modifier(Modifier::BOLD),
    );
    let scrolled = panel
        .shell
        .as_ref()
        .map_or(0, |shell| shell.scroll_offset());
    let hint = if scrolled > 0 {
        format!(" ↑{scrolled}  type to go back ")
    } else if panel.focused {
        " C-t: editor ".to_owned()
    } else {
        " C-t: focus  Esc: hide ".to_owned()
    };
    let hint_width = hint.chars().count() as u16;
    if area.width > hint_width + title.chars().count() as u16 + 4 {
        surface.set_string(area.right() - hint_width - 1, area.y, &hint, style);
    }
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
