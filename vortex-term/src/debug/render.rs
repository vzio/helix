//! Draws the debug panel: a tab bar with the session's status, then the current section, or in a
//! wide panel the data section beside the console.

use tui::buffer::Buffer as Surface;
use vortex_view::{
    debug::engine,
    debug::{Marker, Role, Row, Section},
    graphics::{Modifier, Rect, Style},
    theme::Theme,
    Editor,
};

use super::panel;

/// From this width the console shows beside the data section.
pub const WIDE: u16 = 120;

/// Where things are in the panel.
struct Layout {
    /// The tabs: the section, where it starts and its width, on the panel's first row.
    tabs: Vec<(Section, u16, u16)>,
    /// The sections shown and their areas (below the tab bar).
    panes: Vec<(Section, Rect)>,
    /// The column of the line between the panes, in a wide panel.
    divider: Option<u16>,
}

fn tab_label(section: Section) -> String {
    format!(" {} {} ", section.index() + 1, section.title())
}

fn layout(editor: &Editor, area: Rect) -> Layout {
    let state = &editor.debug;
    let body = area.clip_top(1);
    let mut tabs = Vec::new();
    let mut x = area.x;
    let mut push_tab = |section: Section, x: &mut u16| {
        let width = tab_label(section).chars().count() as u16;
        tabs.push((section, *x, width));
        *x += width;
    };
    if area.width >= WIDE {
        let left = area.width * 3 / 5;
        for section in &Section::ALL[..4] {
            push_tab(*section, &mut x);
        }
        let mut console_x = area.x + left + 1;
        push_tab(Section::Console, &mut console_x);
        let left_area = Rect::new(body.x, body.y, left, body.height);
        let right_area = Rect::new(
            body.x + left + 1,
            body.y,
            body.width.saturating_sub(left + 1),
            body.height,
        );
        Layout {
            tabs,
            panes: vec![
                (state.data_section, left_area),
                (Section::Console, right_area),
            ],
            divider: Some(area.x + left),
        }
    } else {
        for section in Section::ALL {
            push_tab(section, &mut x);
        }
        Layout {
            tabs,
            panes: vec![(state.section, body)],
            divider: None,
        }
    }
}

/// What the mouse is over.
pub enum Hit {
    Tab(Section),
    Row(Section, usize),
}

/// The tab or row at a cell of the panel, as last drawn.
pub fn hit(editor: &Editor, column: u16, row: u16) -> Option<Hit> {
    let area = editor.debug.area;
    let layout = layout(editor, area);
    if row == area.y {
        return layout
            .tabs
            .iter()
            .find(|(_, x, width)| column >= *x && column < x + width)
            .map(|(section, _, _)| Hit::Tab(*section));
    }
    let (section, pane) = layout
        .panes
        .iter()
        .find(|(_, pane)| column >= pane.x && column < pane.right())?;
    let scroll = editor.debug.cursor(*section).scroll;
    let index = scroll + (row - pane.y) as usize;
    (index < panel::rows(editor, *section).len()).then_some(Hit::Row(*section, index))
}

/// A theme scope of this fork, or a scope every theme has.
fn exact_or(theme: &Theme, scope: &str, fallback: &str) -> Style {
    theme
        .try_get_exact(scope)
        .unwrap_or_else(|| theme.get(fallback))
}

/// The color of a style, without its background.
fn fg_only(style: Style) -> Style {
    Style { bg: None, ..style }
}

fn role_style(theme: &Theme, role: Role) -> Style {
    match role {
        Role::Name => exact_or(theme, "ui.debug.name", "variable"),
        Role::Value => exact_or(theme, "ui.debug.value", "ui.text"),
        Role::Changed => theme
            .try_get_exact("ui.debug.value.changed")
            .unwrap_or_else(|| fg_only(theme.get("warning")).add_modifier(Modifier::BOLD)),
        Role::Type | Role::Dim => theme.get("ui.text.inactive"),
        Role::Error => fg_only(theme.get("error")),
        Role::Heading => {
            exact_or(theme, "ui.debug.heading", "ui.text").add_modifier(Modifier::BOLD)
        }
        Role::Current => {
            exact_or(theme, "ui.debug.active", "ui.text.focus").add_modifier(Modifier::BOLD)
        }
        Role::Breakpoint => exact_or(theme, "ui.debug.breakpoint", "error"),
        Role::Unverified => exact_or(theme, "ui.debug.breakpoint.unverified", "ui.text.inactive"),
        Role::Disabled => exact_or(theme, "ui.debug.breakpoint.disabled", "ui.text.inactive"),
        Role::Stdout => theme.get("ui.text"),
        Role::Stderr => theme
            .try_get_exact("ui.debug.console.error")
            .unwrap_or_else(|| fg_only(theme.get("error"))),
        Role::Info => exact_or(theme, "ui.debug.console.info", "ui.text.info"),
        Role::Input => exact_or(theme, "ui.debug.console.input", "ui.text.focus"),
        Role::Frozen => theme
            .try_get_exact("ui.debug.frozen")
            .unwrap_or_else(|| fg_only(theme.get("info"))),
        Role::Rewind => rewind_style(theme),
        Role::Memory => theme
            .try_get_exact("ui.debug.memory")
            .unwrap_or_else(|| fg_only(theme.get("info"))),
        Role::Carried => theme
            .try_get_exact("ui.debug.carried")
            .unwrap_or_else(|| theme.get("ui.text.inactive").add_modifier(Modifier::ITALIC)),
    }
}

/// The REWIND marker's style: `ui.debug.rewind`, else the warning color, bold and reversed.
pub fn rewind_style(theme: &Theme) -> Style {
    theme.try_get_exact("ui.debug.rewind").unwrap_or_else(|| {
        fg_only(theme.get("warning")).add_modifier(Modifier::BOLD | Modifier::REVERSED)
    })
}

/// Draws the panel in `area` and remembers where it is.
pub fn render(editor: &mut Editor, area: Rect, surface: &mut Surface) {
    editor.debug.area = area;
    let theme = &editor.theme;
    let base = theme
        .try_get_exact("ui.debug.panel")
        .or_else(|| theme.try_get_exact("ui.terminal"))
        .unwrap_or_else(|| theme.get("ui.background"));
    surface.set_style(area, base);
    if area.height < 2 || area.width < 10 {
        return;
    }
    let layout = layout(editor, area);

    // the tab bar
    let focused = editor.debug.is_focused();
    let bar = exact_or(theme, "ui.debug.tab", "ui.statusline.inactive");
    surface.set_style(area.with_height(1), base.patch(bar));
    let active_tab = theme
        .try_get_exact("ui.debug.tab.active")
        .unwrap_or_else(|| theme.get("ui.menu.selected"));
    for (section, x, width) in &layout.tabs {
        let shown = layout.panes.iter().any(|(s, _)| s == section);
        let style = if *section == editor.debug.section && focused {
            active_tab
        } else if shown {
            bar.add_modifier(Modifier::BOLD)
        } else {
            bar
        };
        let label = tab_label(*section);
        surface.set_stringn(*x, area.y, &label, *width as usize, base.patch(style));
    }
    // the status, at the right of the bar (or of the left pane's tabs)
    let rewind = editor.debug.spectator.history.marker();
    let status = rewind.clone().unwrap_or_else(|| status_text(editor));
    let tabs_end = layout
        .tabs
        .iter()
        .filter(|(section, _, _)| *section != Section::Console || layout.divider.is_none())
        .map(|(_, x, width)| x + width)
        .max()
        .unwrap_or(area.x);
    let status_end = layout.divider.unwrap_or(area.right());
    let room = status_end.saturating_sub(tabs_end + 2) as usize;
    if room > 3 {
        let text: String = if status.chars().count() > room {
            status
                .chars()
                .take(room.saturating_sub(1))
                .chain(['…'])
                .collect()
        } else {
            status
        };
        let width = text.chars().count() as u16;
        surface.set_stringn(
            status_end.saturating_sub(width + 1),
            area.y,
            &text,
            room,
            if rewind.is_some() {
                base.patch(bar).patch(rewind_style(theme))
            } else {
                base.patch(bar).patch(theme.get("ui.text.inactive"))
            },
        );
    }

    // the keys are debug mode's bar's and the `?` box's: the panel has no row of hints
    if editor.debug.engine.task.is_some() && !editor.debug.mode_on() {
        // over the top row's status (debug mode's bar shows it in its state)
        draw_loading(
            editor,
            Rect::new(area.x, area.y, area.width, 1),
            surface,
            base,
        );
    }

    if editor.debug.spectator.settings.is_some() {
        draw_settings(editor, area.clip_top(1), surface, base);
        return;
    }

    if let Some(divider) = layout.divider {
        let style = base.patch(theme.get("ui.window"));
        for y in area.y + 1..area.bottom() {
            surface.set_stringn(divider, y, "│", 1, style);
        }
        if let Some((_, x, width)) = layout.tabs.iter().find(|(s, _, _)| *s == Section::Console) {
            surface.set_stringn(divider, area.y, "│", 1, base.patch(bar));
            let _ = (x, width);
        }
    }

    for (section, pane) in &layout.panes {
        draw_section(editor, *section, *pane, surface, base);
    }
}

/// The width of the loading animation's bar.
const LOADING_BAR: usize = 24;

/// The engine's task, animated: a spinner, a bar that fills as files are done (a comet that
/// sweeps while the files are still being found) with a glow running along it, and the
/// progress: `◜ ━━━━━━━━━━──────────── installing breakpoints · 12/48 files · 3,410 lines`.
fn draw_loading(editor: &Editor, row: Rect, surface: &mut Surface, base: Style) {
    surface.set_style(row, base);
    let right = row.right().saturating_sub(1);
    let mut x = row.x + 1;
    for (text, style) in loading_spans(editor, row.width as usize) {
        if x >= right {
            break;
        }
        surface.set_stringn(x, row.y, &text, (right - x) as usize, base.patch(style));
        x += text.chars().count() as u16;
    }
}

/// The loading animation as spans, for a row `width` columns wide (the panel's, or debug mode's
/// bar): empty while the engine has no task.
pub(crate) fn loading_spans(editor: &Editor, width: usize) -> Vec<(String, Style)> {
    let Some(task) = &editor.debug.engine.task else {
        return Vec::new();
    };
    let theme = &editor.theme;
    let text = theme
        .try_get_exact("ui.debug.loading")
        .or_else(|| theme.try_get_exact("ui.debug.active"))
        .unwrap_or_else(|| theme.get("ui.text.focus"));
    let bar = theme
        .try_get_exact("ui.debug.loading.bar")
        .unwrap_or_else(|| fg_only(text));
    let glow = theme
        .try_get_exact("ui.debug.loading.glow")
        .unwrap_or_else(|| fg_only(text).add_modifier(Modifier::BOLD));
    let track = theme
        .try_get_exact("ui.debug.loading.track")
        .unwrap_or_else(|| theme.get("ui.text.inactive"));
    let frame = (task.started.elapsed().as_millis() / 80) as usize;
    let mut spans = vec![(format!("{} ", engine::spinner(frame / 2)), text)];
    let cells = LOADING_BAR.min(width.saturating_sub(4) / 3);
    if cells >= 6 {
        let cycle = cells + 6;
        let head = frame % cycle;
        for cell in 0..cells {
            let (glyph, style) = if task.files_total == 0 {
                // still finding the files: a comet sweeps along the track
                let behind = head.checked_sub(cell + 3);
                match behind {
                    Some(0) => ("━", glow),
                    Some(1..=3) => ("━", bar),
                    _ => ("─", track),
                }
            } else {
                let filled = cell * task.files_total < task.files_done * cells;
                match (filled, head.checked_sub(3) == Some(cell)) {
                    (true, true) => ("━", glow),
                    (true, false) => ("━", bar),
                    (false, _) => ("─", track),
                }
            };
            spans.push((glyph.to_string(), style));
        }
        spans.push((" ".to_string(), text));
    }
    spans.push((task.label(), text));
    spans
}

fn status_text(editor: &Editor) -> String {
    let state = &editor.debug;
    // playing (or blocked) says so instead of the program's state
    let playing = state.spectator.label(std::time::Instant::now());
    let mut text = playing.clone().unwrap_or_else(|| state.status.label());
    if let (
        None,
        vortex_view::debug::Status::Stopped {
            description: Some(description),
            ..
        },
    ) = (&playing, &state.status)
    {
        let reason = text.trim_start_matches("stopped: ").to_lowercase();
        if !description.to_lowercase().contains(&reason) {
            text = format!("{text} ({description})");
        }
    }
    if playing.is_none() && state.status.is_active() {
        text = format!(
            "{text} · {} · {}",
            state.spectator.speed_label(),
            state.spectator.follow.label()
        );
    }
    // the lineline's counter: lines run so far, every thread
    if state.engine.kind == engine::EngineKind::Breakpoints
        && state.status.is_active()
        && !state.engine.record.is_empty()
    {
        let lines = state.engine.record.counter() as usize;
        let noun = if lines == 1 { "line" } else { "lines" };
        text = format!("{text} · {} {noun}", engine::thousands(lines));
    }
    if !state.session_name.is_empty() && state.status.is_active() {
        text = format!("{text} · {}", state.session_name);
    }
    if let Some(filter) = state.filters.get(&state.section).filter(|f| !f.is_empty()) {
        text = format!("/{filter}  {text}");
    }
    text
}

/// The capture settings, over the sections.
fn draw_settings(editor: &Editor, pane: Rect, surface: &mut Surface, base: Style) {
    let theme = &editor.theme;
    let rows = panel::settings_rows(editor);
    let selected = editor.debug.spectator.settings.unwrap_or(0) + 1;
    for (i, row) in rows.iter().enumerate().take(pane.height as usize) {
        let y = pane.y + i as u16;
        let row_base = if i == selected {
            let style = base.patch(theme.get("ui.menu.selected"));
            surface.set_style(Rect::new(pane.x, y, pane.width, 1), style);
            style
        } else {
            base
        };
        draw_row(
            theme,
            row,
            pane.x,
            y,
            pane.width,
            row_base,
            i == selected,
            false,
            surface,
        );
    }
}

fn draw_section(
    editor: &mut Editor,
    section: Section,
    pane: Rect,
    surface: &mut Surface,
    base: Style,
) {
    if pane.height == 0 || pane.width == 0 {
        return;
    }
    let rows = panel::rows(editor, section);
    let count = rows.len();
    let height = pane.height as usize;

    // the console follows new output while its cursor is on the last line
    if section == Section::Console {
        let seen = editor.debug.console_seen;
        let cursor = editor.debug.cursor_mut(section);
        if seen == 0 || cursor.row + 1 >= seen {
            cursor.row = count.saturating_sub(1);
        }
        editor.debug.console_seen = count;
    }
    let cursor = editor.debug.cursor_mut(section);
    cursor.row = cursor.row.min(count.saturating_sub(1));
    if cursor.row < cursor.scroll {
        cursor.scroll = cursor.row;
    } else if cursor.row >= cursor.scroll + height {
        cursor.scroll = cursor.row + 1 - height;
    }
    cursor.scroll = cursor.scroll.min(count.saturating_sub(height));
    let cursor = *cursor;

    let theme = &editor.theme;
    let active = editor.debug.is_focused() && editor.debug.section == section;
    let selected = if active {
        theme.get("ui.menu.selected")
    } else {
        theme
            .try_get_exact("ui.cursorline.primary")
            .unwrap_or_else(|| theme.get("ui.cursorline"))
    };
    for (i, row) in rows.iter().enumerate().skip(cursor.scroll).take(height) {
        let y = pane.y + (i - cursor.scroll) as u16;
        let is_cursor = i == cursor.row && row.target != vortex_view::debug::Target::None;
        let row_base = if is_cursor {
            surface.set_style(Rect::new(pane.x, y, pane.width, 1), base.patch(selected));
            base.patch(selected)
        } else {
            base
        };
        draw_row(
            theme,
            row,
            pane.x,
            y,
            pane.width,
            row_base,
            is_cursor && active,
            editor.debug.is_rewinding(),
            surface,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    theme: &Theme,
    row: &Row,
    x: u16,
    y: u16,
    width: u16,
    base: Style,
    selected: bool,
    rewind: bool,
    surface: &mut Surface,
) {
    let mut x = x + 1;
    let end = x + width.saturating_sub(2);
    let indent = (row.depth * 2).min(width / 2);
    x += indent;
    let marker = match row.marker {
        Marker::None => "  ",
        Marker::Collapsed => "▸ ",
        Marker::Expanded => "▾ ",
        Marker::Current => "▶ ",
    };
    let marker_style = if row.marker == Marker::Current {
        role_style(theme, Role::Current)
    } else {
        theme.get("ui.text.inactive")
    };
    if x + 2 <= end {
        surface.set_stringn(x, y, marker, 2, base.patch(marker_style));
        x += 2;
    }
    for (text, role) in &row.parts {
        if x >= end {
            break;
        }
        // in rewind the values are greyed: they are history
        let role = match role {
            Role::Value if rewind => Role::Dim,
            role => *role,
        };
        let mut style = base.patch(role_style(theme, role));
        if selected {
            // the selection's colors win over the roles' backgrounds
            style.bg = base.bg;
        }
        let room = (end - x) as usize;
        let (next_x, _) = surface.set_stringn(x, y, text, room, style);
        if next_x >= end && text.chars().count() > room {
            surface.set_stringn(end.saturating_sub(1), y, "…", 1, style);
        }
        x = next_x;
    }
}
