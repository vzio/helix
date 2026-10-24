//! Inline git blame (fork feature): greyed-out virtual text after the end of lines.

use helix_core::Position;
use helix_view::{
    blame::BlameState, document::Mode, editor::InlineBlameShow, graphics::Style, Document, Editor,
    Theme, View,
};

use crate::ui::document::{LinePos, TextRenderer};
use crate::ui::text_decorations::Decoration;

/// The scope of inline blame. Themes that don't define it get the style of inlay hints.
const SCOPE: &str = "ui.virtual.inline-blame";
const FALLBACK_SCOPE: &str = "ui.virtual.inlay-hint";

/// Columns between the last character of the line and the blame.
const GAP: usize = 4;

enum Lines {
    /// Only this line, the one of the cursor.
    One(usize),
    /// Every line that is drawn.
    All,
}

pub struct InlineBlame<'a> {
    doc: &'a Document,
    format: String,
    lines: Lines,
    style: Style,
}

impl<'a> InlineBlame<'a> {
    /// The decoration for a view, if there is something to show in it.
    ///
    /// Inline blame is only shown in normal and select mode, in the focused view when it is for
    /// the cursor line (which also hides it while a panel like the file tree is focused), for
    /// documents that have a blame. In cursor line mode the line has to be the cursor line for
    /// a while: if it isn't yet, the wait is started here (it does nothing if it has been).
    pub fn new(
        editor: &'a Editor,
        doc: &'a Document,
        view: &View,
        is_focused: bool,
        theme: &Theme,
    ) -> Option<Self> {
        let config = &editor.config().inline_blame;
        if !config.enable
            || !matches!(editor.mode(), Mode::Normal | Mode::Select)
            || matches!(
                doc.blame.state(),
                BlameState::Unknown | BlameState::Pending | BlameState::Unavailable
            )
        {
            return None;
        }
        let lines = match config.show {
            InlineBlameShow::AllLines => Lines::All,
            InlineBlameShow::CursorLine => {
                if !is_focused {
                    return None;
                }
                let line = doc
                    .selection(view.id)
                    .primary()
                    .cursor_line(doc.text().slice(..));
                if config.delay > 0 && doc.blame.ready_line(view.id) != Some(line) {
                    crate::handlers::blame::arm_delay(
                        editor,
                        doc.id(),
                        view.id,
                        line,
                        config.delay,
                    );
                    return None;
                }
                Lines::One(line)
            }
        };
        let style = theme
            .try_get_exact(SCOPE)
            .unwrap_or_else(|| theme.get(FALLBACK_SCOPE));
        Some(Self {
            doc,
            format: config.format.clone(),
            lines,
            style,
        })
    }
}

impl Decoration for InlineBlame<'_> {
    fn render_virt_lines(
        &mut self,
        renderer: &mut TextRenderer,
        pos: LinePos,
        virt_off: Position,
    ) -> Position {
        let none = Position::new(0, 0);
        // A document line that wraps only has room for the blame after its last visual line.
        if !pos.is_last_visual_line {
            return none;
        }
        if let Lines::One(line) = self.lines {
            if line != pos.doc_line {
                return none;
            }
        }
        // Nothing to say about lines with nothing on them, it is just noise.
        let line = self.doc.text().line(pos.doc_line);
        if line.chars().all(char::is_whitespace) {
            return none;
        }
        let Some(text) = self.doc.line_blame(pos.doc_line).display(&self.format) else {
            return none;
        };

        // where the text of the line ends, plus what other decorations added to it
        // (the end of the line already is after the last character: the line ending, or the
        // cursor position at the end of the text, takes up a column)
        let start = virt_off.col + GAP - 1;
        // (columns are relative to the start of the document line, the view might be scrolled
        // horizontally)
        let Some(start_in_view) = start.checked_sub(renderer.offset.col) else {
            return none;
        };
        let available = (renderer.viewport.width as usize).saturating_sub(start_in_view);
        if available == 0 {
            return none;
        }
        // newlines in a commit message must not mess up the line
        let text: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let x = renderer.viewport.x + start_in_view as u16;
        let (end_x, _) = renderer.set_string_truncated(
            x,
            pos.visual_line,
            &text,
            available,
            |_| self.style,
            true,
            false,
        );
        Position::new(0, GAP - 1 + (end_x - x) as usize)
    }
}

#[cfg(test)]
mod test {
    use std::path::Path;
    use std::sync::Arc;

    use arc_swap::ArcSwap;
    use helix_core::{
        syntax::{self, config::SoftWrap},
        text_annotations::TextAnnotations,
        Rope,
    };
    use helix_view::{blame::BlameOutcome, editor::Config, graphics::Rect, view::ViewPosition};
    use tui::buffer::Buffer;

    use super::*;
    use crate::ui::document::render_document;
    use crate::ui::text_decorations::DecorationManager;

    fn document(text: &str, config: Config) -> Document {
        let mut doc = Document::from(
            Rope::from(text),
            None,
            Arc::new(ArcSwap::new(Arc::new(config))),
            Arc::new(ArcSwap::from_pointee(syntax::Loader::default())),
        );
        // every line is not committed yet
        let request = doc.blame.begin(Path::new("/repo/file")).unwrap();
        doc.blame
            .finish(request.generation, BlameOutcome::NotCommitted);
        doc
    }

    /// The rows of the screen with the document `doc` rendered with inline blame.
    fn render(doc: &Document, lines: Lines, width: u16, offset: ViewPosition) -> Vec<String> {
        let area = Rect::new(0, 0, width, 6);
        let mut surface = Buffer::empty(area);
        let theme = Theme::default();
        let mut decorations = DecorationManager::default();
        decorations.add_decoration(InlineBlame {
            doc,
            format: String::new(),
            lines,
            style: Style::default(),
        });
        render_document(
            &mut surface,
            area,
            doc,
            offset,
            &TextAnnotations::default(),
            None,
            Vec::new(),
            &theme,
            decorations,
        );
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| surface[(x, y)].symbol.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn blame_follows_the_end_of_lines() {
        let doc = document("hello\nhi\n\n   \nlast", Config::default());
        let rows = render(&doc, Lines::All, 40, ViewPosition::default());
        assert_eq!(rows[0], "hello    Not committed yet");
        assert_eq!(rows[1], "hi    Not committed yet");
        // nothing for lines with nothing on them
        assert_eq!(rows[2], "");
        assert_eq!(rows[3], "");
        assert_eq!(rows[4], "last    Not committed yet");
    }

    #[test]
    fn only_the_cursor_line() {
        let doc = document("one\ntwo\nthree\n", Config::default());
        let rows = render(&doc, Lines::One(1), 40, ViewPosition::default());
        assert_eq!(rows[0], "one");
        assert_eq!(rows[1], "two    Not committed yet");
        assert_eq!(rows[2], "three");
    }

    #[test]
    fn blame_is_truncated_to_the_viewport() {
        let doc = document("hello\n", Config::default());
        let rows = render(&doc, Lines::All, 20, ViewPosition::default());
        assert_eq!(rows[0].chars().count(), 20);
        assert!(rows[0].starts_with("hello    Not commi"));
        assert!(rows[0].ends_with('…'));

        // no room at all: left out, the text is untouched
        let rows = render(&doc, Lines::All, 9, ViewPosition::default());
        assert_eq!(rows[0], "hello");
        let rows = render(&doc, Lines::All, 5, ViewPosition::default());
        assert_eq!(rows[0], "hello");
    }

    #[test]
    fn horizontally_scrolled_views() {
        let doc = document("0123456789\nab\n", Config::default());
        let offset = ViewPosition {
            horizontal_offset: 4,
            ..Default::default()
        };
        let rows = render(&doc, Lines::All, 30, offset);
        // 6 columns of the first line are left
        assert_eq!(rows[0], "456789    Not committed yet");
        // the blame scrolls with the line
        assert_eq!(rows[1], "  Not committed yet");
    }

    #[test]
    fn wrapped_lines_have_blame_on_their_last_visual_line_only() {
        let config = Config {
            soft_wrap: SoftWrap {
                enable: Some(true),
                wrap_indicator: Some(String::new()),
                ..Default::default()
            },
            ..Default::default()
        };
        let doc = document("aaaa bbbb cccc\nz\n", config);
        let rows = render(&doc, Lines::All, 24, ViewPosition::default());
        // fits in one row
        assert_eq!(rows[0], "aaaa bbbb cccc    Not c…");
        assert_eq!(rows[1], "z    Not committed yet");

        let rows = render(&doc, Lines::All, 12, ViewPosition::default());
        // "aaaa bbbb " / "cccc": the first row is not the last one of the line
        assert_eq!(rows[0], "aaaa bbbb");
        assert!(rows[1].starts_with("cccc    "), "{rows:?}");
        assert!(rows[1].ends_with('…'), "{rows:?}");
        assert!(rows[2].starts_with("z    "), "{rows:?}");
    }

    #[test]
    fn theme_scope_falls_back_to_inlay_hints() {
        let hint = Style::default().fg(helix_view::graphics::Color::Red);
        let blame = Style::default().fg(helix_view::graphics::Color::Blue);
        let theme = |toml: &str| Theme::from(toml::from_str::<toml::Value>(toml).unwrap());

        let only_hint = theme("\"ui.virtual.inlay-hint\" = { fg = \"red\" }");
        assert_eq!(
            only_hint
                .try_get_exact(SCOPE)
                .unwrap_or_else(|| only_hint.get(FALLBACK_SCOPE)),
            hint
        );
        let both = theme(
            "\"ui.virtual.inlay-hint\" = { fg = \"red\" }\n\"ui.virtual.inline-blame\" = { fg = \"blue\" }",
        );
        assert_eq!(
            both.try_get_exact(SCOPE)
                .unwrap_or_else(|| both.get(FALLBACK_SCOPE)),
            blame
        );
    }
}
