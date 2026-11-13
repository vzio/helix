use crate::register::Registers;
use std::{borrow::Cow, fmt::Write};
use vortex_core::unicode::width::UnicodeWidthStr;

#[derive(Debug)]
/// Info box used in editor. Rendering logic will be in other crate.
pub struct Info {
    /// Title shown at top.
    pub title: Cow<'static, str>,
    /// Text body, should contain newlines.
    pub text: String,
    /// Body width.
    pub width: u16,
    /// Body height.
    pub height: u16,
    /// The lines of the body that separate groups (this fork), drawn dim.
    pub separators: Vec<u16>,
}

impl Info {
    pub fn new<T, K, V>(title: T, body: &[(K, V)]) -> Self
    where
        T: Into<Cow<'static, str>>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let title = title.into();
        if body.is_empty() {
            return Self {
                height: 1,
                width: title.len() as u16,
                text: "".to_string(),
                title,
                separators: Vec::new(),
            };
        }

        let item_width = body
            .iter()
            .map(|(item, _)| item.as_ref().width())
            .max()
            .unwrap();
        let mut text = String::new();

        for (item, desc) in body {
            let _ = writeln!(
                text,
                "{:width$}  {}",
                item.as_ref(),
                desc.as_ref(),
                width = item_width
            );
        }

        Self {
            title,
            width: text.lines().map(|l| l.width()).max().unwrap() as u16,
            height: body.len() as u16,
            text,
            separators: Vec::new(),
        }
    }

    /// An infobox in groups (this fork): each group's entries under a separator line with its
    /// label (`── files ───`), or without one for `None`. The keys line up across groups.
    pub fn grouped<T, K, V>(title: T, sections: &[(Option<&str>, Vec<(K, V)>)]) -> Self
    where
        T: Into<Cow<'static, str>>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let title = title.into();
        let item_width = sections
            .iter()
            .flat_map(|(_, body)| body.iter().map(|(item, _)| item.as_ref().width()))
            .max()
            .unwrap_or(0);
        let entry_width = sections
            .iter()
            .flat_map(|(_, body)| body.iter())
            .map(|(_, desc)| item_width + 2 + desc.as_ref().width())
            .max()
            .unwrap_or(0);
        let label_width = sections
            .iter()
            .filter_map(|(label, _)| label.map(|label| label.width() + 4))
            .max()
            .unwrap_or(0);
        let width = entry_width.max(label_width).max(title.width());
        let mut text = String::new();
        let mut separators = Vec::new();
        let mut line = 0;
        for (label, body) in sections {
            let head = match label {
                Some(label) => format!("── {label} "),
                None => String::new(),
            };
            let _ = writeln!(text, "{head}{}", "─".repeat(width - head.width()));
            separators.push(line);
            line += 1;
            for (item, desc) in body {
                let _ = writeln!(
                    text,
                    "{:width$}  {}",
                    item.as_ref(),
                    desc.as_ref(),
                    width = item_width
                );
                line += 1;
            }
        }
        Self {
            title,
            width: width as u16,
            height: line,
            text,
            separators,
        }
    }

    pub fn from_registers(title: impl Into<Cow<'static, str>>, registers: &Registers) -> Self {
        let body: Vec<_> = registers
            .iter_preview()
            .map(|(ch, preview)| (ch.to_string(), preview))
            .collect();

        let mut infobox = Self::new(title, &body);
        infobox.width = 30; // copied content could be very long
        infobox
    }
}
