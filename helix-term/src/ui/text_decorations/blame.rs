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

/// Columns between the end of the line and the blame.
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
        if self
            .doc
            .text()
            .line(pos.doc_line)
            .chars()
            .all(char::is_whitespace)
        {
            return none;
        }
        let Some(text) = self.doc.line_blame(pos.doc_line).display(&self.format) else {
            return none;
        };

        // where the text of the line ends, plus what other decorations added to it
        let start = virt_off.col + GAP;
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
        Position::new(0, GAP + (end_x - x) as usize)
    }
}
