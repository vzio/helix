//! Git merge conflict blocks in a text (this fork, see FORK.md, "Conflict diagnostics").
//!
//! A block is what git writes into a file it can't merge:
//!
//! ```text
//! <<<<<<< HEAD
//! our side
//! ||||||| base          (only with merge.conflictStyle diff3 or zdiff3)
//! the common ancestor
//! =======
//! their side
//! >>>>>>> feature-x
//! ```
//!
//! The markers are at the start of a line and at least 7 characters long (the length of all the
//! markers of a block is the same, and longer with the `conflict-marker-size` attribute). Only
//! complete blocks count: a text with a stray marker line is not reported. The editor only looks
//! for blocks in the files git reports as unmerged, so docs and fixtures with marker-like lines
//! aren't flagged.

use std::ops::RangeInclusive;

/// The length of a marker when the `conflict-marker-size` attribute isn't set.
const MARKER_SIZE: usize = 7;

/// A conflict block, by lines (0-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictBlock {
    /// From the `<<<<<<<` line to the `>>>>>>>` line, both included.
    pub lines: RangeInclusive<usize>,
    /// The `|||||||` line, with the diff3 and zdiff3 styles.
    pub base: Option<usize>,
    /// The `=======` line.
    pub separator: usize,
    /// The label after `<<<<<<<`: the side being merged into (`HEAD`), empty when there is none.
    pub ours: String,
    /// The label after `>>>>>>>`: the side merged in (`feature-x`), empty when there is none.
    pub theirs: String,
}

impl ConflictBlock {
    /// "Conflict: HEAD vs feature-x".
    pub fn message(&self) -> String {
        let label = |label: &str, side: &'static str| -> String {
            if label.is_empty() {
                side.to_string()
            } else {
                label.to_string()
            }
        };
        format!(
            "Conflict: {} vs {}",
            label(&self.ours, "ours"),
            label(&self.theirs, "theirs")
        )
    }
}

/// The kind of a marker line, its length and its label.
#[derive(Debug, PartialEq, Eq)]
enum Marker<'a> {
    Start(usize, &'a str),
    Base(usize),
    Separator(usize),
    End(usize, &'a str),
}

/// The marker `line` is, if it is one.
fn marker(line: &str) -> Option<Marker<'_>> {
    let line = line.trim_end_matches(['\n', '\r']);
    let first = line.chars().next()?;
    if !matches!(first, '<' | '|' | '=' | '>') {
        return None;
    }
    let len = line.len() - line.trim_start_matches(first).len();
    if len < MARKER_SIZE {
        return None;
    }
    let rest = &line[len..];
    // a label is separated by a space; `=======` has none
    let label = match rest.strip_prefix(' ') {
        Some(label) => label.trim(),
        None if rest.is_empty() => "",
        None => return None,
    };
    Some(match first {
        '<' => Marker::Start(len, label),
        '|' => Marker::Base(len),
        '=' if rest.is_empty() => Marker::Separator(len),
        '>' => Marker::End(len, label),
        _ => return None,
    })
}

/// The block being read: where it started, its marker length and the markers seen so far.
struct Open {
    start: usize,
    len: usize,
    base: Option<usize>,
    separator: Option<usize>,
}

/// The complete conflict blocks of a text, given as its lines (with or without their line
/// endings), in order.
pub fn conflict_blocks<I, S>(lines: I) -> Vec<ConflictBlock>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut blocks = Vec::new();
    let mut open: Option<Open> = None;
    // the labels borrow from the lines, which the iterator doesn't keep: keep the start label
    let mut ours = String::new();
    for (number, line) in lines.into_iter().enumerate() {
        let line = line.as_ref();
        let Some(marker) = marker(line) else {
            continue;
        };
        match marker {
            // a new block starts here, even inside an unfinished one (which is dropped)
            Marker::Start(len, label) => {
                ours = label.to_string();
                open = Some(Open {
                    start: number,
                    len,
                    base: None,
                    separator: None,
                });
            }
            Marker::Base(len) => {
                if let Some(block) = open.as_mut().filter(|block| block.len == len) {
                    if block.base.is_none() && block.separator.is_none() {
                        block.base = Some(number);
                    }
                }
            }
            Marker::Separator(len) => {
                if let Some(block) = open.as_mut().filter(|block| block.len == len) {
                    if block.separator.is_none() {
                        block.separator = Some(number);
                    }
                }
            }
            Marker::End(len, label) => {
                if let Some(block) = open.take_if(|block| block.len == len) {
                    if let Some(separator) = block.separator {
                        blocks.push(ConflictBlock {
                            lines: block.start..=number,
                            base: block.base,
                            separator,
                            ours: std::mem::take(&mut ours),
                            theirs: label.to_string(),
                        });
                    } else {
                        // `>>>>>>>` before `=======`: not a block, look for the next one
                        open = None;
                    }
                }
            }
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(text: &str) -> Vec<ConflictBlock> {
        conflict_blocks(text.split_inclusive('\n'))
    }

    #[test]
    fn a_merge_conflict() {
        let text = "fn main() {\n<<<<<<< HEAD\n    ours();\n=======\n    theirs();\n>>>>>>> feature-x\n}\n";
        assert_eq!(
            blocks(text),
            [ConflictBlock {
                lines: 1..=5,
                base: None,
                separator: 3,
                ours: "HEAD".into(),
                theirs: "feature-x".into(),
            }]
        );
        assert_eq!(blocks(text)[0].message(), "Conflict: HEAD vs feature-x");
    }

    #[test]
    fn diff3_blocks_and_several_per_file() {
        let text = "\
<<<<<<< HEAD
a
||||||| merged common ancestors
b
=======
c
>>>>>>> topic
same
<<<<<<< HEAD
d
||||||| 1234567 (base)
=======
>>>>>>> 89abcde (Rebased commit)
";
        let found = blocks(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].lines, 0..=6);
        assert_eq!(found[0].base, Some(2));
        assert_eq!(found[0].separator, 4);
        assert_eq!(found[1].lines, 8..=12);
        assert_eq!(found[1].base, Some(10));
        assert_eq!(found[1].separator, 11);
        assert_eq!(
            found[1].message(),
            "Conflict: HEAD vs 89abcde (Rebased commit)"
        );
    }

    #[test]
    fn crlf_and_missing_labels() {
        let text = "<<<<<<<\r\nx\r\n=======\r\ny\r\n>>>>>>>\r\n";
        let found = blocks(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message(), "Conflict: ours vs theirs");
    }

    #[test]
    fn longer_markers_from_the_conflict_marker_size_attribute() {
        let text = "<<<<<<<<<< ours\n======= not a separator here\n==========\n>>>>>>>>>> theirs\n";
        let found = blocks(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].separator, 2);
        // markers of another length don't close it
        let text = "<<<<<<<<<< ours\n=======\n>>>>>>> theirs\n";
        assert!(blocks(text).is_empty());
    }

    #[test]
    fn incomplete_or_marker_like_text_is_not_a_block() {
        for text in [
            // the start marker removed
            "a\n=======\nb\n>>>>>>> topic\n",
            // the end marker removed
            "<<<<<<< HEAD\na\n=======\nb\n",
            // no separator
            "<<<<<<< HEAD\na\n>>>>>>> topic\n",
            // indented, or too short, or a label without a space
            "  <<<<<<< HEAD\na\n=======\nb\n>>>>>>> topic\n",
            "<<<<<< HEAD\na\n======\nb\n>>>>>> topic\n",
            "<<<<<<<HEAD\na\n=======\nb\n>>>>>>> topic\n",
            // a Markdown heading underline
            "Title\n=======\n\ntext\n",
        ] {
            assert!(blocks(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn an_unfinished_block_is_dropped_for_the_next_one() {
        let text = "<<<<<<< HEAD\na\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> x\n";
        let found = blocks(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lines, 2..=6);
    }
}
