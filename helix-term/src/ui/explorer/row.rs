//! The entries of the tree and the rows the picker shows for them.

use std::path::PathBuf;

use helix_view::graphics::{Modifier, Style};
use helix_view::Theme;
use tui::text::{Span, Spans};

use super::git_status::Mark;
use super::tree::Node;

/// An entry of the tree as it is shown: a node of the tree, which may be expanded.
#[derive(Clone, Debug)]
pub struct Entry {
    pub node: Node,
    /// Whether the entries below the directory are shown.
    pub expanded: bool,
    /// Positions (in characters) in the name of the characters the filter matched.
    pub matched: Vec<u32>,
}

impl Entry {
    pub fn new(node: Node, expanded: bool) -> Self {
        Self {
            node,
            expanded,
            matched: Vec::new(),
        }
    }
}

/// The item of the picker for an entry: where it is, for the preview, and how it looks.
#[derive(Clone)]
pub struct Row {
    pub path: PathBuf,
    line: Spans<'static>,
}

impl Row {
    pub fn new(entry: &Entry, mark: Option<Mark>, current: bool, cut: bool, look: &Look) -> Self {
        let node = &entry.node;
        let mut style = if node.is_dir {
            look.directory
        } else {
            look.text
        };
        if let Some(mark) = mark {
            // The color of the change, but not its background or modifiers.
            style.fg = mark.style(look.theme).fg.or(style.fg);
        }
        if node.ignored {
            style = style.patch(look.ignored);
        }
        if current {
            style = style.add_modifier(Modifier::BOLD);
        }
        if cut {
            style = style.add_modifier(Modifier::ITALIC);
        }

        let sign = match (node.is_dir, entry.expanded) {
            (true, true) => "- ",
            (true, false) => "+ ",
            (false, _) => "  ",
        };
        let mut spans = vec![Span::styled(
            format!("{}{sign}", "  ".repeat(node.depth)),
            style,
        )];
        // Runs of characters with the same style: the matched ones stand out.
        let mut run = String::new();
        let mut run_matched = false;
        let mut push_run = |run: &mut String, matched: bool| {
            if !run.is_empty() {
                let style = if matched {
                    style.patch(look.matched)
                } else {
                    style
                };
                spans.push(Span::styled(std::mem::take(run), style));
            }
        };
        for (index, ch) in node.name.chars().enumerate() {
            let matched = entry.matched.contains(&(index as u32));
            if matched != run_matched {
                push_run(&mut run, run_matched);
                run_matched = matched;
            }
            run.push(ch);
        }
        push_run(&mut run, run_matched);
        if node.is_dir {
            spans.push(Span::styled("/", style));
        }
        if let Some(mark) = mark {
            let letter = if node.is_dir { "●" } else { mark.letter() };
            spans.push(Span::styled(format!(" {letter}"), mark.style(look.theme)));
        }
        Self {
            path: node.path.clone(),
            line: Spans::from(spans),
        }
    }

    pub fn line(&self) -> Spans<'static> {
        self.line.clone()
    }

    /// What the row says, without the styles.
    #[cfg(test)]
    pub fn text(&self) -> String {
        self.line.0.iter().map(|span| &*span.content).collect()
    }
}

/// The styles of the theme the rows are drawn with.
pub struct Look<'a> {
    theme: &'a Theme,
    directory: Style,
    text: Style,
    ignored: Style,
    matched: Style,
}

impl<'a> Look<'a> {
    pub fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            directory: theme.get("ui.text.directory"),
            text: theme.get("ui.text"),
            ignored: theme
                .try_get("ui.explorer.ignored")
                .or_else(|| theme.try_get("ui.text.inactive"))
                .unwrap_or_else(|| Style::default().add_modifier(Modifier::DIM)),
            matched: theme.get("special").add_modifier(Modifier::BOLD),
        }
    }
}
