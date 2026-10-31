//! The panel with the lesson, drawn over the editor while an exercise is open.
//!
//! On a wide terminal it is a column at the top right, on a narrow one (a phone) a band over the
//! bottom of the editor, above the statusline. It only draws, it takes no events: `:practice
//! lesson` shows and hides it.
//!
//! The panel is made like the info popup of the which-key help (`ui/info.rs`): a bordered box
//! with its title in the border, filled with the popup style, and a margin of one column between
//! the border and the text. On top of that, the wide column has a blank row after the border and
//! before it, and the sections (the header, `Lesson`, `Goal`, `Hints` and the count) are one blank
//! row apart. The band has no blank rows, it has little room.
//!
//! Spans in backticks in the lesson and in the hints are drawn in the style of keys, without the
//! backticks (two backticks in a row are one backtick of the text). The lines of the goal are not
//! wrapped, they are text to compare with: what doesn't fit is cut off with a `…`.
//!
//! # Theme scopes
//!
//! Each scope is looked up exactly (not through its parents) in the order given, the first the
//! theme defines wins. The style of every part is put on top of the style of the panel.
//!
//! | Scope | Used for | Falls back to |
//! | --- | --- | --- |
//! | `ui.practice` | the panel: border and background | `ui.popup.info`, `ui.popup` |
//! | `ui.practice.lesson` | the lesson, the title of the exercise (bold) | `ui.text.info`, `ui.text` |
//! | `ui.practice.goal` | the block with the goal | `markup.raw.block`, `markup.raw`, `ui.text` |
//! | `ui.practice.hint` | the revealed hints | `hint`, `ui.text.info` |
//! | `ui.practice.label` | `Lesson`, `Goal`, `Hints`, `Teach`, `Gold` | `markup.heading`, `ui.text.focus` (both bold) |
//! | `ui.practice.key` | spans in backticks, the solutions, commands | `markup.raw.inline`, `markup.raw`, else bold |
//! | `ui.practice.status` | the count of keys | `ui.text.info` (bold) |
//! | `ui.practice.solved` | the line with the result once solved | `diff.plus`, `info` (bold) |
//! | `ui.practice.dim` | secondary text, and the bar before the goal (its foreground) | `ui.text.inactive`, `comment`, else dim |

use helix_core::unicode::width::UnicodeWidthChar;
use helix_view::graphics::{Margin, Modifier, Rect, Style};
use helix_view::{Editor, Theme};
use tui::buffer::Buffer as Surface;
use tui::widgets::{Block, Widget};

use super::exercise::{Exercise, Kind};
use super::session::{self, Attempt};

/// Editor areas at least this wide get a column on the right instead of a band at the bottom.
pub const WIDE_WIDTH: u16 = 100;
/// The width of the column: the longest line of a goal (50) and what is around it.
const COLUMN_WIDTH: u16 = 56;
/// The most of the editor area the column gets, in percent.
const COLUMN_SHARE: u32 = 45;
/// The columns around the text: the border and the margin, on both sides.
const FRAME: u16 = 4;
/// The columns before a line of the goal: the bar and a space.
const GOAL_INDENT: usize = 2;
const GOAL_BAR: char = '▎';
/// The columns of the label before a solution (`Teach`, `Gold`).
const SOLUTION_LABEL: usize = 7;

/// What a part of the panel is, which decides its style (see the scopes above).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Title,
    Label,
    Lesson,
    Key,
    Goal,
    Bar,
    Hint,
    Status,
    Solved,
    Dim,
}

/// The styles of the roles in a theme.
struct Styles {
    panel: Style,
    title: Style,
    label: Style,
    lesson: Style,
    key: Style,
    goal: Style,
    bar: Style,
    hint: Style,
    status: Style,
    solved: Style,
    dim: Style,
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        // the first scope that the theme defines itself, not through a parent: the parents of
        // these scopes (`ui`, `markup`) are not what the fallbacks are meant to reach
        let first = |scopes: &[&str]| scopes.iter().find_map(|scope| theme.try_get_exact(scope));
        let bold = |style: Option<Style>, otherwise: Style| match style {
            Some(style) => style.add_modifier(Modifier::BOLD),
            None => otherwise.add_modifier(Modifier::BOLD),
        };

        let panel = first(&["ui.practice", "ui.popup.info", "ui.popup"]).unwrap_or_default();
        let lesson = first(&["ui.practice.lesson", "ui.text.info", "ui.text"]).unwrap_or_default();
        let goal = first(&[
            "ui.practice.goal",
            "markup.raw.block",
            "markup.raw",
            "ui.text",
        ])
        .unwrap_or_default();
        let dim = first(&["ui.practice.dim", "ui.text.inactive", "comment"])
            .unwrap_or_else(|| Style::default().add_modifier(Modifier::DIM));
        // the background of the dim text is the one of what it is on
        let bar = goal.patch(Style { bg: None, ..dim });
        let label = first(&["ui.practice.label"])
            .unwrap_or_else(|| bold(first(&["markup.heading", "ui.text.focus"]), lesson));
        let key = first(&["ui.practice.key", "markup.raw.inline", "markup.raw"])
            .unwrap_or_else(|| Style::default().add_modifier(Modifier::BOLD));
        let hint = first(&["ui.practice.hint", "hint", "ui.text.info"]).unwrap_or(lesson);
        let status = first(&["ui.practice.status"])
            .unwrap_or_else(|| bold(first(&["ui.text.info"]), lesson));
        let solved = first(&["ui.practice.solved"])
            .unwrap_or_else(|| bold(first(&["diff.plus", "info"]), lesson));
        Self {
            panel,
            title: lesson.add_modifier(Modifier::BOLD),
            label,
            lesson,
            key,
            goal,
            bar,
            hint,
            status,
            solved,
            dim,
        }
    }

    /// The style of a cell with this role: the style of the role on top of the one of the panel.
    fn get(&self, role: Role) -> Style {
        self.panel.patch(match role {
            Role::Title => self.title,
            Role::Label => self.label,
            Role::Lesson => self.lesson,
            Role::Key => self.key,
            Role::Goal => self.goal,
            Role::Bar => self.bar,
            Role::Hint => self.hint,
            Role::Status => self.status,
            Role::Solved => self.solved,
            Role::Dim => self.dim,
        })
    }
}

/// A character of the panel and what it is.
type Cell = (char, Role);

/// A line of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    cells: Vec<Cell>,
    /// A role whose style the whole row has, also after the last cell (the block of the goal).
    fill: Option<Role>,
}

impl Line {
    fn new(cells: Vec<Cell>) -> Self {
        Self { cells, fill: None }
    }

    fn blank() -> Self {
        Self::new(Vec::new())
    }

    fn text(text: &str, role: Role) -> Self {
        Self::new(text.chars().map(|c| (c, role)).collect())
    }

    fn is_blank(&self) -> bool {
        self.cells.is_empty()
    }
}

/// The characters of `text` as cells of the role, except for the spans in backticks, which are
/// keys. A doubled backtick is a backtick of the text.
fn inline(text: &str, role: Role) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut in_key = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            if chars.next_if_eq(&'`').is_some() {
                cells.push(('`', if in_key { Role::Key } else { role }));
            } else {
                in_key = !in_key;
            }
        } else {
            cells.push((c, if in_key { Role::Key } else { role }));
        }
    }
    cells
}

fn plain(text: &str, role: Role) -> Vec<Cell> {
    text.chars().map(|c| (c, role)).collect()
}

fn cells_width(cells: &[Cell]) -> usize {
    cells.iter().map(|(c, _)| c.width().unwrap_or(0)).sum()
}

/// Breaks `text` into lines of at most `width` columns, at spaces, or inside words that are
/// longer than a line. The leading whitespace of the text is kept, and continuation lines are
/// indented by `hanging` more columns. Empty text is one empty line. `base` is the role of the
/// spaces that are added.
fn wrap(text: &[Cell], width: usize, hanging: usize, base: Role) -> Vec<Vec<Cell>> {
    let width = width.max(1);
    let is_space = |(c, _): &Cell| c.is_whitespace();
    let indent: Vec<Cell> = text
        .iter()
        .take_while(|cell| is_space(cell))
        .copied()
        .collect();
    let continuation: Vec<Cell> = plain(&" ".repeat(cells_width(&indent) + hanging), base);
    let mut lines = Vec::new();
    let mut line = indent.clone();
    // whether `line` has a word, not only the indentation
    let mut has_word = false;

    for word in text[indent.len()..]
        .split(is_space)
        .filter(|word| !word.is_empty())
    {
        let mut word = word;
        loop {
            let separator = usize::from(has_word);
            if cells_width(&line) + separator + cells_width(word) <= width {
                if has_word {
                    // inside a span of keys the space belongs to it
                    let role = match (line.last(), word.first()) {
                        (Some((_, before)), Some((_, after))) if before == after => *before,
                        _ => base,
                    };
                    line.push((' ', role));
                }
                line.extend_from_slice(word);
                has_word = true;
                break;
            }
            if has_word {
                lines.push(std::mem::replace(&mut line, continuation.clone()));
                has_word = false;
                continue;
            }
            // a word that doesn't fit a line of its own: cut it
            let room = width.saturating_sub(cells_width(&line)).max(1);
            let mut taken = 0;
            let mut cut = 0;
            for (at, (c, _)) in word.iter().enumerate() {
                let w = c.width().unwrap_or(0);
                if taken + w > room && cut > 0 {
                    break;
                }
                taken += w;
                cut = at + 1;
            }
            line.extend_from_slice(&word[..cut]);
            lines.push(std::mem::replace(&mut line, continuation.clone()));
            word = &word[cut..];
            if word.is_empty() {
                break;
            }
        }
    }
    if has_word || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Cuts `cells` to `width` columns, with a `…` at the end when something was cut off.
fn clip(mut cells: Vec<Cell>, width: usize) -> Vec<Cell> {
    if cells_width(&cells) <= width {
        return cells;
    }
    ellipsis(&mut cells, width);
    cells
}

/// Makes the line end in a `…` that fits in `width` columns.
fn ellipsis(cells: &mut Vec<Cell>, width: usize) {
    let role = cells.last().map_or(Role::Lesson, |(_, role)| *role);
    while !cells.is_empty() && cells_width(cells) >= width.max(1) {
        cells.pop();
    }
    cells.push(('…', role));
}

/// The lines of a lesson as the panel wraps them. The lesson is written in lines of 80 columns,
/// which the panel breaks again to its own width, so a line break is a space, unless the next
/// line is blank (a paragraph break, which stays) or starts a list item or is indented.
fn reflow(text: &str) -> Vec<String> {
    let starts_item = |line: &str| {
        let trimmed = line.trim_start();
        trimmed.len() < line.len()
            || trimmed.starts_with(['-', '*', '•'])
            || trimmed.split_once(['.', ')']).is_some_and(|(number, _)| {
                !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())
            })
    };
    let mut lines: Vec<String> = Vec::new();
    let mut continues = false;
    for line in text.trim_end().lines() {
        if line.trim().is_empty() {
            lines.push(String::new());
            continues = false;
        } else if continues && !starts_item(line) {
            let last = lines.last_mut().unwrap();
            last.push(' ');
            last.push_str(line.trim());
        } else {
            lines.push(line.trim_end().to_owned());
            continues = true;
        }
    }
    lines
}

/// The lines of the panel that belong together, and how much they matter when there is not room
/// for all of them.
struct Part {
    /// Lower is more important.
    rank: u8,
    /// Whether a block that is cut off says so with a `…`.
    ellipsis: bool,
    lines: Vec<Line>,
}

const RANK_COUNT: u8 = 1;
const RANK_GOAL: u8 = 2;
const RANK_HINTS: u8 = 3;
const RANK_RESULT: u8 = 3;
const RANK_HEADER: u8 = 4;
const RANK_LESSON: u8 = 5;
const RANK_TIP: u8 = 6;

/// What the panel shows, for lines that are `width` columns wide.
struct Content {
    /// The title in the border.
    title: String,
    /// The blocks in the order of the panel.
    blocks: Vec<Part>,
}

/// The lines of a section: the label, the body and `gap` blank lines.
fn section(label: Vec<Cell>, body: Vec<Line>, gap: usize) -> Vec<Line> {
    let mut lines = vec![Line::new(label)];
    lines.extend(body);
    lines.extend((0..gap).map(|_| Line::blank()));
    lines
}

fn wrapped(cells: &[Cell], width: usize, hanging: usize, base: Role) -> Vec<Line> {
    wrap(cells, width, hanging, base)
        .into_iter()
        .map(Line::new)
        .collect()
}

/// The content for lines of `width` columns, with `gap` blank lines between the sections.
fn content(exercises: &[Exercise], attempt: &Attempt, width: usize, gap: usize) -> Content {
    let exercise = &exercises[attempt.exercise];
    let title = format!(
        "{}.{} / {} · {}",
        exercise.chapter_number, exercise.number, exercise.chapter_len, exercise.chapter_title
    );
    let blank = |count: usize| (0..count).map(|_| Line::blank());
    let mut blocks = Vec::new();

    // the kind is said when it is a challenge: there is no lesson to lean on
    let mut header = Vec::new();
    if exercise.kind == Kind::Challenge {
        header.extend(plain("Challenge", Role::Label));
        header.extend(plain(" · ", Role::Dim));
    }
    header.extend(plain(&exercise.title, Role::Title));
    let mut lines = wrapped(&header, width, 0, Role::Title);
    lines.extend(blank(gap));
    blocks.push(Part {
        rank: RANK_HEADER,
        ellipsis: false,
        lines,
    });

    let mut body = Vec::new();
    for line in reflow(&exercise.lesson) {
        body.extend(wrapped(
            &inline(&line, Role::Lesson),
            width,
            0,
            Role::Lesson,
        ));
    }
    if !body.is_empty() {
        blocks.push(Part {
            rank: RANK_LESSON,
            ellipsis: true,
            lines: section(plain("Lesson", Role::Label), body, gap),
        });
    }

    // not wrapped, it is text to compare with
    let body = exercise
        .goal
        .lines()
        .map(|line| {
            let mut cells = vec![(GOAL_BAR, Role::Bar), (' ', Role::Goal)];
            cells.extend(clip(
                plain(&line.replace('\t', "    "), Role::Goal),
                width.saturating_sub(GOAL_INDENT),
            ));
            Line {
                cells,
                fill: Some(Role::Goal),
            }
        })
        .collect();
    blocks.push(Part {
        rank: RANK_GOAL,
        ellipsis: false,
        lines: section(plain("Goal", Role::Label), body, gap),
    });

    let revealed = attempt.hints.min(exercise.hints.len());
    if revealed > 0 {
        let mut label = plain("Hints ", Role::Label);
        label.extend(plain(
            &format!("{revealed}/{}", exercise.hints.len()),
            Role::Dim,
        ));
        let mut body = Vec::new();
        for (n, hint) in exercise.hints.iter().take(revealed).enumerate() {
            let mut cells = plain(&format!("{} ", n + 1), Role::Dim);
            cells.extend(inline(hint, Role::Hint));
            body.extend(wrapped(&cells, width, 2, Role::Hint));
        }
        blocks.push(Part {
            rank: RANK_HINTS,
            ellipsis: false,
            lines: section(label, body, gap),
        });
    }

    let par = exercise.par();
    match attempt.solved_at {
        None => {
            let mut line = format!("Keys {} · par {par}", attempt.keys);
            if let Some(best) = attempt.best {
                line.push_str(&format!(" · best {best}"));
            }
            blocks.push(Part {
                rank: RANK_COUNT,
                ellipsis: false,
                lines: wrapped(&plain(&line, Role::Status), width, 0, Role::Status),
            });
            if revealed < exercise.hints.len() {
                let tip = format!(
                    ":practice hint gives a hint ({revealed}/{})",
                    exercise.hints.len()
                );
                blocks.push(Part {
                    rank: RANK_TIP,
                    ellipsis: false,
                    lines: wrapped(&plain(&tip, Role::Dim), width, 0, Role::Dim),
                });
            }
        }
        Some(keys) => {
            let best = attempt.best.unwrap_or(keys);
            let unit = if keys == 1 { "key" } else { "keys" };
            let result = format!("Solved in {keys} {unit} · par {par} · best {best}");
            blocks.push(Part {
                rank: RANK_COUNT,
                ellipsis: false,
                lines: wrapped(&plain(&result, Role::Solved), width, 0, Role::Solved),
            });

            // the solutions, and what the feature saved
            let mut lines: Vec<Line> = blank(gap).collect();
            let solution = |label: &str, keys: &str| {
                let indent = SOLUTION_LABEL;
                let mut lines = wrapped(
                    &plain(keys, Role::Key),
                    width.saturating_sub(indent),
                    0,
                    Role::Key,
                );
                for (at, line) in lines.iter_mut().enumerate() {
                    let before = if at == 0 {
                        format!("{label:<indent$}")
                    } else {
                        " ".repeat(indent)
                    };
                    line.cells.splice(0..0, plain(&before, Role::Label));
                }
                lines
            };
            lines.extend(solution("Teach", &exercise.teach));
            if let Some(gold) = exercise.gold.as_ref().filter(|_| best <= par) {
                lines.extend(solution("Gold", gold));
            }
            let naive = exercise.naive_keys();
            if naive > 0 {
                let unit = if naive == 1 { "key" } else { "keys" };
                let text = format!("Without the feature: {naive} {unit}");
                lines.extend(wrapped(&plain(&text, Role::Lesson), width, 0, Role::Lesson));
            }
            if attempt.exercise + 1 < exercises.len() {
                lines.extend(blank(gap));
                lines.push(Line::text(":practice next", Role::Key));
            }
            blocks.push(Part {
                rank: RANK_RESULT,
                ellipsis: false,
                lines,
            });
        }
    }
    Content { title, blocks }
}

/// The lines that fit in `height` rows of `width` columns, in the order of the panel. When there
/// is not room for all, the blocks go from the least important one: the tip, the lesson (cut off
/// with a `…`), the header, then the solutions or the hints and the goal, last the count or the
/// result. With `pad`, a blank line is added before and after, if there is room for both.
fn fit(content: Content, height: usize, width: usize, pad: bool) -> Vec<Line> {
    let mut room = height;
    let mut order: Vec<usize> = (0..content.blocks.len()).collect();
    order.sort_by_key(|&at| content.blocks[at].rank);
    let mut taken = vec![0; content.blocks.len()];
    for at in order {
        let block = &content.blocks[at];
        let mut n = block.lines.len().min(room);
        // a section that says it was cut off is shown with a line of its body, or not at all
        if block.ellipsis && n < block.lines.len() && n < 2 {
            n = 0;
        }
        taken[at] = n;
        room -= n;
    }

    let mut lines: Vec<Line> = Vec::new();
    for (block, taken) in content.blocks.into_iter().zip(taken) {
        let cut = block.lines[taken..].iter().any(|line| !line.is_blank());
        let start = lines.len();
        lines.extend(block.lines.into_iter().take(taken));
        if cut && block.ellipsis {
            while lines.len() > start + 1 && lines.last().is_some_and(Line::is_blank) {
                lines.pop();
            }
            // (the first line of the section is its label)
            if lines.len() > start + 1 {
                ellipsis(&mut lines.last_mut().unwrap().cells, width);
            }
        }
    }
    while lines.last().is_some_and(Line::is_blank) {
        lines.pop();
    }
    if pad && !lines.is_empty() && lines.len() + 2 <= height {
        lines.insert(0, Line::blank());
        lines.push(Line::blank());
    }
    lines
}

fn is_wide(editor_area: Rect) -> bool {
    editor_area.width >= WIDE_WIDTH
}

/// The width of the panel, border included, for a goal whose longest line has `goal_width`
/// columns: the column fits the goal if the editor area allows it.
fn panel_width(editor_area: Rect, goal_width: usize) -> u16 {
    if is_wide(editor_area) {
        let wanted = (goal_width + GOAL_INDENT + usize::from(FRAME)).max(usize::from(COLUMN_WIDTH));
        let most = u32::from(editor_area.width) * COLUMN_SHARE / 100;
        u16::try_from(wanted.min(most as usize)).unwrap_or(u16::MAX)
    } else {
        editor_area.width
    }
}

/// The most rows the panel gets, border included. The column leaves the row of the statusline of
/// the view free. The band leaves the `text_rows` lines of the buffer visible above it, and gets
/// at least half of the area when they need more.
fn max_height(editor_area: Rect, text_rows: u16) -> u16 {
    let below_statusline = editor_area.height.saturating_sub(1);
    if is_wide(editor_area) {
        below_statusline
    } else {
        below_statusline
            .saturating_sub(text_rows)
            .max(editor_area.height / 2)
    }
}

/// The area of the panel in `editor_area`, `width` columns wide, for `lines` lines of text.
fn panel_area(editor_area: Rect, width: u16, text_rows: u16, lines: usize) -> Rect {
    let height = u16::try_from(lines + 2)
        .unwrap_or(u16::MAX)
        .min(max_height(editor_area, text_rows));
    if is_wide(editor_area) {
        Rect::new(editor_area.right() - width, editor_area.y, width, height)
    } else {
        // above the statusline of the view
        let bottom = editor_area.bottom().saturating_sub(1);
        Rect::new(editor_area.x, bottom.saturating_sub(height), width, height)
    }
}

/// Draws the panel in `editor_area`, if there is an exercise open in the focused view, the panel
/// is not hidden and the popup with the keys of a pending command isn't showing.
pub fn render(editor: &Editor, editor_area: Rect, surface: &mut Surface) {
    if !session::panel_visible() {
        return;
    }
    let Some(attempt) = session::live_attempt(editor) else {
        return;
    };
    let (view, doc) = current_ref!(editor);
    if view.doc != attempt.doc || (editor.autoinfo.is_some() && editor.config().auto_info) {
        return;
    }
    let Ok(exercises) = session::exercises() else {
        return;
    };
    if attempt.exercise >= exercises.len() {
        return;
    }

    // at least a border, a margin and a column of text
    if editor_area.width < 5 || editor_area.height < 4 {
        return;
    }
    let wide = is_wide(editor_area);
    let goal_width = exercises[attempt.exercise]
        .goal
        .lines()
        .map(|line| {
            line.replace('\t', "    ")
                .chars()
                .filter_map(|c| c.width())
                .sum()
        })
        .max()
        .unwrap_or(0);
    let width = panel_width(editor_area, goal_width);
    let text_width = usize::from(width.saturating_sub(FRAME));
    let content = content(&exercises, &attempt, text_width, usize::from(wide));
    let title = content.title.clone();
    // the lines of the buffer, without the empty one after the final line ending
    let text_rows = u16::try_from(doc.text().len_lines().saturating_sub(1)).unwrap_or(u16::MAX);
    let rows = usize::from(max_height(editor_area, text_rows).saturating_sub(2));
    let lines = fit(content, rows, text_width, wide);
    let area = panel_area(editor_area, width, text_rows, lines.len());
    if area.width < 5 || area.height < 3 {
        return;
    }

    let styles = Styles::new(&editor.theme);
    surface.clear_with(area, styles.panel);
    let block = Block::bordered().title(title).border_style(styles.panel);
    let inner = block.inner(area).inner(Margin::horizontal(1));
    block.render(area, surface);

    for (row, line) in lines.iter().enumerate().take(usize::from(inner.height)) {
        let y = inner.y + row as u16;
        if let Some(role) = line.fill {
            surface.set_style(Rect::new(inner.x, y, inner.width, 1), styles.get(role));
        }
        // runs of cells of one role are drawn together
        let mut x = inner.x;
        let mut run = String::new();
        let mut cells = line.cells.iter().peekable();
        while let Some(&(c, role)) = cells.next() {
            run.push(c);
            if cells.peek().is_some_and(|(_, next)| *next == role) {
                continue;
            }
            let left = usize::from(inner.right().saturating_sub(x));
            (x, _) = surface.set_stringn(x, y, &run, left, styles.get(role));
            run.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(text: &str) -> Vec<Cell> {
        plain(text, Role::Lesson)
    }

    fn string(cells: &[Cell]) -> String {
        cells.iter().map(|(c, _)| c).collect()
    }

    fn wrapped_text(text: &str, width: usize, hanging: usize) -> Vec<String> {
        wrap(&cells(text), width, hanging, Role::Lesson)
            .iter()
            .map(|line| string(line))
            .collect()
    }

    #[test]
    fn wraps_at_spaces() {
        assert_eq!(
            wrapped_text("the quick brown fox", 9, 0),
            ["the quick", "brown fox"]
        );
        assert_eq!(
            wrapped_text("the quick brown fox", 10, 0),
            ["the quick", "brown fox"]
        );
        assert_eq!(
            wrapped_text("the quick brown fox", 100, 0),
            ["the quick brown fox"]
        );
    }

    #[test]
    fn reflow_joins_lines_except_paragraphs_and_items() {
        assert_eq!(reflow("one\ntwo\nthree\n"), ["one two three"]);
        assert_eq!(reflow("one\ntwo\n\nthree"), ["one two", "", "three"]);
        assert_eq!(
            reflow("Keys:\n- `w` word\n- `b` back\n  more of it\n1. first\n2) second"),
            [
                "Keys:",
                "- `w` word",
                "- `b` back",
                "  more of it",
                "1. first",
                "2) second"
            ]
        );
        assert_eq!(reflow("a\n`b` c\n3 apples"), ["a `b` c 3 apples"]);
        assert_eq!(reflow(""), Vec::<String>::new());
    }

    #[test]
    fn wrap_keeps_empty_lines_and_indentation() {
        assert_eq!(wrapped_text("", 10, 0), [""]);
        assert_eq!(wrapped_text("  a b", 10, 0), ["  a b"]);
        assert_eq!(wrapped_text("a b c d", 3, 2), ["a b", "  c", "  d"]);
    }

    #[test]
    fn wrap_cuts_long_words_and_never_loops() {
        assert_eq!(wrapped_text("abcdefgh", 3, 0), ["abc", "def", "gh"]);
        assert_eq!(
            wrapped_text("ab abcdefgh", 3, 0),
            ["ab", "abc", "def", "gh"]
        );
        // width 0 and wide characters
        assert_eq!(wrapped_text("ab", 0, 0), ["a", "b"]);
        assert_eq!(wrapped_text("世界", 1, 0), ["世", "界"]);
        for width in 0..6 {
            for line in wrapped_text("some `code` and a loooooong word", width, 2) {
                assert!(
                    cells_width(&cells(&line)) <= width.max(1) + 2,
                    "{line:?} {width}"
                );
            }
        }
    }

    #[test]
    fn backticks_make_keys_and_are_not_drawn() {
        let line = inline("press `w` or `<space>f`, not `` ok", Role::Lesson);
        assert_eq!(string(&line), "press w or <space>f, not ` ok");
        let roles: String = line
            .iter()
            .map(|(_, role)| if *role == Role::Key { 'k' } else { '.' })
            .collect();
        assert_eq!(roles, "......k....kkkkkkkk..........");
        // a span is still one when it is broken over lines, and the space in it too
        let lines = wrap(&inline("a `b c d` e", Role::Lesson), 5, 0, Role::Lesson);
        assert_eq!(
            lines.iter().map(|l| string(l)).collect::<Vec<_>>(),
            ["a b c", "d e"]
        );
        assert_eq!(lines[0][3].1, Role::Key);
        assert_eq!(lines[0][2].1, Role::Key);
        // an unclosed one ends with the text
        assert_eq!(string(&inline("a `b", Role::Lesson)), "a b");
    }

    #[test]
    fn clip_cuts_with_an_ellipsis() {
        assert_eq!(string(&clip(cells("abcdef"), 6)), "abcdef");
        assert_eq!(string(&clip(cells("abcdefg"), 6)), "abcde…");
        assert_eq!(string(&clip(cells("世界世界"), 5)), "世界…");
        assert_eq!(string(&clip(cells("abc"), 0)), "…");
    }

    fn exercises() -> Vec<Exercise> {
        let exercise = |id: &str, number: usize, gold: Option<&str>| Exercise {
            title: "Word by word".into(),
            lesson: "`w` selects to the start of the next word.\nSecond line.\n".into(),
            goal: "The quick brown fox\nsecond\n".into(),
            hints: vec!["one `x`".into(), "two".into(), "three".into()],
            gold: gold.map(str::to_owned),
            naive: "llllll".into(),
            chapter_number: 3,
            chapter_title: "Words, lines and counts".into(),
            number,
            chapter_len: 7,
            ..Exercise::for_test(id, "wd")
        };
        vec![
            exercise("motions/01-a", 2, None),
            exercise("motions/02-b", 7, Some("d")),
        ]
    }

    fn attempt(exercise: usize) -> Attempt {
        Attempt {
            exercise,
            doc: Default::default(),
            keys: 3,
            solved_at: None,
            hints: 0,
            best: None,
            commands: Vec::new(),
        }
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|line| string(&line.cells)).collect()
    }

    fn block_text(content: &Content) -> Vec<String> {
        content
            .blocks
            .iter()
            .flat_map(|block| text(&block.lines))
            .collect()
    }

    #[test]
    fn content_while_solving() {
        let mut attempt = attempt(0);
        attempt.hints = 2;
        attempt.best = Some(5);
        let content = content(&exercises(), &attempt, 40, 1);
        assert_eq!(content.title, "3.2 / 7 · Words, lines and counts");
        assert_eq!(
            block_text(&content),
            [
                "Word by word",
                "",
                "Lesson",
                "w selects to the start of the next word.",
                "Second line.",
                "",
                "Goal",
                "▎ The quick brown fox",
                "▎ second",
                "",
                "Hints 2/3",
                "1 one x",
                "2 two",
                "",
                "Keys 3 · par 2 · best 5",
                ":practice hint gives a hint (2/3)",
            ]
        );
        // without the gaps for the band
        let content = super::content(&exercises(), &attempt, 30, 0);
        assert_eq!(
            block_text(&content).iter().filter(|l| l.is_empty()).count(),
            0
        );
        // all hints out: no tip
        attempt.hints = 3;
        let content = super::content(&exercises(), &attempt, 40, 0);
        assert_eq!(
            block_text(&content).last().unwrap(),
            "Keys 3 · par 2 · best 5"
        );
    }

    #[test]
    fn the_goal_is_a_block_that_is_clipped_not_wrapped() {
        let content = content(&exercises(), &attempt(0), 12, 1);
        let goal = &content.blocks[2];
        assert_eq!(text(&goal.lines)[..3], ["Goal", "▎ The quick…", "▎ second"]);
        assert_eq!(goal.lines[1].fill, Some(Role::Goal));
        assert_eq!(goal.lines[1].cells[0], (GOAL_BAR, Role::Bar));
        assert_eq!(goal.lines[1].cells[2].1, Role::Goal);
    }

    #[test]
    fn challenges_say_so() {
        let mut exercises = exercises();
        exercises[0].kind = Kind::Challenge;
        let content = content(&exercises, &attempt(0), 40, 1);
        assert_eq!(
            text(&content.blocks[0].lines)[0],
            "Challenge · Word by word"
        );
    }

    #[test]
    fn content_when_solved() {
        let mut solved = attempt(1);
        solved.solved_at = Some(2);
        solved.best = Some(2);
        let content = content(&exercises(), &solved, 40, 1);
        assert_eq!(content.title, "3.7 / 7 · Words, lines and counts");
        // at par, a gold exists; it is the last exercise
        let lines = block_text(&content);
        assert_eq!(
            lines[lines.len() - 5..],
            [
                "Solved in 2 keys · par 2 · best 2",
                "",
                "Teach  wd",
                "Gold   d",
                "Without the feature: 6 keys",
            ]
        );
        // over par: no gold, and there is a next one
        let mut over = attempt(0);
        over.solved_at = Some(4);
        over.best = Some(4);
        let lines = block_text(&super::content(&exercises(), &over, 40, 1));
        assert_eq!(
            lines[lines.len() - 6..],
            [
                "Solved in 4 keys · par 2 · best 4",
                "",
                "Teach  wd",
                "Without the feature: 6 keys",
                "",
                ":practice next",
            ]
        );
    }

    #[test]
    fn fit_cuts_the_lesson_first_and_keeps_the_count_and_the_goal() {
        let make = |width| content(&exercises(), &attempt(0), width, 0);
        let all = text(&fit(make(60), 100, 60, false));
        let rows = |height, width| text(&fit(make(width), height, width, false));
        assert_eq!(
            all,
            [
                "Word by word",
                "Lesson",
                "w selects to the start of the next word. Second line.",
                "Goal",
                "▎ The quick brown fox",
                "▎ second",
                "Keys 3 · par 2",
                ":practice hint gives a hint (0/3)",
            ]
        );
        // no room for the tip
        assert_eq!(rows(7, 60)[..], all[..7]);
        // a lesson is shown with at least a line, or not at all
        assert_eq!(
            rows(6, 60),
            [
                "Word by word",
                "Goal",
                "▎ The quick brown fox",
                "▎ second",
                "Keys 3 · par 2",
                all[7].as_str()
            ]
        );
        // room for some of a longer lesson, which says that there is more
        assert_eq!(
            rows(8, 24)[..4],
            [
                "Word by word",
                "Lesson",
                "w selects to the start",
                "of the next word. Secon…",
            ]
        );
        // then the header goes, then the goal, the count stays
        assert_eq!(
            rows(4, 50),
            [
                "Goal",
                "▎ The quick brown fox",
                "▎ second",
                "Keys 3 · par 2"
            ]
        );
        assert_eq!(
            rows(3, 50),
            ["Goal", "▎ The quick brown fox", "Keys 3 · par 2"]
        );
        assert_eq!(rows(2, 50), ["Goal", "Keys 3 · par 2"]);
        assert_eq!(rows(1, 50), ["Keys 3 · par 2"]);
        assert_eq!(rows(0, 50), Vec::<String>::new());
    }

    #[test]
    fn fit_pads_when_there_is_room_for_both_sides() {
        let make = || content(&exercises(), &attempt(0), 60, 0);
        let padded = text(&fit(make(), 10, 60, true));
        assert_eq!(padded.len(), 10);
        assert!(padded[0].is_empty() && padded[9].is_empty());
        assert_eq!(text(&fit(make(), 9, 60, true)).len(), 8);
        assert_eq!(text(&fit(make(), 0, 60, true)).len(), 0);
    }

    fn theme(toml: &str) -> Theme {
        Theme::from(toml::from_str::<toml::Value>(toml).unwrap())
    }

    #[test]
    fn styles_follow_the_scopes_and_fall_back() {
        use helix_view::graphics::Color;
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        let fg = |style: Style| style.fg;

        // nothing defined: plain, but the parts stay distinct
        let styles = Styles::new(&theme(""));
        assert_eq!(styles.get(Role::Lesson), Style::default());
        assert!(styles
            .get(Role::Label)
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(styles.get(Role::Key).add_modifier.contains(Modifier::BOLD));
        assert!(styles.get(Role::Dim).add_modifier.contains(Modifier::DIM));

        // the fallbacks, the panel from the popup
        let styles = Styles::new(&theme(
            r##"
"ui.popup" = { bg = "#101010" }
"ui.text" = { fg = "#aaaaaa" }
"markup.raw" = { fg = "#00ff00" }
"markup.raw.inline" = { fg = "#00ffff" }
"markup.heading" = { fg = "#ff00ff" }
"hint" = { fg = "#ffff00" }
"comment" = { fg = "#555555" }
"diff.plus" = { fg = "#0000ff" }
"##,
        ));
        assert_eq!(styles.get(Role::Lesson).bg, Some(rgb(16, 16, 16)));
        assert_eq!(fg(styles.get(Role::Lesson)), Some(rgb(0xaa, 0xaa, 0xaa)));
        assert_eq!(fg(styles.get(Role::Goal)), Some(rgb(0, 255, 0)));
        assert_eq!(fg(styles.get(Role::Key)), Some(rgb(0, 255, 255)));
        assert_eq!(fg(styles.get(Role::Label)), Some(rgb(255, 0, 255)));
        assert_eq!(fg(styles.get(Role::Hint)), Some(rgb(255, 255, 0)));
        assert_eq!(fg(styles.get(Role::Dim)), Some(rgb(0x55, 0x55, 0x55)));
        assert_eq!(fg(styles.get(Role::Solved)), Some(rgb(0, 0, 255)));
        // the bar is dim on the background of the goal
        assert_eq!(fg(styles.get(Role::Bar)), Some(rgb(0x55, 0x55, 0x55)));
        assert_eq!(styles.get(Role::Bar).bg, Some(rgb(16, 16, 16)));

        // the scopes of the panel win
        let styles = Styles::new(&theme(
            r##"
"ui.popup" = { bg = "#101010" }
"markup.raw" = { fg = "#00ff00" }
"ui.practice" = { bg = "#202020" }
"ui.practice.goal" = { fg = "#123456", bg = "#654321" }
"##,
        ));
        assert_eq!(styles.get(Role::Lesson).bg, Some(rgb(0x20, 0x20, 0x20)));
        assert_eq!(styles.get(Role::Goal).fg, Some(rgb(0x12, 0x34, 0x56)));
        assert_eq!(styles.get(Role::Goal).bg, Some(rgb(0x65, 0x43, 0x21)));
    }

    #[test]
    fn areas() {
        // wide: a column at the top right, as wide as the goal needs, 45% at most
        assert_eq!(panel_width(Rect::new(0, 1, 100, 29), 50), 45);
        assert_eq!(panel_width(Rect::new(0, 0, 160, 29), 50), 56);
        assert_eq!(panel_width(Rect::new(0, 0, 160, 29), 10), 56);
        assert_eq!(panel_width(Rect::new(0, 0, 200, 29), 70), 76);
        assert_eq!(panel_width(Rect::new(0, 0, 200, 29), 200), 90);
        assert_eq!(panel_width(Rect::new(0, 0, 50, 29), 20), 50);
        let area = panel_area(Rect::new(0, 1, 100, 29), 45, 3, 10);
        assert_eq!(area, Rect::new(55, 1, 45, 12));
        let area = panel_area(Rect::new(0, 0, 200, 50), 56, 3, 10);
        assert_eq!(area, Rect::new(144, 0, 56, 12));
        // the column leaves the row of the statusline
        assert_eq!(panel_area(Rect::new(0, 0, 100, 10), 45, 3, 50).height, 9);
        // narrow: the bottom, above the statusline, below the lines of the buffer
        let area = panel_area(Rect::new(0, 0, 50, 19), 50, 3, 30);
        assert_eq!(area, Rect::new(0, 3, 50, 15));
        let area = panel_area(Rect::new(0, 0, 50, 19), 50, 3, 4);
        assert_eq!(area, Rect::new(0, 12, 50, 6));
        // and at least half the height when the buffer is long
        let area = panel_area(Rect::new(0, 0, 50, 19), 50, 12, 30);
        assert_eq!(area, Rect::new(0, 9, 50, 9));
        // tiny sizes don't overflow
        for width in 0..8 {
            for height in 0..8 {
                let area = Rect::new(0, 0, width, height);
                let _ = panel_area(area, panel_width(area, 50), u16::MAX, 100);
                let area = Rect::new(3, 2, width, height);
                let _ = panel_area(area, panel_width(area, 0), 0, 0);
            }
        }
    }
}
