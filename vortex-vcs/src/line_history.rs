//! Line history: the commits that changed some lines of a file, like `git log -L`.
//!
//! The lines are followed back through history as sets of line ranges. Going from a commit to its
//! parent, each range is mapped through the diff of the file between them ([`map_back`]): lines
//! the diff doesn't touch keep their place, shifted by the lines added and removed above them;
//! a range that ends in a hunk ends at the end of the hunk's old lines, and one that starts in a
//! hunk starts at its start. So the lines a commit rewrote are followed into the lines they
//! replaced, and the lines it added (a hunk without old lines) stop being followed: that commit
//! is where they come from. A commit is listed when its diff touches the ranges.
//!
//! The same mapping takes the lines of a buffer to their place in HEAD, through the buffer's diff
//! against HEAD.
//!
//! Merges follow the rule of `git log -L` ([`merge_rule`]): when the lines are the same in one of
//! the parents (the first such parent, in order), the merge isn't listed and only that parent is
//! followed; otherwise the merge is listed and every parent is followed with the lines mapped to
//! it.

use std::ops::Range;
use std::path::PathBuf;

use crate::staging::LineHunk;
use crate::CommitInfo;

/// Sorts `ranges`, drops the empty ones and merges the ones that overlap or touch, as git
/// normalizes the ranges of `-L`.
pub fn normalize(mut ranges: Vec<Range<u32>>) -> Vec<Range<u32>> {
    ranges.retain(|range| !range.is_empty());
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<u32>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Whether `hunk` changes lines of `range` (of the new side of the diff): it replaces or adds
/// some of them, or removes lines between two of them. Lines removed right above or below the
/// range don't touch it.
pub fn touches(hunk: &LineHunk, range: &Range<u32>) -> bool {
    if hunk.after.is_empty() {
        range.start < hunk.after.start && hunk.after.start < range.end
    } else {
        hunk.after.start < range.end && range.start < hunk.after.end
    }
}

/// The hunks of `hunks` that touch any of `ranges`, in order.
pub fn touching<'a>(
    hunks: &'a [LineHunk],
    ranges: &'a [Range<u32>],
) -> impl Iterator<Item = &'a LineHunk> + 'a {
    hunks
        .iter()
        .filter(|hunk| ranges.iter().any(|range| touches(hunk, range)))
}

/// What a diff does to the lines followed, see [`map_back`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    /// The lines in the old side of the diff, normalized. Empty when all of them were added.
    pub parent: Vec<Range<u32>>,
    /// Whether the diff changes any of them.
    pub touched: bool,
    /// Whether a whole range was added by the diff (it has no lines in the old side): its lines
    /// come from there.
    pub added: bool,
}

impl Mapped {
    /// The lines are the same in the old side.
    pub fn same(ranges: &[Range<u32>]) -> Mapped {
        Mapped {
            parent: ranges.to_vec(),
            touched: false,
            added: false,
        }
    }

    /// The old side doesn't have the file: every line was added.
    pub fn absent(ranges: &[Range<u32>]) -> Mapped {
        Mapped {
            parent: Vec::new(),
            touched: !ranges.is_empty(),
            added: !ranges.is_empty(),
        }
    }
}

#[derive(Clone, Copy)]
enum Edge {
    Start,
    End,
}

/// The line of the old side of the diff `hunks` that `line` of the new side comes from. A line
/// in a hunk maps to the start of the hunk's old lines (`Edge::Start`) or their end
/// (`Edge::End`, exclusive); any other line to itself shifted by the hunks above it. For
/// `Edge::End` the result is one past the line, so `start..end` of a range maps to
/// `map(start, Start)..map(end - 1, End)`.
fn map_line(hunks: &[LineHunk], line: u32, edge: Edge) -> u32 {
    let mut shift: i64 = 0;
    for hunk in hunks {
        if hunk.after.start <= line && line < hunk.after.end {
            return match edge {
                Edge::Start => hunk.before.start,
                Edge::End => hunk.before.end,
            };
        }
        // lines removed right above `line` are above it too
        if hunk.after.end > line {
            break;
        }
        shift += hunk.after.len() as i64 - hunk.before.len() as i64;
    }
    let mapped = (line as i64 - shift).max(0) as u32;
    match edge {
        Edge::Start => mapped,
        Edge::End => mapped + 1,
    }
}

/// Maps `ranges` (normalized, in the new side of the diff `hunks`, which are sorted) to the old
/// side, see the module documentation.
pub fn map_back(ranges: &[Range<u32>], hunks: &[LineHunk]) -> Mapped {
    let mut parent = Vec::with_capacity(ranges.len());
    let mut touched = false;
    let mut added = false;
    for range in ranges.iter().filter(|range| !range.is_empty()) {
        touched |= hunks.iter().any(|hunk| touches(hunk, range));
        let start = map_line(hunks, range.start, Edge::Start);
        let end = map_line(hunks, range.end - 1, Edge::End);
        if start < end {
            parent.push(start..end);
        } else {
            added = true;
        }
    }
    Mapped {
        parent: normalize(parent),
        touched,
        added,
    }
}

/// What to do with a commit, by the rule of `git log -L` for merges, see [`merge_rule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The lines are the same in this parent: the commit isn't listed, and only this parent is
    /// followed, with these lines.
    Follow {
        parent: usize,
        lines: Vec<Range<u32>>,
    },
    /// The commit changed the lines from every parent: it is listed, and each parent is followed
    /// with its lines (none for a parent where they were all added).
    Show {
        parents: Vec<Vec<Range<u32>>>,
        added: bool,
    },
}

/// Decides what to do with a commit of `parents` parents from the diff of the file with each
/// parent, `map(i)` mapping the lines to parent `i` (called in order, and only as long as
/// needed): the first parent where the lines are untouched takes them all, and the commit isn't
/// listed; when every parent's diff touches them, the commit is listed. A commit without a
/// parent added all the lines. It is listed as having added lines when all of a range's lines,
/// for a commit with one parent, or all its lines, for a merge, have no place in its parents.
///
/// This is what `git log -L` does without `--first-parent`: the history of the lines is
/// simplified as `git log <path>` simplifies the history of a file.
pub fn merge_rule(parents: usize, mut map: impl FnMut(usize) -> Mapped) -> Step {
    if parents == 0 {
        return Step::Show {
            parents: Vec::new(),
            added: true,
        };
    }
    let mut mapped = Vec::with_capacity(parents);
    for i in 0..parents {
        let m = map(i);
        if !m.touched {
            return Step::Follow {
                parent: i,
                lines: m.parent,
            };
        }
        mapped.push(m);
    }
    let added = match mapped.as_slice() {
        [only] => only.added,
        all => all.iter().all(|m| m.parent.is_empty()),
    };
    Step::Show {
        parents: mapped.into_iter().map(|m| m.parent).collect(),
        added,
    }
}

/// A commit that changed the lines followed, as the line history lists it.
pub struct LineCommit<'a> {
    pub info: CommitInfo,
    /// The file in this commit (absolute).
    pub path: PathBuf,
    /// The file in the first parent (absolute), where it has another path (a rename), or `None`.
    pub old_path: Option<PathBuf>,
    /// The lines followed in this commit's version of the file.
    pub lines: Vec<Range<u32>>,
    /// Whether the commit added lines followed: where (some of) their history starts.
    pub added: bool,
    /// The hunks of the diff from the first parent that touch the lines.
    pub hunks: Vec<LineHunk>,
    /// The file in the first parent (empty when it doesn't have it) and in this commit.
    pub before: &'a str,
    pub after: &'a str,
}

/// What the walk of the line history reports, in order.
pub enum LineEvent<'a> {
    /// The number of commits looked at so far.
    Searched(usize),
    /// The label of the section of the log the commits that follow are in, as the commit log
    /// labels them.
    Divider(&'a str),
    Commit(Box<LineCommit<'a>>),
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod test {
    use super::*;
    use crate::staging::line_hunks;

    fn hunk(before: Range<u32>, after: Range<u32>) -> LineHunk {
        LineHunk { before, after }
    }

    /// The lines of `ranges` in `after` mapped back to `before`, both given as lines.
    fn back(before: &[&str], after: &[&str], ranges: &[Range<u32>]) -> Mapped {
        let text = |lines: &[&str]| lines.iter().map(|line| format!("{line}\n")).collect();
        let (before, after): (String, String) = (text(before), text(after));
        map_back(ranges, &line_hunks(&before, &after))
    }

    #[test]
    fn normalize_sorts_and_merges() {
        assert_eq!(
            normalize(vec![5..7, 0..2, 1..3, 3..4, 9..9]),
            vec![0..4, 5..7]
        );
        assert_eq!(normalize(vec![]), Vec::<Range<u32>>::new());
    }

    #[test]
    fn untouched_lines_keep_their_place() {
        let m = back(&["a", "b", "c", "d"], &["a", "b", "C", "d"], &[0..2]);
        assert_eq!(m, Mapped::same(&[0..2]));
    }

    #[test]
    fn an_edited_line_maps_to_the_line_it_replaced() {
        let m = back(&["a", "b", "c"], &["a", "B", "c"], &[1..2]);
        assert_eq!(m.parent, vec![1..2]);
        assert!(m.touched);
        assert!(!m.added);
    }

    #[test]
    fn lines_inserted_above_shift_the_range() {
        let before = ["fn a() {}", "fn b() {}", "fn c() {}"];
        let after = [
            "use x;",
            "use y;",
            "",
            "fn a() {}",
            "fn b() {}",
            "fn c() {}",
        ];
        // `fn b` is line 4 now, line 1 before
        let m = back(&before, &after, &[4..5]);
        assert_eq!(m, Mapped::same(&[1..2]));
        // lines removed above shift it the other way
        let m = back(&after, &before, &[1..2]);
        assert_eq!(m, Mapped::same(&[4..5]));
    }

    #[test]
    fn a_split_line_maps_to_the_line_it_was() {
        let before = ["a", "let x = f(1, 2);", "z"];
        let after = ["a", "let x = f(", "    1, 2", ");", "z"];
        let m = back(&before, &after, &[2..3]);
        assert_eq!(m.parent, vec![1..2]);
        assert!(m.touched && !m.added);
    }

    #[test]
    fn joined_lines_map_to_all_the_lines_they_were() {
        let before = ["a", "let x = f(", "    1, 2", ");", "z"];
        let after = ["a", "let x = f(1, 2);", "z"];
        let m = back(&before, &after, &[1..2]);
        assert_eq!(m.parent, vec![1..4]);
        assert!(m.touched && !m.added);
    }

    #[test]
    fn added_lines_come_from_the_commit_that_added_them() {
        let m = back(&["a", "c"], &["a", "b", "c"], &[1..2]);
        assert_eq!(m.parent, Vec::<Range<u32>>::new());
        assert!(m.touched && m.added);
        // a range only partly added keeps its other lines
        let m = back(&["a", "c"], &["a", "b", "c"], &[1..3]);
        assert_eq!(m.parent, vec![1..2]);
        assert!(m.touched && !m.added);
    }

    #[test]
    fn the_whole_file_goes_back_to_its_first_version() {
        // the file was created with "a": the whole of it maps to that line
        let m = back(&["a"], &["a", "b", "c"], &[0..3]);
        assert_eq!(m.parent, vec![0..1]);
        // and the commit that created it added everything
        let m = back(&[], &["a"], &[0..1]);
        assert_eq!(m, Mapped::absent(&[0..1]));
    }

    #[test]
    fn lines_removed_inside_the_range_touch_it_but_not_around_it() {
        let before = ["a", "b", "x", "c", "d"];
        let after = ["a", "b", "c", "d"];
        // `b`, `c`: `x` was between them
        let m = back(&before, &after, &[1..3]);
        assert_eq!(m.parent, vec![1..4]);
        assert!(m.touched);
        // `c` alone, and `b` alone: `x` is right above or below
        assert_eq!(back(&before, &after, &[2..3]), Mapped::same(&[3..4]));
        assert_eq!(back(&before, &after, &[1..2]), Mapped::same(&[1..2]));
    }

    #[test]
    fn several_ranges_are_mapped_each() {
        let before = ["a", "b", "c", "d", "e"];
        let after = ["new", "a", "b", "C", "d", "e"];
        let m = back(&before, &after, &[1..2, 3..4, 5..6]);
        assert_eq!(m.parent, vec![0..1, 2..3, 4..5]);
        assert!(m.touched && !m.added);
        // two ranges that meet in the parent are merged: 0..2 ends in the hunk, so at its old
        // end, 4, where the second one starts
        let m = map_back(&[0..2, 3..4], &[hunk(1..4, 1..3)]);
        assert_eq!(m.parent, vec![0..5]);
    }

    #[test]
    fn touching_hunks() {
        let hunks = [hunk(0..0, 0..2), hunk(4..5, 6..6), hunk(8..9, 9..10)];
        let ranges = [5..8];
        // the removal at 6 is inside 5..8
        assert_eq!(
            touching(&hunks, &ranges).collect::<Vec<_>>(),
            vec![&hunks[1]]
        );
        assert_eq!(touching(&hunks, &[9..10]).count(), 1);
        assert_eq!(touching(&hunks, &[2..6]).count(), 0);
    }

    #[test]
    fn a_merge_that_keeps_one_side_follows_that_side() {
        let side = |touched| Mapped {
            parent: vec![3..4],
            touched,
            added: false,
        };
        let mut asked = Vec::new();
        let step = merge_rule(3, |i| {
            asked.push(i);
            side(i == 0)
        });
        assert_eq!(
            step,
            Step::Follow {
                parent: 1,
                lines: vec![3..4]
            }
        );
        // the parents after it aren't looked at
        assert_eq!(asked, vec![0, 1]);
    }

    #[test]
    fn a_merge_that_changed_the_lines_from_every_side_is_listed() {
        let step = merge_rule(2, |i| Mapped {
            parent: vec![i as u32..i as u32 + 1],
            touched: true,
            added: false,
        });
        assert_eq!(
            step,
            Step::Show {
                parents: vec![vec![0..1], vec![1..2]],
                added: false
            }
        );
        // the lines are new on both sides: the merge added them (an "evil merge")
        let step = merge_rule(2, |_| Mapped::absent(&[0..1]));
        assert_eq!(
            step,
            Step::Show {
                parents: vec![vec![], vec![]],
                added: true
            }
        );
    }

    #[test]
    fn a_commit_with_one_parent() {
        assert_eq!(
            merge_rule(1, |_| Mapped::same(&[0..2])),
            Step::Follow {
                parent: 0,
                lines: vec![0..2]
            }
        );
        let partly_added = Mapped {
            parent: vec![0..1],
            touched: true,
            added: true,
        };
        assert_eq!(
            merge_rule(1, |_| partly_added.clone()),
            Step::Show {
                parents: vec![vec![0..1]],
                added: true
            }
        );
        // a root commit added everything
        assert_eq!(
            merge_rule(0, |_| unreachable!()),
            Step::Show {
                parents: vec![],
                added: true
            }
        );
    }
}
