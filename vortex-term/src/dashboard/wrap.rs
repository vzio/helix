//! Long values on two rows: a value that doesn't fit its column goes on to a second row, and
//! one that doesn't fit two ends with `…`.
//!
//! Rows break where a reader expects it: between words, and in a URL also after a `/` (or the
//! `:` of an scp-style `user@host:path`). A word too long for a row breaks where the row ends.

use vortex_core::unicode::width::UnicodeWidthChar;
use vortex_view::graphics::Style;

/// A piece of text in one style.
pub type Span = (String, Style);

/// Where a value may break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breaks {
    /// Between words.
    Words,
    /// Between words, and after the `/`s of a URL (or the `:` of an scp-style one).
    Url,
}

/// The characters of some spans, each with its style and width.
fn chars(spans: &[Span]) -> Vec<(char, Style, usize)> {
    spans
        .iter()
        .flat_map(|(text, style)| {
            text.chars()
                .map(move |c| (c, *style, c.width().unwrap_or(0)))
        })
        .filter(|(_, _, width)| *width > 0)
        .collect()
}

/// The indexes a row may start at.
fn starts(chars: &[(char, Style, usize)], breaks: Breaks) -> Vec<usize> {
    let text: String = chars.iter().map(|(c, _, _)| *c).collect();
    let scp = !text.contains("://");
    let mut starts = Vec::new();
    for i in 1..chars.len() {
        let (previous, c) = (chars[i - 1].0, chars[i].0);
        let after_space = previous == ' ' && c != ' ';
        let after_url_break = breaks == Breaks::Url
            && c != ' '
            && ((previous == '/' && c != '/') || (previous == ':' && scp && c != '/'));
        if after_space || after_url_break {
            starts.push(i);
        }
    }
    starts
}

fn width(chars: &[(char, Style, usize)]) -> usize {
    chars.iter().map(|(_, _, width)| width).sum()
}

/// `chars` as spans, a span per run of one style.
fn spans(chars: &[(char, Style, usize)]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for (c, style, _) in chars {
        match spans.last_mut() {
            Some((text, last)) if last == style => text.push(*c),
            _ => spans.push((c.to_string(), *style)),
        }
    }
    spans
}

/// The rows `spans` take in `width` columns, at most `max_rows` (at least one): broken where
/// `breaks` allows, the last one cut with `…` when the rest doesn't fit.
pub fn wrap(spans: &[Span], width: usize, max_rows: usize, breaks: Breaks) -> Vec<Vec<Span>> {
    let chars = chars(spans);
    let starts = starts(&chars, breaks);
    let max_rows = max_rows.max(1);
    let trimmed = |range: &[(char, Style, usize)]| -> usize {
        let end = range
            .iter()
            .rposition(|(c, _, _)| *c != ' ')
            .map_or(0, |i| i + 1);
        end
    };
    let mut rows = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let rest = &chars[start..];
        let last = rows.len() + 1 == max_rows;
        if self::width(&rest[..trimmed(rest)]) <= width {
            rows.push(self::spans(&rest[..trimmed(rest)]));
            break;
        }
        if last || width == 0 {
            // the last row: as much as fits, and `…`
            let mut end = start;
            let mut used = 0;
            while end < chars.len() && used + chars[end].2 < width {
                used += chars[end].2;
                end += 1;
            }
            let shown = &chars[start..end];
            let mut row = self::spans(&shown[..trimmed(shown)]);
            let style = chars[end.min(chars.len() - 1)].1;
            row.push(("…".to_string(), style));
            if width > 0 {
                rows.push(row);
            }
            break;
        }
        // the farthest break that fits, else the most that fits
        let fits =
            |end: usize| self::width(&chars[start..end][..trimmed(&chars[start..end])]) <= width;
        let end = starts
            .iter()
            .copied()
            .filter(|&end| end > start)
            .take_while(|&end| fits(end))
            .last()
            .unwrap_or_else(|| {
                let mut end = start;
                let mut used = 0;
                while end < chars.len() && used + chars[end].2 <= width {
                    used += chars[end].2;
                    end += 1;
                }
                end.max(start + 1)
            });
        let row = &chars[start..end];
        rows.push(self::spans(&row[..trimmed(row)]));
        start = end;
        while start < chars.len() && chars[start].0 == ' ' {
            start += 1;
        }
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

/// The rows of a value typed in up to `columns` columns, in all its rows together.
pub fn typed(rows: &[Vec<Span>], columns: usize) -> Vec<Vec<Span>> {
    let mut left = columns;
    rows.iter()
        .map(|row| {
            let mut out = Vec::new();
            for (text, style) in row {
                let mut piece = String::new();
                for c in text.chars() {
                    let w = c.width().unwrap_or(0);
                    if w > left {
                        left = 0;
                        break;
                    }
                    left -= w;
                    piece.push(c);
                }
                if !piece.is_empty() {
                    out.push((piece, *style));
                }
            }
            out
        })
        .collect()
}

/// The columns of all the rows together.
pub fn columns(rows: &[Vec<Span>]) -> usize {
    rows.iter()
        .flatten()
        .map(|(text, _)| text.chars().filter_map(|c| c.width()).sum::<usize>())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vortex_core::unicode::width::UnicodeWidthStr;
    use vortex_view::graphics::Color;

    fn plain(text: &str) -> Vec<Span> {
        vec![(text.to_string(), Style::default())]
    }

    fn text(rows: &[Vec<Span>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.iter().map(|(text, _)| text.as_str()).collect())
            .collect()
    }

    #[test]
    fn a_value_that_fits_is_one_row() {
        assert_eq!(text(&wrap(&plain("main"), 10, 2, Breaks::Words)), ["main"]);
        assert_eq!(text(&wrap(&plain(""), 10, 2, Breaks::Words)), [""]);
    }

    #[test]
    fn words_break_between_words() {
        let summary = plain("3768ea4  feat: remove microslop support and fix CI (#27)");
        assert_eq!(
            text(&wrap(&summary, 38, 2, Breaks::Words)),
            [
                "3768ea4  feat: remove microslop",
                "support and fix CI (#27)"
            ]
        );
    }

    #[test]
    fn urls_break_after_a_slash() {
        let remote = plain("origin  https://github.com/some-organization/a-long-repository-name");
        assert_eq!(
            text(&wrap(&remote, 38, 2, Breaks::Url)),
            [
                "origin  https://github.com/",
                "some-organization/a-long-repository-n…"
            ]
        );
        let scp = plain("origin  git@github.com:vzio/vortex.git");
        assert_eq!(
            text(&wrap(&scp, 30, 2, Breaks::Url)),
            ["origin  git@github.com:vzio/", "vortex.git"]
        );
    }

    #[test]
    fn two_rows_at_most() {
        let long = plain("one two three four five six seven eight nine ten");
        assert_eq!(
            text(&wrap(&long, 14, 2, Breaks::Words)),
            ["one two three", "four five six…"]
        );
        assert_eq!(text(&wrap(&long, 14, 1, Breaks::Words)), ["one two three…"]);
    }

    #[test]
    fn a_word_too_long_for_a_row_breaks_where_the_row_ends() {
        assert_eq!(
            text(&wrap(&plain("abcdefghijklmnop"), 6, 2, Breaks::Words)),
            ["abcdef", "ghijk…"]
        );
    }

    #[test]
    fn typing_in_runs_through_the_rows() {
        let rows = wrap(&plain("alpha beta gamma"), 10, 2, Breaks::Words);
        assert_eq!(text(&rows), ["alpha beta", "gamma"]);
        assert_eq!(text(&typed(&rows, 7)), ["alpha b", ""]);
        assert_eq!(text(&typed(&rows, 12)), ["alpha beta", "ga"]);
        assert_eq!(columns(&rows), 15);
    }

    fn width_of(row: &[Span]) -> usize {
        row.iter().map(|(text, _)| text.width()).sum()
    }

    #[test]
    fn spaces_between_words_are_not_kept_at_a_break() {
        let rows = wrap(&plain("alpha    beta    gamma"), 9, 3, Breaks::Words);
        assert_eq!(text(&rows), ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn a_url_never_breaks_after_the_colon_of_its_scheme() {
        let url = plain("see https://example.org/some/long/path/to/a/file");
        // (a row narrower than `https://` breaks it hard, wherever the row ends)
        for width in 8..40 {
            for row in text(&wrap(&url, width, 4, Breaks::Url)) {
                assert!(!row.ends_with("https:"), "{width}: {row:?}");
            }
        }
        // with a scheme, only the slashes break it
        assert_eq!(
            text(&wrap(&plain("https://example.org/a/b"), 16, 2, Breaks::Url)),
            ["https://", "example.org/a/b"]
        );
    }

    #[test]
    fn an_scp_style_url_breaks_after_the_colon_and_the_slashes() {
        let scp = plain("git@example.org:team/project/repo.git");
        assert_eq!(
            text(&wrap(&scp, 16, 3, Breaks::Url)),
            ["git@example.org:", "team/project/", "repo.git"]
        );
        // words don't break there
        assert_eq!(
            text(&wrap(&scp, 20, 3, Breaks::Words)),
            ["git@example.org:team", "/project/repo.git"]
        );
    }

    #[test]
    fn never_more_rows_than_asked() {
        let long = plain("one two three four five six seven eight nine ten eleven twelve");
        for max_rows in 1..5 {
            let rows = wrap(&long, 12, max_rows, Breaks::Words);
            assert_eq!(rows.len(), max_rows);
            assert!(text(&rows).last().unwrap().ends_with('…'));
        }
        // zero rows still gives one
        assert_eq!(wrap(&long, 12, 0, Breaks::Words).len(), 1);
    }

    #[test]
    fn the_last_row_ends_with_an_ellipsis_only_when_the_rest_does_not_fit() {
        let rows = text(&wrap(&plain("alpha beta gamma"), 10, 2, Breaks::Words));
        assert_eq!(rows, ["alpha beta", "gamma"]);
        let rows = text(&wrap(&plain("alpha beta gamma delta"), 10, 2, Breaks::Words));
        assert_eq!(rows, ["alpha beta", "gamma del…"]);
        // exactly full is not cut
        let rows = text(&wrap(&plain("alpha beta gamma"), 10, 2, Breaks::Words));
        assert!(!rows.iter().any(|row| row.contains('…')));
    }

    #[test]
    fn a_word_longer_than_the_width_breaks_hard_on_every_row() {
        assert_eq!(
            text(&wrap(&plain("abcdefghijklmnop"), 5, 4, Breaks::Words)),
            ["abcde", "fghij", "klmno", "p"]
        );
        assert_eq!(
            text(&wrap(&plain("ab abcdefghijklmnop"), 5, 2, Breaks::Words)),
            ["ab", "abcd…"]
        );
    }

    #[test]
    fn styles_stay_with_their_text_across_a_break() {
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let spans = vec![
            ("origin  ".to_string(), red),
            ("https://example.org/path".to_string(), blue),
        ];
        let rows = wrap(&spans, 18, 2, Breaks::Url);
        assert_eq!(text(&rows), ["origin  https://", "example.org/path"]);
        assert_eq!(
            rows[0],
            [("origin  ".to_string(), red), ("https://".to_string(), blue)]
        );
        assert_eq!(rows[1], [("example.org/path".to_string(), blue)]);
        // a break in the middle of a styled run splits it in two spans of the same style
        let rows = wrap(&spans, 10, 3, Breaks::Words);
        for row in &rows {
            for (piece, style) in row {
                let expected = if "origin".contains(piece.as_str()) {
                    red
                } else {
                    blue
                };
                assert_eq!(*style, expected, "{piece:?}");
            }
        }
    }

    #[test]
    fn the_ellipsis_takes_the_style_of_the_text_it_cuts() {
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let spans = vec![("aaaa bbbb ".to_string(), red), ("cccc dddd".to_string(), blue)];
        let rows = wrap(&spans, 8, 2, Breaks::Words);
        assert_eq!(text(&rows), ["aaaa", "bbbb cc…"]);
        let last = rows.last().unwrap().last().unwrap();
        assert_eq!(last.0, "…");
        assert_eq!(last.1, blue);
    }

    #[test]
    fn rows_never_exceed_the_width() {
        let values = [
            "main",
            "3768ea4  feat: remove microslop support and fix CI (#27)",
            "origin  https://github.com/some-organization/a-long-repository-name",
            "origin  git@github.com:vzio/vortex.git",
            "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
            "a b c d e f g h i j k l m n o p q r s t u v w x y z",
            "   leading and trailing spaces   ",
            "/home/user/projects/a/very/deeply/nested/directory/tree/with/files",
        ];
        for value in values {
            let spans = plain(value);
            let all: String = value.chars().filter(|c| *c != ' ').collect();
            for breaks in [Breaks::Words, Breaks::Url] {
                for width in 1..60 {
                    for max_rows in 1..=3 {
                        let rows = wrap(&spans, width, max_rows, breaks);
                        assert!(
                            (1..=max_rows).contains(&rows.len()),
                            "{value:?} {width} {max_rows}: {} rows",
                            rows.len()
                        );
                        for row in &rows {
                            assert!(
                                width_of(row) <= width,
                                "{value:?} at {width}: {:?}",
                                text(&rows)
                            );
                        }
                        // nothing is lost but spaces, unless the end is cut
                        let shown: String = text(&rows)
                            .concat()
                            .chars()
                            .filter(|c| *c != ' ')
                            .collect();
                        if shown.ends_with('…') {
                            assert!(all.starts_with(&shown[..shown.len() - '…'.len_utf8()]));
                        } else {
                            assert_eq!(shown, all, "{value:?} at {width}");
                        }
                    }
                }
            }
        }
    }
}
