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
        let lines: Vec<&str> = self.info.text.lines().collect();
        // too tall for the screen (debug mode's keys): two columns, split at the group nearest the
        // middle, when they fit side by side
        let split = (self.info.height + 2 > area.height
            && self.info.width * 2 + 3 + 4 <= area.width)
            .then(|| {
                let middle = lines.len() / 2;
                self.info
                    .separators
                    .iter()
                    .map(|&line| line as usize)
                    .filter(|&line| line > 0)
                    .min_by_key(|&line| line.abs_diff(middle))
            })
            .flatten();
        let columns: Vec<(&[&str], usize)> = match split {
            Some(at) => vec![(&lines[..at], 0), (&lines[at..], at)],
            None => vec![(&lines[..], 0)],
        };
        let body_height = columns
            .iter()
            .map(|(column, _)| column.len())
            .max()
            .unwrap_or(0) as u16;
        let body_width = self.info.width * columns.len() as u16 + 3 * (columns.len() as u16 - 1);
        let width = (body_width.max(title_width) + 4).min(area.width);
        let height = (body_height + 2).min(area.height);
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
        for (index, (column, first)) in columns.iter().enumerate() {
            let x = inner.x + index as u16 * (self.info.width + 3);
            let area = Rect::new(
                x,
                inner.y,
                self.info.width.min(inner.right().saturating_sub(x)),
                inner.height,
            );
            Paragraph::new(&Text::from(column.join("\n")))
                .style(text_style)
                .render(area, surface);
            for &line in &self.info.separators {
                let Some(row) = (line as usize).checked_sub(*first) else {
                    continue;
                };
                if row < column.len() && (row as u16) < area.height {
                    surface.set_style(Rect::new(area.x, area.y + row as u16, area.width, 1), dim);
                }
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
