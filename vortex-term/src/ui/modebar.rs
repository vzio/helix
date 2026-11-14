//! The mode bar (this fork): a bar of one to three rows above the message line, shown only
//! while a mode that uses it is on. Watch mode uses it (two rows: its state and its keys), and
//! debug mode (three: the transport bar, the lineline, the keys).
//!
//! A mode supplies a [`Bar`]: rows of segments placed left, middle and right, or a row of cells
//! as wide as the bar (a [`Line`], like debug mode's lineline, whose cells may be clicked). Each
//! segment is a [`Part::State`], a [`Part::Info`] or a [`Part::Keys`] badge, and the bar lays them out: when a
//! row doesn't fit, keys collapse first (their labels go, then the last badges), then the
//! information is shortened, and the state last. A badge names a command: it shows the keys the
//! keymap has for that command (a remapped key shows as remapped, an unbound command has no
//! badge), and a click on it runs the command.
//!
//! Nothing is laid out or drawn, and no row is taken, while no mode uses the bar.

use std::borrow::Cow;

use tui::buffer::Buffer as Surface;
use vortex_core::unicode::width::{UnicodeWidthChar, UnicodeWidthStr};
use vortex_view::{
    graphics::{Modifier, Rect, Style},
    input::KeyEvent,
    keyboard::KeyCode,
    Editor, Theme,
};

use crate::commands::MappableCommand;
use crate::keymap::{Keymaps, Layer, ReverseKeymap};

/// The most rows a mode may ask for.
pub const MAX_ROWS: u16 = 3;
/// The space between two segments of a group.
const GAP: u16 = 2;
/// The least space between two groups of a row.
const GROUP_GAP: u16 = 3;

/// What a segment is, which also decides what gives way first on a narrow window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    /// A passing note (what the last key did): cut first, before any key collapses.
    Note,
    /// The mode's state (`WATCH ● following`): shortened last.
    State,
    /// Information (the current change, the latest activity): shortened after the keys.
    Info,
    /// A key badge: collapses first.
    Keys,
}

/// A key badge: a command, its label (a word, or a glyph like `▶`), and the keys the keymap has
/// for the command.
#[derive(Debug, Clone)]
pub struct Badge {
    pub command: MappableCommand,
    pub label: Cow<'static, str>,
    /// The key shown, from the keymap; `None` when the command has no key (no badge is drawn).
    pub key: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Content {
    Text(String),
    Badge(Badge),
    /// Text in styles of its own (debug mode's loading animation): shortened like a text.
    Spans(Vec<(String, Style)>),
}

/// One segment of a row.
#[derive(Debug, Clone)]
pub struct Segment {
    pub part: Part,
    pub content: Content,
    /// Theme scopes tried before the part's own (`ui.modebar.state`, `ui.modebar.info`).
    pub scopes: Vec<&'static str>,
}

impl Segment {
    pub fn state(text: impl Into<String>) -> Self {
        Self {
            part: Part::State,
            content: Content::Text(text.into()),
            scopes: Vec::new(),
        }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self {
            part: Part::Info,
            content: Content::Text(text.into()),
            scopes: Vec::new(),
        }
    }

    /// A passing note: cut before anything else.
    pub fn note(text: impl Into<String>) -> Self {
        Self {
            part: Part::Note,
            content: Content::Text(text.into()),
            scopes: Vec::new(),
        }
    }

    /// A badge for `command`, labelled `label`; its key is looked up in `keys` (the keymap the
    /// mode's keys are in).
    pub fn key(command: &str, label: impl Into<Cow<'static, str>>, keys: &ReverseKeymap) -> Self {
        let command = command
            .parse::<MappableCommand>()
            .unwrap_or(MappableCommand::no_op);
        let key = keys
            .get(command.name())
            .and_then(|bindings| bindings.iter().find(|keys| keys.len() == 1))
            .map(|keys| key_label(&keys[0]));
        Self {
            part: Part::Keys,
            content: Content::Badge(Badge {
                command,
                label: label.into(),
                key,
            }),
            scopes: Vec::new(),
        }
    }

    /// A state drawn in styles of its own.
    pub fn spans(part: Part, spans: Vec<(String, Style)>) -> Self {
        Self {
            part,
            content: Content::Spans(spans),
            scopes: Vec::new(),
        }
    }

    /// Tries `scope` first for this segment's style.
    pub fn scope(mut self, scope: &'static str) -> Self {
        self.scopes.push(scope);
        self
    }

    fn visible(&self) -> bool {
        match &self.content {
            Content::Text(text) => !text.is_empty(),
            Content::Badge(badge) => badge.key.is_some(),
            Content::Spans(spans) => spans.iter().any(|(text, _)| !text.is_empty()),
        }
    }
}

/// One cell of a [`Line`].
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub symbol: char,
    pub style: Style,
    /// What a click on it means to the mode (see [`click_spot`]).
    pub spot: Option<u64>,
}

/// A row of cells as wide as the bar (inside its padding), drawn as they are: the mode lays it
/// out for the width it was given.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Line {
    pub cells: Vec<Cell>,
}

/// One row: its segments on the left, in the middle and on the right; or a [`Line`].
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub left: Vec<Segment>,
    pub middle: Vec<Segment>,
    pub right: Vec<Segment>,
    /// Drawn instead of the segments, when set.
    pub line: Option<Line>,
}

/// What a mode shows in the bar: one to three rows.
#[derive(Debug, Clone, Default)]
pub struct Bar {
    pub rows: Vec<Row>,
}

impl Bar {
    /// The rows the bar takes, at most [`MAX_ROWS`].
    pub fn height(&self) -> u16 {
        (self.rows.len() as u16).min(MAX_ROWS)
    }
}

/// How a key looks on a badge: `v`, `N`, `Esc`, `Space`, `C-x`.
pub fn key_label(key: &KeyEvent) -> String {
    let plain = KeyEvent {
        code: key.code,
        modifiers: vortex_view::keyboard::KeyModifiers::NONE,
    };
    let modifiers = key.to_string();
    let modifiers = modifiers
        .strip_suffix(&plain.to_string())
        .unwrap_or_default()
        .to_string();
    let name = match key.code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Backspace => "Bksp".to_string(),
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        _ => plain.to_string(),
    };
    format!("{modifiers}{name}")
}

/// The rows the bar takes now: 0 while no mode uses it. Cheap: no layout.
pub fn height(editor: &Editor) -> u16 {
    if editor.watch.is_active() {
        crate::watch::BAR_ROWS
    } else if editor.debug.mode_on() {
        crate::debug::bar::BAR_ROWS
    } else {
        0
    }
}

/// The bar of the mode that is on, if one uses it, for a bar `width` columns wide.
pub fn current(editor: &Editor, keymaps: &Keymaps, width: u16) -> Option<Bar> {
    if editor.watch.is_active() {
        let keys = keymaps.layer_reverse_map(Layer::Watch);
        return crate::watch::bar(editor, &keys);
    }
    if editor.debug.mode_on() {
        let keys = keymaps.layer_reverse_map(Layer::Debug);
        // a line is laid out inside the bar's padding
        return Some(crate::debug::bar::bar(
            editor,
            &keys,
            width.saturating_sub(2),
        ));
    }
    None
}

/// A click on a cell of a [`Line`] that has a spot: the mode that is on says what it means.
pub fn click_spot(editor: &mut Editor, spot: u64) {
    if editor.debug.mode_on() {
        crate::debug::bar::click(editor, spot);
    }
}

/// The bar as drawn: where its badges and spots are, for the mouse.
#[derive(Debug, Clone, Default)]
pub struct Drawn {
    pub area: Rect,
    pub badges: Vec<(Rect, MappableCommand)>,
    /// The cells of lines that can be clicked, with their spot.
    pub spots: Vec<((u16, u16), u64)>,
}

impl Drawn {
    /// The spot of the line's cell at `(column, row)`.
    pub fn spot_at(&self, column: u16, row: u16) -> Option<u64> {
        self.spots
            .iter()
            .find(|(cell, _)| *cell == (column, row))
            .map(|(_, spot)| *spot)
    }

    /// The command of the badge at the cell `(column, row)`.
    pub fn command_at(&self, column: u16, row: u16) -> Option<&MappableCommand> {
        self.badges
            .iter()
            .find(|(rect, _)| {
                (rect.left()..rect.right()).contains(&column)
                    && (rect.top()..rect.bottom()).contains(&row)
            })
            .map(|(_, command)| command)
    }
}

/// The styles of the bar.
struct Styles {
    bar: Style,
    state: Style,
    info: Style,
    key: Style,
    label: Style,
}

fn first(theme: &Theme, scopes: &[&str]) -> Option<Style> {
    scopes.iter().find_map(|scope| theme.try_get_exact(scope))
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        let bar = first(theme, &["ui.modebar", "ui.statusline"]).unwrap_or_default();
        let state = first(theme, &["ui.modebar.state", "ui.statusline.normal"])
            .unwrap_or_else(|| Style::default().add_modifier(Modifier::BOLD));
        let info = first(theme, &["ui.modebar.info", "ui.statusline"]).unwrap_or_default();
        let key = first(theme, &["ui.modebar.key", "ui.dashboard.key"]).unwrap_or_else(|| {
            match theme.try_get_exact("ui.selection") {
                Some(selection) => selection.add_modifier(Modifier::BOLD),
                None => Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            }
        });
        let label = first(
            theme,
            &["ui.modebar.label", "ui.dashboard.action", "ui.statusline"],
        )
        .unwrap_or_default();
        Self {
            bar,
            state: bar.patch(state),
            info: bar.patch(info),
            key: bar.patch(key),
            label: bar.patch(label),
        }
    }

    fn of(&self, theme: &Theme, segment: &Segment) -> Style {
        let own = first(theme, &segment.scopes).map(|style| self.bar.patch(style));
        own.unwrap_or(match segment.part {
            Part::State => self.state,
            Part::Info | Part::Note => self.info,
            Part::Keys => self.label,
        })
    }
}

/// How much a segment shows, once the row is fitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    All,
    /// A badge without its label.
    KeyOnly,
    /// A text cut to this width (with `…`).
    Cut(u16),
    Hidden,
}

fn text_width(text: &str) -> u16 {
    text.width() as u16
}

/// The width of a badge: ` key ` and ` label`.
fn badge_width(badge: &Badge, shown: Shown) -> u16 {
    let key = badge.key.as_deref().map_or(0, text_width) + 2;
    match shown {
        Shown::Hidden => 0,
        Shown::KeyOnly => key,
        _ if badge.label.is_empty() => key,
        _ => key + 1 + text_width(&badge.label),
    }
}

fn width_of(segment: &Segment, shown: Shown) -> u16 {
    match (&segment.content, shown) {
        (_, Shown::Hidden) => 0,
        (Content::Text(text), Shown::All | Shown::KeyOnly) => text_width(text),
        (Content::Spans(spans), Shown::All | Shown::KeyOnly) => {
            spans.iter().map(|(text, _)| text_width(text)).sum()
        }
        (Content::Text(_) | Content::Spans(_), Shown::Cut(width)) => width,
        (Content::Badge(badge), shown) => badge_width(badge, shown),
    }
}

/// `text` cut to `width` columns, ending in `…` when it was cut.
pub fn cut(text: &str, width: u16) -> String {
    if text_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0) as u16;
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// `text` cut to at most `width` columns, without `…`.
fn cut_plain(text: &str, width: u16) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0) as u16;
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// A row's segments with what each shows: the groups, in order.
struct Fitted<'a> {
    groups: [Vec<(&'a Segment, Shown)>; 3],
}

impl Fitted<'_> {
    fn group_width(group: &[(&Segment, Shown)]) -> u16 {
        let shown: Vec<u16> = group
            .iter()
            .map(|(segment, shown)| width_of(segment, *shown))
            .filter(|&width| width > 0)
            .collect();
        shown.iter().sum::<u16>() + GAP * shown.len().saturating_sub(1) as u16
    }

    fn width(&self) -> u16 {
        let widths: Vec<u16> = self
            .groups
            .iter()
            .map(|group| Self::group_width(group))
            .filter(|&width| width > 0)
            .collect();
        widths.iter().sum::<u16>() + GROUP_GAP * widths.len().saturating_sub(1) as u16
    }

    /// The segments of `part`, in the order they give way: the last first.
    fn of_part(&mut self, part: Part) -> Vec<&mut Shown> {
        let mut out: Vec<&mut Shown> = self
            .groups
            .iter_mut()
            .flat_map(|group| group.iter_mut())
            .filter(|(segment, shown)| segment.part == part && *shown != Shown::Hidden)
            .map(|(_, shown)| shown)
            .collect();
        out.reverse();
        out
    }
}

/// Fits a row into `width` columns: keys collapse (labels, then the last badges), then the
/// information is cut (the longest first), then the state.
fn fit(row: &Row, width: u16) -> Fitted<'_> {
    fn group(segments: &[Segment]) -> Vec<(&Segment, Shown)> {
        segments
            .iter()
            .filter(|segment| segment.visible())
            .map(|segment| (segment, Shown::All))
            .collect()
    }
    let mut fitted = Fitted {
        groups: [group(&row.left), group(&row.middle), group(&row.right)],
    };
    if fitted.width() <= width {
        return fitted;
    }
    // a note gives way first, down to nothing, so that it never hides the keys
    if !cut_part(&mut fitted, Part::Note, width) {
        return fitted;
    }
    // the keys: all labels go, then badges from the last
    for shown in fitted.of_part(Part::Keys) {
        *shown = Shown::KeyOnly;
    }
    loop {
        if fitted.width() <= width {
            return fitted;
        }
        match fitted.of_part(Part::Keys).into_iter().next() {
            Some(shown) => *shown = Shown::Hidden,
            None => break,
        }
    }
    // then the information, then the state: the widest is cut a column at a time, down to
    // nothing
    for part in [Part::Info, Part::State] {
        if !cut_part(&mut fitted, part, width) {
            return fitted;
        }
    }
    fitted
}

/// Cuts the segments of `part` (the widest first, a column at a time, down to nothing) until the
/// row fits: `false` once it does.
fn cut_part(fitted: &mut Fitted<'_>, part: Part, width: u16) -> bool {
    loop {
        let over = fitted.width().saturating_sub(width);
        if over == 0 {
            return false;
        }
        let widest = fitted
            .groups
            .iter_mut()
            .flat_map(|group| group.iter_mut())
            .filter(|(segment, shown)| segment.part == part && *shown != Shown::Hidden)
            .max_by_key(|(segment, shown)| width_of(segment, *shown));
        let Some((segment, shown)) = widest else {
            return true;
        };
        let now = width_of(segment, *shown);
        // a cut text keeps at least two columns (a character and `…`), or goes
        *shown = match now.saturating_sub(over) {
            left if left >= 2 => Shown::Cut(left),
            _ if now > 2 => Shown::Cut(2),
            _ => Shown::Hidden,
        };
    }
}

/// Draws `bar` in `area` (its last rows are the bar's), and returns where its badges went.
pub fn render(bar: &Bar, theme: &Theme, area: Rect, surface: &mut Surface) -> Drawn {
    let styles = Styles::new(theme);
    surface.set_style(area, styles.bar);
    let mut drawn = Drawn {
        area,
        badges: Vec::new(),
        spots: Vec::new(),
    };
    // a column of padding on each side
    let inner = Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    for (i, row) in bar.rows.iter().take(area.height as usize).enumerate() {
        let y = inner.y + i as u16;
        if let Some(line) = &row.line {
            for (x, cell) in (inner.x..inner.right()).zip(&line.cells) {
                let mut buf = [0; 4];
                surface.set_string(
                    x,
                    y,
                    cell.symbol.encode_utf8(&mut buf),
                    styles.bar.patch(cell.style),
                );
                if let Some(spot) = cell.spot {
                    drawn.spots.push(((x, y), spot));
                }
            }
            continue;
        }
        let fitted = fit(row, inner.width);
        let widths: Vec<u16> = fitted
            .groups
            .iter()
            .map(|group| Fitted::group_width(group))
            .collect();
        let left_end = inner.x + widths[0];
        let right_start = inner.right().saturating_sub(widths[2]);
        // the middle centered on the bar, moved aside when it would touch the others
        let middle_x = {
            let centered = inner.x + inner.width.saturating_sub(widths[1]) / 2;
            let lowest = if widths[0] > 0 {
                left_end + GROUP_GAP
            } else {
                inner.x
            };
            let highest = if widths[2] > 0 {
                right_start.saturating_sub(GROUP_GAP + widths[1])
            } else {
                inner.right().saturating_sub(widths[1])
            };
            centered.min(highest).max(lowest)
        };
        let starts = [inner.x, middle_x, right_start];
        for (group, &start) in fitted.groups.iter().zip(&starts) {
            let mut x = start;
            for &(segment, shown) in group {
                let width = width_of(segment, shown);
                if width == 0 {
                    continue;
                }
                let style = styles.of(theme, segment);
                match &segment.content {
                    Content::Text(text) => {
                        let text = match shown {
                            Shown::Cut(width) => cut(text, width),
                            _ => text.clone(),
                        };
                        surface.set_stringn(x, y, &text, width as usize, style);
                    }
                    Content::Spans(spans) => {
                        let full = width_of(segment, Shown::All);
                        // cut: the spans up to a column before the width, then `…`
                        let room = if width < full {
                            width.saturating_sub(1)
                        } else {
                            width
                        };
                        let mut used = 0;
                        for (text, span_style) in spans {
                            if used >= room {
                                break;
                            }
                            let shown = cut_plain(text, room - used);
                            surface.set_string(x + used, y, &shown, style.patch(*span_style));
                            used += text_width(&shown);
                        }
                        if width < full {
                            surface.set_string(x + used, y, "…", style);
                        }
                    }
                    Content::Badge(badge) => {
                        let key = format!(" {} ", badge.key.as_deref().unwrap_or_default());
                        surface.set_string(x, y, &key, styles.key);
                        if shown == Shown::All && !badge.label.is_empty() {
                            let label_x = x + text_width(&key) + 1;
                            surface.set_string(label_x, y, &badge.label, style);
                        }
                        drawn
                            .badges
                            .push((Rect::new(x, y, width, 1), badge.command.clone()));
                    }
                }
                x += width + GAP;
            }
        }
    }
    drawn
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::KeyTrie;
    use vortex_core::hashmap;

    fn keys(pairs: &[(&str, &str)]) -> ReverseKeymap {
        let mut map = ReverseKeymap::new();
        for (command, key) in pairs {
            map.entry(command.to_string())
                .or_default()
                .push(vec![key.parse().unwrap()]);
        }
        map
    }

    fn text(surface: &Surface) -> Vec<String> {
        let area = surface.area;
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| surface[(x, y)].symbol.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn draw(bar: &Bar, width: u16) -> (Vec<String>, Drawn) {
        let area = Rect::new(0, 0, width, bar.height());
        let mut surface = Surface::empty(area);
        let drawn = render(bar, &Theme::default(), area, &mut surface);
        (text(&surface), drawn)
    }

    /// A watch-like bar: the state, the change and the activity, then the keys.
    fn two_rows(keys: &ReverseKeymap) -> Bar {
        Bar {
            rows: vec![
                Row {
                    left: vec![Segment::state("WATCH ● following")],
                    middle: vec![Segment::info("src/main.rs · 3/12 changes")],
                    right: vec![Segment::info("followed src/main.rs:30")],
                    line: None,
                },
                Row {
                    left: vec![
                        Segment::key("watch_look", "look", keys),
                        Segment::key("watch_next_change", "next", keys),
                        Segment::key("watch_prev_change", "previous", keys),
                        Segment::key("watch_leave", "leave", keys),
                    ],
                    ..Row::default()
                },
            ],
        }
    }

    fn watch_keys() -> ReverseKeymap {
        keys(&[
            ("watch_look", "v"),
            ("watch_next_change", "n"),
            ("watch_prev_change", "N"),
            ("watch_leave", "esc"),
        ])
    }

    #[test]
    fn one_row() {
        let bar = Bar {
            rows: vec![Row {
                left: vec![Segment::state("STATE")],
                middle: vec![Segment::info("middle")],
                right: vec![Segment::info("right")],
                line: None,
            }],
        };
        assert_eq!(bar.height(), 1);
        let (rows, _) = draw(&bar, 40);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert!(row.starts_with(" STATE"), "{row:?}");
        assert!(row.ends_with("right"), "{row:?}");
        // the middle is centered
        let middle = row.find("middle").unwrap();
        assert_eq!(middle, (1 + (38 - 6) / 2) as usize, "{row:?}");
    }

    #[test]
    fn two_rows_with_badges() {
        let keys = watch_keys();
        let (rows, drawn) = draw(&two_rows(&keys), 100);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].starts_with(" WATCH ● following"), "{rows:?}");
        assert!(rows[0].contains("src/main.rs · 3/12 changes"), "{rows:?}");
        assert!(rows[0].ends_with("followed src/main.rs:30"), "{rows:?}");
        assert_eq!(
            rows[1], "  v  look   n  next   N  previous   Esc  leave",
            "{rows:?}"
        );
        // every badge can be clicked, on its key and on its label
        assert_eq!(drawn.badges.len(), 4);
        let names: Vec<&str> = drawn.badges.iter().map(|(_, c)| c.name()).collect();
        assert_eq!(
            names,
            [
                "watch_look",
                "watch_next_change",
                "watch_prev_change",
                "watch_leave"
            ]
        );
        let label = rows[1].find("previous").unwrap() as u16;
        assert_eq!(
            drawn.command_at(label + 2, 1).map(|c| c.name()),
            Some("watch_prev_change")
        );
        assert_eq!(drawn.command_at(label + 2, 0).map(|c| c.name()), None);
    }

    /// A fake three-row mode, as debug mode will be: the transport bar with a glyph and a key per
    /// button, the lineline, the keys.
    #[test]
    fn three_rows_like_debug_mode() {
        let keys = keys(&[
            ("dap_restart", "R"),
            ("dap_rewind_back", "["),
            ("dap_play", "space"),
            ("dap_pause", "p"),
            ("dap_fast_forward", "f"),
            ("dap_live", "L"),
            ("dap_step_in", "i"),
            ("dap_next", "n"),
            ("dap_step_out", "o"),
            ("normal_mode", "esc"),
        ]);
        let transport = ["|◀", "◀◀", "▶", "❚❚", "▶▶", "▶|"];
        let commands = [
            "dap_restart",
            "dap_rewind_back",
            "dap_play",
            "dap_pause",
            "dap_fast_forward",
            "dap_live",
        ];
        let bar = Bar {
            rows: vec![
                Row {
                    left: vec![Segment::state("DEBUG ▶ playing")],
                    middle: commands
                        .iter()
                        .zip(transport)
                        .map(|(command, glyph)| Segment::key(command, glyph, &keys))
                        .collect(),
                    right: vec![Segment::info("4 lines/s")],
                    line: None,
                },
                Row {
                    left: vec![Segment::info("▁▂▃▅▇█▇▅▃▂▁ lineline")],
                    right: vec![Segment::info("main.rs:42")],
                    ..Row::default()
                },
                Row {
                    left: vec![
                        Segment::key("dap_step_in", "in", &keys),
                        Segment::key("dap_next", "over", &keys),
                        Segment::key("dap_step_out", "out", &keys),
                        Segment::key("normal_mode", "leave", &keys),
                    ],
                    ..Row::default()
                },
            ],
        };
        assert_eq!(bar.height(), 3);
        let (rows, drawn) = draw(&bar, 120);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].contains(" R  |◀   [  ◀◀   Space  ▶   p  ❚❚   f  ▶▶   L  ▶|"));
        assert!(rows[1].contains("lineline") && rows[1].ends_with("main.rs:42"));
        assert!(rows[2].contains(" i  in   n  over   o  out   Esc  leave"));
        // the transport's buttons are clickable
        let play = rows[0].find("▶   p").unwrap();
        let play = rows[0][..play].width() as u16;
        assert_eq!(
            drawn.command_at(play, 0).map(|c| c.name()),
            Some("dap_play")
        );
        // no emoji presentation in the glyphs
        for glyph in transport {
            assert!(!glyph.contains('\u{fe0f}'));
        }
    }

    /// Keys collapse first (labels, then the last badges), then the information is cut, then
    /// the state.
    #[test]
    fn truncation_order() {
        let keys = watch_keys();
        let bar = two_rows(&keys);

        // the keys' labels go before any badge
        let (rows, _) = draw(&bar, 30);
        assert_eq!(rows[1], "  v    n    N    Esc", "{rows:?}");
        // then the last badges
        let (rows, _) = draw(&bar, 12);
        assert_eq!(rows[1], "  v    n", "{rows:?}");

        // row 1: the information is cut, the state stays whole
        let (rows, _) = draw(&bar, 50);
        assert!(rows[0].starts_with(" WATCH ● following"), "{rows:?}");
        assert!(rows[0].contains('…'), "{rows:?}");
        // narrower: the information is down to almost nothing before the state is touched
        let (rows, _) = draw(&bar, 30);
        assert!(rows[0].starts_with(" WATCH ● following"), "{rows:?}");
        // and the state goes last
        let (rows, _) = draw(&bar, 12);
        assert!(rows[0].starts_with(" WATCH"), "{rows:?}");
        assert!(rows[0].contains('…'), "{rows:?}");
        // nothing spills over
        for width in 4..120 {
            let (rows, _) = draw(&bar, width);
            for row in rows {
                assert!(row.width() as u16 <= width, "{width}: {row:?}");
            }
        }
    }

    /// A note gives way before any key loses its label.
    #[test]
    fn a_note_is_cut_before_the_keys() {
        let keys = watch_keys();
        let mut bar = two_rows(&keys);
        bar.rows[1].right = vec![Segment::note(
            "Stopped at the first line of your code that ran",
        )];
        let (rows, _) = draw(&bar, 60);
        assert!(
            rows[1].starts_with("  v  look   n  next   N  previous   Esc  leave"),
            "{rows:?}"
        );
        assert!(rows[1].contains('…'), "{rows:?}");
        // narrower than the keys: the note is gone first
        let (rows, _) = draw(&bar, 48);
        assert_eq!(
            rows[1], "  v  look   n  next   N  previous   Esc  leave",
            "{rows:?}"
        );
    }

    #[test]
    fn remapped_and_unbound_keys() {
        // `v` remapped to `l`, `watch_prev_change` unbound: no badge for it
        let keys = keys(&[
            ("watch_look", "l"),
            ("watch_next_change", "n"),
            ("watch_leave", "C-q"),
        ]);
        let (rows, drawn) = draw(&two_rows(&keys), 100);
        assert_eq!(rows[1], "  l  look   n  next   C-q  leave", "{rows:?}");
        assert_eq!(drawn.badges.len(), 3);
    }

    #[test]
    fn badges_take_their_keys_from_a_layer() {
        use crate::keymap;
        let layer = keymap!({ "Watch"
            "x" => watch_look,
        });
        let KeyTrie::Node(_) = &layer else {
            panic!("a node")
        };
        let mut keymaps = Keymaps::default();
        keymaps.set_layers(hashmap! { Layer::Watch => layer });
        let keys = keymaps.layer_reverse_map(Layer::Watch);
        let segment = Segment::key("watch_look", "look", &keys);
        let Content::Badge(badge) = segment.content else {
            panic!("a badge")
        };
        assert_eq!(badge.key.as_deref(), Some("x"));
    }

    #[test]
    fn key_labels() {
        for (key, label) in [
            ("v", "v"),
            ("N", "N"),
            ("esc", "Esc"),
            ("space", "Space"),
            ("C-x", "C-x"),
            ("A-ret", "A-Enter"),
        ] {
            assert_eq!(key_label(&key.parse().unwrap()), label);
        }
    }
}
