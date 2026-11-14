//! The key help overlay (this fork): `?` in the normal mode of the fork's modal pickers and
//! views lists their keys in a box over them, instead of printing them to the status line. Any
//! key closes it.

use tui::buffer::Buffer as Surface;
use tui::text::Text;
use tui::widgets::{Block, Paragraph, Widget};
use vortex_view::graphics::{Margin, Rect};
use vortex_view::info::Info;

use crate::compositor::{Component, Context, Event, EventResult};

/// The id of the overlay in the compositor.
pub const ID: &str = "key-help";

/// A box of keys, in groups.
pub struct KeyHelp {
    info: Info,
}

/// The keys of one group: `(keys, what they do)`.
pub type Keys = Vec<(&'static str, &'static str)>;

impl KeyHelp {
    /// The keys of `title` in groups, each under its label.
    pub fn new(title: impl Into<String>, groups: &[(&str, Keys)]) -> Self {
        let sections: Vec<(Option<&str>, Keys)> = groups
            .iter()
            .filter(|(_, keys)| !keys.is_empty())
            .map(|(label, keys)| (Some(*label), keys.clone()))
            .collect();
        let title = format!(" {} · any key closes ", title.into());
        Self {
            info: Info::grouped(title, &sections),
        }
    }

    /// A box of an infobox already laid out (a keymap's: debug mode's keys as configured).
    pub fn from_info(info: Info) -> Self {
        Self { info }
    }

    /// The text of the box, its lines (for tests).
    pub fn text(&self) -> &str {
        &self.info.text
    }

    /// A callback that pushes the overlay.
    pub fn push(self) -> crate::compositor::Callback {
        Box::new(move |compositor, _| compositor.push(Box::new(self)))
    }
}

impl Component for KeyHelp {
    fn handle_event(&mut self, event: &Event, _cx: &mut Context) -> EventResult {
        match event {
            Event::Key(_) => EventResult::Consumed(Some(Box::new(|compositor, _| {
                compositor.remove(ID);
            }))),
            Event::Mouse(_) | Event::Paste(_) => EventResult::Consumed(None),
            _ => EventResult::Ignored(None),
        }
    }

    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let theme = &cx.editor.theme;
        let text_style = theme.get("ui.text.info");
        let popup_style = theme.get("ui.popup.info");
        let dim = theme
            .try_get("ui.infobox.separator")
            .or_else(|| theme.try_get("ui.text.inactive"))
            .map(|dim| popup_style.patch(dim))
            .unwrap_or(text_style);
        let title_width =
            vortex_core::unicode::width::UnicodeWidthStr::width(self.info.title.as_ref()) as u16;
        let width = (self.info.width.max(title_width) + 4).min(area.width);
        let height = (self.info.height + 2).min(area.height);
        let box_area = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        surface.clear_with(box_area, popup_style);
        let block = Block::bordered()
            .title(self.info.title.as_ref())
            .border_style(popup_style);
        let inner = block.inner(box_area).inner(Margin::horizontal(1));
        block.render(box_area, surface);
        Paragraph::new(&Text::from(self.info.text.as_str()))
            .style(text_style)
            .render(inner, surface);
        for &line in &self.info.separators {
            if line < inner.height {
                surface.set_style(Rect::new(inner.x, inner.y + line, inner.width, 1), dim);
            }
        }
    }

    fn id(&self) -> Option<&'static str> {
        Some(ID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_and_keys() {
        let help = KeyHelp::new(
            "Commit log",
            &[
                (
                    "commits",
                    vec![("v", "extend"), ("A-s", "the whole section")],
                ),
                ("empty", vec![]),
                ("list", vec![("j k", "move"), ("?", "these keys")]),
            ],
        );
        let lines: Vec<&str> = help.text().lines().collect();
        assert!(lines[0].starts_with("── commits "), "{lines:?}");
        assert_eq!(lines[1], "v    extend");
        assert_eq!(lines[2], "A-s  the whole section");
        assert!(lines[3].starts_with("── list "), "{lines:?}");
        assert_eq!(lines.len(), 6);
        assert!(help.info.title.contains("Commit log"));
    }
}
