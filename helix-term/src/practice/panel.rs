//! The panel with the lesson, drawn over the editor while an exercise is open.
//!
//! On a wide terminal it is a column at the top right, on a narrow one (a phone) a band over the
//! bottom of the editor, above the statusline. It only draws, it takes no events: `:practice
//! lesson` shows and hides it.

use helix_core::unicode::width::{UnicodeWidthChar, UnicodeWidthStr};
use helix_view::graphics::{Margin, Modifier, Rect, Style};
use helix_view::Editor;
use tui::buffer::Buffer as Surface;
use tui::widgets::{Block, Widget};

use super::command::{topic_of, topics};
use super::exercise::Exercise;
use super::session::{self, Attempt};

/// Editor areas at least this wide get a column on the right instead of a band at the bottom.
pub const WIDE_WIDTH: u16 = 100;
/// The widest the column gets.
const MAX_COLUMN_WIDTH: u16 = 60;

/// A line of the panel and whether it is emphasized.
type Line = (String, bool);

/// What the panel shows, for lines that are `width` columns wide.
struct Content {
    title: String,
    /// The heading, lesson, goal and hints, which are cut off when there is no room.
    body: Vec<Line>,
    /// The count or the result, always shown first.
    footer: Vec<Line>,
}

/// Breaks `text` into lines of at most `width` columns, at spaces, or inside words that are
/// longer than a line. The leading whitespace of the text is kept, and continuation lines are
/// indented by `hanging` more columns. Empty text is one empty line.
fn wrap(text: &str, width: usize, hanging: usize) -> Vec<String> {
    let width = width.max(1);
    let indent: String = text.chars().take_while(|c| c.is_whitespace()).collect();
    let continuation = " ".repeat(indent.width() + hanging);
    let mut lines = Vec::new();
    let mut line = indent.clone();
    // whether `line` has a word, not only the indentation
    let mut has_word = false;

    for word in text.split_whitespace() {
        let mut word = word;
        loop {
            let separator = usize::from(has_word);
            if line.width() + separator + word.width() <= width {
                if has_word {
                    line.push(' ');
                }
                line.push_str(word);
                has_word = true;
                break;
            }
            if has_word {
                lines.push(std::mem::replace(&mut line, continuation.clone()));
                has_word = false;
                continue;
            }
            // a word that doesn't fit a line of its own: cut it
            let room = width.saturating_sub(line.width()).max(1);
            let mut taken = 0;
            let mut cut = 0;
            for (at, c) in word.char_indices() {
                let w = c.width().unwrap_or(0);
                if taken + w > room && cut > 0 {
                    break;
                }
                taken += w;
                cut = at + c.len_utf8();
            }
            line.push_str(&word[..cut]);
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

fn content(exercises: &[Exercise], attempt: &Attempt, width: usize) -> Content {
    let exercise = &exercises[attempt.exercise];
    let topic = topic_of(exercise);
    let in_topic: Vec<_> = exercises
        .iter()
        .filter(|other| topic_of(other) == topic)
        .collect();
    let position = in_topic
        .iter()
        .position(|other| other.id == exercise.id)
        .map_or(1, |position| position + 1);
    debug_assert!(topics(exercises).contains(&topic));
    let title = format!("practice · {topic} {position}/{}", in_topic.len());

    let mut body: Vec<Line> = vec![(exercise.title.clone(), true)];
    for line in exercise.lesson.trim_end().lines() {
        body.extend(wrap(line, width, 0).into_iter().map(|line| (line, false)));
    }
    body.push(("Goal:".to_owned(), true));
    for line in exercise.goal.lines() {
        // not wrapped, it is text to compare with
        body.push((format!("  {line}"), false));
    }
    for (n, hint) in exercise.hints.iter().take(attempt.hints).enumerate() {
        let text = format!("Hint {}: {hint}", n + 1);
        body.extend(wrap(&text, width, 2).into_iter().map(|line| (line, false)));
    }

    let par = exercise.par();
    let mut footer: Vec<Line> = Vec::new();
    match attempt.solved_at {
        None => {
            let mut line = format!("Keys {} · par {par}", attempt.keys);
            if let Some(best) = attempt.best {
                line.push_str(&format!(" · best {best}"));
            }
            footer.push((line, true));
        }
        Some(keys) => {
            let best = attempt.best.unwrap_or(keys);
            let unit = if keys == 1 { "key" } else { "keys" };
            footer.push((
                format!("Solved in {keys} {unit} · par {par} · best {best}"),
                true,
            ));
            let teach = format!("Teach: {}", exercise.teach);
            footer.extend(wrap(&teach, width, 2).into_iter().map(|line| (line, false)));
            if let Some(gold) = exercise.gold.as_ref().filter(|_| best <= par) {
                let gold = format!("Gold: {gold}");
                footer.extend(wrap(&gold, width, 2).into_iter().map(|line| (line, false)));
            }
            if attempt.exercise + 1 < exercises.len() {
                footer.push((":practice next".to_owned(), false));
            }
        }
    }
    Content {
        title,
        body,
        footer,
    }
}

/// The lines that fit in `height` rows: the footer, and as much of the body as there is room for
/// before it.
fn fit(content: Content, height: usize) -> Vec<Line> {
    let footer = content.footer.len().min(height);
    let mut lines: Vec<Line> = content.body.into_iter().take(height - footer).collect();
    lines.extend(content.footer.into_iter().take(footer));
    lines
}

fn is_wide(editor_area: Rect) -> bool {
    editor_area.width >= WIDE_WIDTH
}

fn panel_width(editor_area: Rect) -> u16 {
    if is_wide(editor_area) {
        (editor_area.width * 45 / 100).min(MAX_COLUMN_WIDTH)
    } else {
        editor_area.width
    }
}

/// The most rows the panel gets, border included. The column leaves the row of the statusline of
/// the view free, the band is at most half of the area.
fn max_height(editor_area: Rect) -> u16 {
    if is_wide(editor_area) {
        editor_area.height.saturating_sub(1)
    } else {
        editor_area.height / 2
    }
}

/// The area of the panel in `editor_area`, for `lines` lines of text.
fn panel_area(editor_area: Rect, lines: usize) -> Rect {
    let width = panel_width(editor_area);
    let height = u16::try_from(lines + 2)
        .unwrap_or(u16::MAX)
        .min(max_height(editor_area));
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
    let (view, _) = current_ref!(editor);
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
    let width = panel_width(editor_area);
    let content = content(&exercises, &attempt, usize::from(width.saturating_sub(4)));
    let title = content.title.clone();
    let rows = usize::from(max_height(editor_area).saturating_sub(2));
    let lines = fit(content, rows);
    let area = panel_area(editor_area, lines.len());
    if area.width < 5 || area.height < 3 {
        return;
    }

    let text_style = editor.theme.get("ui.text.info");
    let popup_style = editor.theme.get("ui.popup.info");
    surface.clear_with(area, popup_style);
    let block = Block::bordered().title(title).border_style(popup_style);
    let inner = block.inner(area).inner(Margin::horizontal(1));
    block.render(area, surface);

    for (row, (line, emphasized)) in lines.iter().enumerate().take(usize::from(inner.height)) {
        let style: Style = if *emphasized {
            text_style.add_modifier(Modifier::BOLD)
        } else {
            text_style
        };
        surface.set_stringn(
            inner.x,
            inner.y + row as u16,
            line,
            usize::from(inner.width),
            style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces() {
        assert_eq!(
            wrap("the quick brown fox", 9, 0),
            ["the quick", "brown fox"]
        );
        assert_eq!(
            wrap("the quick brown fox", 10, 0),
            ["the quick", "brown fox"]
        );
        assert_eq!(wrap("the quick brown fox", 100, 0), ["the quick brown fox"]);
    }

    #[test]
    fn wrap_keeps_empty_lines_and_indentation() {
        assert_eq!(wrap("", 10, 0), [""]);
        assert_eq!(wrap("  a b", 10, 0), ["  a b"]);
        assert_eq!(wrap("a b c d", 3, 2), ["a b", "  c", "  d"]);
    }

    #[test]
    fn wrap_cuts_long_words_and_never_loops() {
        assert_eq!(wrap("abcdefgh", 3, 0), ["abc", "def", "gh"]);
        assert_eq!(wrap("ab abcdefgh", 3, 0), ["ab", "abc", "def", "gh"]);
        // width 0 and wide characters
        assert_eq!(wrap("ab", 0, 0), ["a", "b"]);
        assert_eq!(wrap("世界", 1, 0), ["世", "界"]);
        for width in 0..6 {
            for line in wrap("some `code` and a loooooong word", width, 2) {
                assert!(line.width() <= width.max(1) + 2, "{line:?} {width}");
            }
        }
    }

    fn exercises() -> Vec<Exercise> {
        let exercise = |id: &str, gold: Option<&str>| Exercise {
            id: id.into(),
            title: "Word by word".into(),
            topic: "motions".into(),
            feature: String::new(),
            lesson: "`w` selects to the start of the next word.\nSecond line.\n".into(),
            start: String::new(),
            goal: "The quick brown fox\n".into(),
            language: None,
            hints: vec!["one".into(), "two".into(), "three".into()],
            teach: "wd".into(),
            gold: gold.map(str::to_owned),
        };
        vec![
            exercise("motions/01-a", None),
            exercise("motions/02-b", Some("d")),
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
        }
    }

    fn text(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|(line, _)| line.as_str()).collect()
    }

    #[test]
    fn content_while_solving() {
        let mut attempt = attempt(0);
        attempt.hints = 2;
        attempt.best = Some(5);
        let content = content(&exercises(), &attempt, 30);
        assert_eq!(content.title, "practice · motions 1/2");
        assert_eq!(
            text(&content.body),
            [
                "Word by word",
                "`w` selects to the start of",
                "the next word.",
                "Second line.",
                "Goal:",
                "  The quick brown fox",
                "Hint 1: one",
                "Hint 2: two"
            ]
        );
        assert_eq!(text(&content.footer), ["Keys 3 · par 2 · best 5"]);
    }

    #[test]
    fn content_when_solved() {
        let mut solved = attempt(1);
        solved.solved_at = Some(2);
        solved.best = Some(2);
        let content = content(&exercises(), &solved, 40);
        assert_eq!(content.title, "practice · motions 2/2");
        // at par, a gold exists; it is the last exercise
        assert_eq!(
            text(&content.footer),
            ["Solved in 2 keys · par 2 · best 2", "Teach: wd", "Gold: d"]
        );
        // over par: no gold, and there is a next one
        let mut over = attempt(0);
        over.solved_at = Some(4);
        over.best = Some(4);
        assert_eq!(
            text(&super::content(&exercises(), &over, 40).footer),
            [
                "Solved in 4 keys · par 2 · best 4",
                "Teach: wd",
                ":practice next"
            ]
        );
    }

    #[test]
    fn fit_keeps_the_footer() {
        let make = || content(&exercises(), &attempt(0), 50);
        let all = text_owned(fit(make(), 100));
        assert_eq!(all.last().unwrap(), "Keys 3 · par 2");
        let few = text_owned(fit(make(), 3));
        assert_eq!(
            few,
            [
                "Word by word",
                "`w` selects to the start of the next word.",
                "Keys 3 · par 2"
            ]
        );
        assert_eq!(text_owned(fit(make(), 0)), Vec::<String>::new());
    }

    fn text_owned(lines: Vec<Line>) -> Vec<String> {
        lines.into_iter().map(|(line, _)| line).collect()
    }

    #[test]
    fn areas() {
        // wide: a column at the top right, 45% capped at 60
        let area = panel_area(Rect::new(0, 1, 100, 29), 10);
        assert_eq!(area, Rect::new(55, 1, 45, 12));
        let area = panel_area(Rect::new(0, 0, 200, 50), 10);
        assert_eq!(area, Rect::new(140, 0, 60, 12));
        // the column leaves the row of the statusline
        assert_eq!(panel_area(Rect::new(0, 0, 100, 10), 50).height, 9);
        // narrow: the bottom, above the statusline, at most half the height
        let area = panel_area(Rect::new(0, 0, 50, 19), 30);
        assert_eq!(area, Rect::new(0, 9, 50, 9));
        let area = panel_area(Rect::new(0, 0, 50, 19), 4);
        assert_eq!(area, Rect::new(0, 12, 50, 6));
        // tiny sizes don't overflow
        for width in 0..8 {
            for height in 0..8 {
                let _ = panel_area(Rect::new(0, 0, width, height), 100);
                let _ = panel_area(Rect::new(3, 2, width, height), 0);
            }
        }
    }
}
