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
    /// The title of the exercise.
    heading: Line,
    /// The lesson, the first to be cut off when there is no room: the goal is needed to solve the
    /// exercise, and the hints were asked for.
    lesson: Vec<Line>,
    /// The goal and the hints that were revealed.
    task: Vec<Line>,
    /// The count or the result, the last to be cut off.
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

fn content(exercises: &[Exercise], attempt: &Attempt, width: usize) -> Content {
    let exercise = &exercises[attempt.exercise];
    let title = format!(
        "practice · {}.{} / {} · {}",
        exercise.chapter_number, exercise.number, exercise.chapter_len, exercise.chapter_title
    );

    let mut lesson: Vec<Line> = Vec::new();
    for line in reflow(&exercise.lesson) {
        lesson.extend(wrap(&line, width, 0).into_iter().map(|line| (line, false)));
    }
    let mut task: Vec<Line> = vec![("Goal:".to_owned(), true)];
    for line in exercise.goal.lines() {
        // not wrapped, it is text to compare with
        task.push((format!("  {line}"), false));
    }
    for (n, hint) in exercise.hints.iter().take(attempt.hints).enumerate() {
        let text = format!("Hint {}: {hint}", n + 1);
        task.extend(wrap(&text, width, 2).into_iter().map(|line| (line, false)));
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
        heading: (exercise.title.clone(), true),
        lesson,
        task,
        footer,
    }
}

/// The lines that fit in `height` rows of `width` columns, in the order of the panel. When
/// there is not room for all, the lesson is the first to go (cut off with a `…`), then the title,
/// then the rest of the footer, then the hints and the goal, last the count or the result.
fn fit(content: Content, height: usize, width: usize) -> Vec<Line> {
    let mut room = height;
    let mut take = |wanted: usize| {
        let taken = wanted.min(room);
        room -= taken;
        taken
    };
    let footer_first = take(content.footer.len().min(1));
    let task = take(content.task.len());
    let footer = footer_first + take(content.footer.len() - footer_first);
    let heading = take(1);
    let lesson = take(content.lesson.len());

    let mut lines: Vec<Line> = Some(content.heading).into_iter().take(heading).collect();
    let cut = lesson < content.lesson.len();
    lines.extend(content.lesson.into_iter().take(lesson));
    if cut && lesson > 0 {
        let (last, _) = lines.last_mut().unwrap();
        while last.width() >= width.max(1) {
            last.pop();
        }
        last.push('…');
    }
    lines.extend(content.task.into_iter().take(task));
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

/// The area of the panel in `editor_area`, for `lines` lines of text.
fn panel_area(editor_area: Rect, text_rows: u16, lines: usize) -> Rect {
    let width = panel_width(editor_area);
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
    let width = panel_width(editor_area);
    let content = content(&exercises, &attempt, usize::from(width.saturating_sub(4)));
    let title = content.title.clone();
    // the lines of the buffer, without the empty one after the final line ending
    let text_rows = u16::try_from(doc.text().len_lines().saturating_sub(1)).unwrap_or(u16::MAX);
    let rows = usize::from(max_height(editor_area, text_rows).saturating_sub(2));
    let lines = fit(content, rows, usize::from(width.saturating_sub(4)));
    let area = panel_area(editor_area, text_rows, lines.len());
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
        let exercise = |id: &str, number: usize, gold: Option<&str>| Exercise {
            title: "Word by word".into(),
            lesson: "`w` selects to the start of the next word.\nSecond line.\n".into(),
            goal: "The quick brown fox\n".into(),
            hints: vec!["one".into(), "two".into(), "three".into()],
            gold: gold.map(str::to_owned),
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

    fn text(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|(line, _)| line.as_str()).collect()
    }

    #[test]
    fn content_while_solving() {
        let mut attempt = attempt(0);
        attempt.hints = 2;
        attempt.best = Some(5);
        let content = content(&exercises(), &attempt, 30);
        assert_eq!(
            content.title,
            "practice · 3.2 / 7 · Words, lines and counts"
        );
        assert_eq!(
            text_owned(fit(content, 100, 30)).join("\n"),
            [
                "Word by word",
                "`w` selects to the start of",
                "the next word. Second line.",
                "Goal:",
                "  The quick brown fox",
                "Hint 1: one",
                "Hint 2: two",
                "Keys 3 · par 2 · best 5"
            ]
            .join("\n")
        );
    }

    #[test]
    fn content_when_solved() {
        let mut solved = attempt(1);
        solved.solved_at = Some(2);
        solved.best = Some(2);
        let content = content(&exercises(), &solved, 40);
        assert_eq!(
            content.title,
            "practice · 3.7 / 7 · Words, lines and counts"
        );
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
    fn fit_cuts_the_lesson_first_and_keeps_the_count_and_the_goal() {
        let make = || content(&exercises(), &attempt(0), 50);
        let all = text_owned(fit(make(), 100, 50));
        assert_eq!(all.len(), 6);
        assert_eq!(all.last().unwrap(), "Keys 3 · par 2");
        // no room for the lesson
        assert_eq!(
            text_owned(fit(make(), 4, 50)),
            [
                "Word by word",
                "Goal:",
                "  The quick brown fox",
                "Keys 3 · par 2"
            ]
        );
        // room for a line of it, which says that there is more
        assert_eq!(
            text_owned(fit(make(), 5, 50)),
            [
                "Word by word",
                "`w` selects to the start of the next word. Second…",
                "Goal:",
                "  The quick brown fox",
                "Keys 3 · par 2"
            ]
        );
        assert_eq!(text_owned(fit(make(), 5, 20))[1], "`w` selects to the …");
        // then the title goes, then the goal, the count stays
        assert_eq!(
            text_owned(fit(make(), 3, 50)),
            ["Goal:", "  The quick brown fox", "Keys 3 · par 2"]
        );
        assert_eq!(text_owned(fit(make(), 2, 50)), ["Goal:", "Keys 3 · par 2"]);
        assert_eq!(text_owned(fit(make(), 1, 50)), ["Keys 3 · par 2"]);
        assert_eq!(text_owned(fit(make(), 0, 50)), Vec::<String>::new());
    }

    fn text_owned(lines: Vec<Line>) -> Vec<String> {
        lines.into_iter().map(|(line, _)| line).collect()
    }

    #[test]
    fn areas() {
        // wide: a column at the top right, 45% capped at 60
        let area = panel_area(Rect::new(0, 1, 100, 29), 3, 10);
        assert_eq!(area, Rect::new(55, 1, 45, 12));
        let area = panel_area(Rect::new(0, 0, 200, 50), 3, 10);
        assert_eq!(area, Rect::new(140, 0, 60, 12));
        // the column leaves the row of the statusline
        assert_eq!(panel_area(Rect::new(0, 0, 100, 10), 3, 50).height, 9);
        // narrow: the bottom, above the statusline, below the lines of the buffer
        let area = panel_area(Rect::new(0, 0, 50, 19), 3, 30);
        assert_eq!(area, Rect::new(0, 3, 50, 15));
        let area = panel_area(Rect::new(0, 0, 50, 19), 3, 4);
        assert_eq!(area, Rect::new(0, 12, 50, 6));
        // and at least half the height when the buffer is long
        let area = panel_area(Rect::new(0, 0, 50, 19), 12, 30);
        assert_eq!(area, Rect::new(0, 9, 50, 9));
        // tiny sizes don't overflow
        for width in 0..8 {
            for height in 0..8 {
                let _ = panel_area(Rect::new(0, 0, width, height), u16::MAX, 100);
                let _ = panel_area(Rect::new(3, 2, width, height), 0, 0);
            }
        }
    }
}
