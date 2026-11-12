//! Time per line (fork feature, the debugger's spectator mode): how long the steps that left a
//! line took, greyed after its end like inline blame, so the code reads as a rough flame graph.

use std::collections::BTreeMap;

use vortex_core::Position;
use vortex_view::{debug::timing::format_line_time, graphics::Style, Document, Editor, Theme};

use crate::ui::document::{LinePos, TextRenderer};
use crate::ui::text_decorations::Decoration;

/// The scope of the labels; themes that don't define it get inline blame's style, then inlay
/// hints'.
const SCOPE: &str = "ui.virtual.step-time";
const FALLBACKS: [&str; 2] = ["ui.virtual.inline-blame", "ui.virtual.inlay-hint"];
/// Columns between the end of the line and the label.
const GAP: usize = 3;

pub struct StepTimes {
    labels: BTreeMap<usize, String>,
    style: Style,
}

impl StepTimes {
    /// The decoration for a document with labels.
    pub fn new(editor: &Editor, doc: &Document, theme: &Theme) -> Option<Self> {
        let timing = &editor.debug.spectator.timing;
        let lines = timing.file(doc.path()?)?;
        if lines.is_empty() {
            return None;
        }
        let style = theme
            .try_get_exact(SCOPE)
            .or_else(|| {
                FALLBACKS
                    .iter()
                    .find_map(|scope| theme.try_get_exact(scope))
            })
            .unwrap_or_else(|| theme.get("ui.text.inactive"));
        Some(Self {
            labels: lines
                .iter()
                .map(|(line, time)| (*line, format_line_time(time)))
                .collect(),
            style,
        })
    }
}

impl Decoration for StepTimes {
    fn render_virt_lines(
        &mut self,
        renderer: &mut TextRenderer,
        pos: LinePos,
        virt_off: Position,
    ) -> Position {
        let none = Position::new(0, 0);
        if !pos.is_last_visual_line {
            return none;
        }
        let Some(text) = self.labels.get(&pos.doc_line) else {
            return none;
        };
        let start = virt_off.col + GAP - 1;
        let Some(start_in_view) = start.checked_sub(renderer.offset.col) else {
            return none;
        };
        let available = (renderer.viewport.width as usize).saturating_sub(start_in_view);
        if available == 0 {
            return none;
        }
        let x = renderer.viewport.x + start_in_view as u16;
        let style = self.style;
        let (end_x, _) = renderer.set_string_truncated(
            x,
            pos.visual_line,
            text,
            available,
            |_| style,
            true,
            false,
        );
        Position::new(0, GAP - 1 + (end_x - x) as usize)
    }
}
