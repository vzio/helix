//! Bookmarks (this fork): lines set by hand with `C-s`, walked with `C-i`/`C-o` in the order they
//! were set, across files, as a ring.
//!
//! A buffer keeps the bookmarks of its lines ([`Marks`]) and maps them through every change, as
//! it does its selections: a bookmark stays on its line when lines come and go above it, and goes
//! when its line is deleted. A file's bookmarks outlive its buffer: closing it keeps them by path
//! and line until the file is open again; a buffer without a file takes its bookmarks with it.
//! The order is a sequence number given when a bookmark is set; a bookmark's number is its rank,
//! so removing one closes the gap. All of it is in memory only.

use std::path::{Path, PathBuf};

use vortex_core::{ChangeSet, Operation, Rope, RopeSlice};

use crate::{DocumentId, Editor};

/// A bookmark in a buffer: the start of its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// When it was set: the order of the ring.
    pub seq: u64,
    /// The first character of its line.
    pos: usize,
}

/// The bookmarks of a buffer, one per line.
#[derive(Debug, Clone, Default)]
pub struct Marks(Vec<Mark>);

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The bookmarks with their lines.
    pub fn lines<'a>(&'a self, text: RopeSlice<'a>) -> impl Iterator<Item = (u64, usize)> + 'a {
        self.0
            .iter()
            .map(move |mark| (mark.seq, text.char_to_line(mark.pos.min(text.len_chars()))))
    }

    pub fn seq_at(&self, text: RopeSlice, line: usize) -> Option<u64> {
        self.lines(text)
            .find(|&(_, at)| at == line)
            .map(|(seq, _)| seq)
    }

    /// Sets a bookmark with the order `seq` on `line`.
    pub fn insert(&mut self, text: RopeSlice, line: usize, seq: u64) {
        let line = line.min(text.len_lines().saturating_sub(1));
        if self.seq_at(text, line).is_none() {
            self.0.push(Mark {
                seq,
                pos: text.line_to_char(line),
            });
        }
    }

    pub fn remove(&mut self, seq: u64) {
        self.0.retain(|mark| mark.seq != seq);
    }

    /// Maps the bookmarks through `changes` from `old` to `new`: a bookmark whose whole line is
    /// deleted goes, the others stay on their line; of two bookmarks that end on the same line
    /// (joined lines), the older stays.
    pub(crate) fn apply(&mut self, old: &Rope, new: RopeSlice, changes: &ChangeSet) {
        if self.0.is_empty() {
            return;
        }
        // the deleted ranges, in the old text
        let mut deleted = Vec::new();
        let mut pos = 0;
        for op in changes.changes() {
            match op {
                Operation::Retain(n) => pos += n,
                Operation::Delete(n) => {
                    deleted.push(pos..pos + n);
                    pos += n;
                }
                Operation::Insert(_) => {}
            }
        }
        let old = old.slice(..);
        self.0.retain_mut(|mark| {
            let line = old.char_to_line(mark.pos.min(old.len_chars()));
            let start = old.line_to_char(line);
            let last = line + 1 >= old.len_lines();
            let end = if last {
                old.len_chars()
            } else {
                old.line_to_char(line + 1)
            };
            // the last line has no newline of its own: it goes with the one before it
            if end > start
                && deleted.iter().any(|range| {
                    range.start <= start && range.end >= end && (!last || range.start < start)
                })
            {
                return false;
            }
            // text inserted at the start of the line goes before it
            let moved = changes.map_pos(start, vortex_core::Assoc::After);
            mark.pos = new.line_to_char(new.char_to_line(moved.min(new.len_chars())));
            true
        });
        self.0.sort_by_key(|mark| (mark.pos, mark.seq));
        self.0.dedup_by_key(|mark| mark.pos);
    }
}

/// A bookmark of a closed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedMark {
    pub seq: u64,
    pub path: PathBuf,
    pub line: usize,
}

/// The editor's bookmarks besides the buffers': the next number to give, the closed files', and
/// the one the ring is at.
#[derive(Debug, Clone, Default)]
pub struct Bookmarks {
    next_seq: u64,
    pub closed: Vec<ClosedMark>,
    /// The bookmark last gone to or set: `C-i` and `C-o` go on from it when the cursor isn't on
    /// one (it may have been removed since).
    pub current: Option<u64>,
}

/// Where a bookmark is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    Buffer { doc: DocumentId, line: usize },
    File { path: PathBuf, line: usize },
}

/// A bookmark, from [`Editor::bookmarks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub seq: u64,
    pub at: At,
}

impl Bookmark {
    pub fn line(&self) -> usize {
        match self.at {
            At::Buffer { line, .. } | At::File { line, .. } => line,
        }
    }
}

/// Which way `C-i` and `C-o` go round the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Next,
    Previous,
}

/// The bookmark after (or before) `from` in the order they were set, round the ring; the first
/// (or last) without `from`. `from` need not be one of them any more.
pub fn step(seqs: &[u64], from: Option<u64>, direction: Direction) -> Option<u64> {
    let first = *seqs.first()?;
    let last = *seqs.last()?;
    let Some(from) = from else {
        return Some(match direction {
            Direction::Next => first,
            Direction::Previous => last,
        });
    };
    Some(match direction {
        Direction::Next => seqs
            .iter()
            .copied()
            .find(|&seq| seq > from)
            .unwrap_or(first),
        Direction::Previous => seqs
            .iter()
            .copied()
            .rev()
            .find(|&seq| seq < from)
            .unwrap_or(last),
    })
}

impl Editor {
    /// Every bookmark, in the order they were set: a bookmark's number is its index plus one.
    pub fn bookmarks(&self) -> Vec<Bookmark> {
        let mut all: Vec<Bookmark> = self
            .documents
            .values()
            .flat_map(|doc| {
                let text = doc.text().slice(..);
                doc.bookmarks.lines(text).map(move |(seq, line)| Bookmark {
                    seq,
                    at: At::Buffer {
                        doc: doc.id(),
                        line,
                    },
                })
            })
            .chain(self.bookmark_state.closed.iter().map(|mark| Bookmark {
                seq: mark.seq,
                at: At::File {
                    path: mark.path.clone(),
                    line: mark.line,
                },
            }))
            .collect();
        all.sort_by_key(|bookmark| bookmark.seq);
        all
    }

    /// The numbers of the bookmarks of `doc`, by line, for the gutter.
    pub fn bookmark_numbers(&self, doc: DocumentId) -> Vec<(usize, usize)> {
        let Some(document) = self.document(doc) else {
            return Vec::new();
        };
        if document.bookmarks.is_empty() {
            return Vec::new();
        }
        self.bookmarks()
            .iter()
            .enumerate()
            .filter_map(|(index, bookmark)| match bookmark.at {
                At::Buffer { doc: id, line } if id == doc => Some((line, index + 1)),
                _ => None,
            })
            .collect()
    }

    /// Sets a bookmark on each of `lines` of `doc` that has none, removes the bookmark of each
    /// that has one; returns how many were set and removed.
    pub fn toggle_bookmarks(&mut self, doc: DocumentId, lines: &[usize]) -> (usize, usize) {
        let (mut set, mut removed) = (0, 0);
        let mut next_seq = self.bookmark_state.next_seq;
        let mut current = self.bookmark_state.current;
        let Some(document) = self.documents.get_mut(&doc) else {
            return (0, 0);
        };
        let text = document.text().clone();
        let text = text.slice(..);
        for &line in lines {
            match document.bookmarks.seq_at(text, line) {
                Some(seq) => {
                    document.bookmarks.remove(seq);
                    removed += 1;
                }
                None => {
                    document.bookmarks.insert(text, line, next_seq);
                    current = Some(next_seq);
                    next_seq += 1;
                    set += 1;
                }
            }
        }
        self.bookmark_state.next_seq = next_seq;
        self.bookmark_state.current = current;
        (set, removed)
    }

    /// Removes the bookmark `seq`, wherever it is.
    pub fn remove_bookmark(&mut self, seq: u64) {
        for doc in self.documents.values_mut() {
            doc.bookmarks.remove(seq);
        }
        self.bookmark_state.closed.retain(|mark| mark.seq != seq);
    }

    /// The bookmark the ring goes to from the cursor: after (or before) the one on its line, or
    /// else the last one gone to or set.
    pub fn bookmark_step(&self, direction: Direction) -> Option<Bookmark> {
        let all = self.bookmarks();
        let seqs: Vec<u64> = all.iter().map(|bookmark| bookmark.seq).collect();
        let (view, doc) = crate::current_ref!(self);
        let text = doc.text().slice(..);
        let line = text.char_to_line(doc.selection(view.id).primary().cursor(text));
        let from = doc
            .bookmarks
            .seq_at(text, line)
            .or(self.bookmark_state.current);
        let seq = step(&seqs, from, direction)?;
        all.into_iter().find(|bookmark| bookmark.seq == seq)
    }

    /// Keeps the bookmarks of a file whose buffer closes, by path and line.
    pub(crate) fn keep_closed_bookmarks(&mut self, doc: DocumentId) {
        let Some(document) = self.documents.get(&doc) else {
            return;
        };
        let Some(path) = document.path() else {
            return;
        };
        let text = document.text().slice(..);
        let closed: Vec<ClosedMark> = document
            .bookmarks
            .lines(text)
            .map(|(seq, line)| ClosedMark {
                seq,
                path: path.to_path_buf(),
                line,
            })
            .collect();
        self.bookmark_state.closed.extend(closed);
    }

    /// Gives a buffer opened on `path` the bookmarks its file kept while it was closed.
    pub(crate) fn adopt_closed_bookmarks(&mut self, doc: DocumentId, path: &Path) {
        if !self
            .bookmark_state
            .closed
            .iter()
            .any(|mark| mark.path == path)
        {
            return;
        }
        let (adopted, kept) = std::mem::take(&mut self.bookmark_state.closed)
            .into_iter()
            .partition(|mark| mark.path == path);
        self.bookmark_state.closed = kept;
        let Some(document) = self.documents.get_mut(&doc) else {
            return;
        };
        let text = document.text().clone();
        for mark in adopted {
            document
                .bookmarks
                .insert(text.slice(..), mark.line, mark.seq);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vortex_core::Transaction;

    fn lines(marks: &Marks, text: &Rope) -> Vec<(u64, usize)> {
        let mut lines: Vec<_> = marks.lines(text.slice(..)).collect();
        lines.sort();
        lines
    }

    fn change(text: &mut Rope, marks: &mut Marks, from: usize, to: usize, insert: &str) {
        let old = text.clone();
        let transaction = Transaction::change(
            text,
            std::iter::once((from, to, (!insert.is_empty()).then(|| insert.into()))),
        );
        assert!(transaction.apply(text));
        marks.apply(&old, text.slice(..), transaction.changes());
    }

    fn marked(text: &Rope, lines: &[usize]) -> Marks {
        let mut marks = Marks::default();
        for (seq, &line) in lines.iter().enumerate() {
            marks.insert(text.slice(..), line, seq as u64);
        }
        marks
    }

    #[test]
    fn one_bookmark_per_line() {
        let text = Rope::from("a\nb\n");
        let mut marks = marked(&text, &[1, 1]);
        assert_eq!(lines(&marks, &text), [(0, 1)]);
        marks.remove(0);
        assert!(marks.is_empty());
    }

    #[test]
    fn bookmarks_stay_on_their_line_through_inserts_and_deletes_above() {
        let mut text = Rope::from("zero\none\ntwo\nthree\n");
        let mut marks = marked(&text, &[2, 3]);
        // two lines inserted above
        change(&mut text, &mut marks, 0, 0, "new\nnewer\n");
        assert_eq!(lines(&marks, &text), [(0, 4), (1, 5)]);
        // a line inserted right at the start of the bookmarked line goes above it
        let start = text.line_to_char(4);
        change(&mut text, &mut marks, start, start, "above\n");
        assert_eq!(lines(&marks, &text), [(0, 5), (1, 6)]);
        // typing at the start of the line keeps it there
        let start = text.line_to_char(5);
        change(&mut text, &mut marks, start, start, "x");
        assert_eq!(lines(&marks, &text), [(0, 5), (1, 6)]);
        // the first three lines deleted
        let end = text.line_to_char(3);
        change(&mut text, &mut marks, 0, end, "");
        assert_eq!(lines(&marks, &text), [(0, 2), (1, 3)]);
        assert_eq!(text.line(2).to_string(), "xtwo\n");
    }

    #[test]
    fn a_bookmark_goes_with_its_line() {
        let mut text = Rope::from("zero\none\ntwo\nthree");
        let mut marks = marked(&text, &[1, 2, 3]);
        // line 1, with its newline
        let (start, end) = (text.line_to_char(1), text.line_to_char(2));
        change(&mut text, &mut marks, start, end, "");
        assert_eq!(lines(&marks, &text), [(1, 1), (2, 2)]);
        // the last line, without a newline, with the newline before it
        let start = text.line_to_char(2) - 1;
        let end = text.len_chars();
        change(&mut text, &mut marks, start, end, "");
        assert_eq!(lines(&marks, &text), [(1, 1)]);
        // its text alone deleted: the line is still there
        let (start, end) = (text.line_to_char(1), text.len_chars());
        change(&mut text, &mut marks, start, end, "");
        assert_eq!(lines(&marks, &text), [(1, 1)]);
    }

    #[test]
    fn joined_lines_keep_the_older_bookmark() {
        let mut text = Rope::from("one\ntwo\n");
        let mut marks = Marks::default();
        marks.insert(text.slice(..), 1, 5);
        marks.insert(text.slice(..), 0, 9);
        // the newline between them
        change(&mut text, &mut marks, 3, 4, " ");
        assert_eq!(lines(&marks, &text), [(5, 0)]);
    }

    #[test]
    fn the_ring_goes_in_creation_order_and_wraps() {
        let seqs = [2, 5, 9];
        assert_eq!(step(&seqs, None, Direction::Next), Some(2));
        assert_eq!(step(&seqs, None, Direction::Previous), Some(9));
        assert_eq!(step(&seqs, Some(2), Direction::Next), Some(5));
        assert_eq!(step(&seqs, Some(9), Direction::Next), Some(2));
        assert_eq!(step(&seqs, Some(2), Direction::Previous), Some(9));
        // from a removed bookmark: the ones around where it was
        assert_eq!(step(&seqs, Some(6), Direction::Next), Some(9));
        assert_eq!(step(&seqs, Some(6), Direction::Previous), Some(5));
        assert_eq!(step(&[], Some(6), Direction::Next), None);
    }
}
