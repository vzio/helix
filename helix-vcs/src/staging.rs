//! Content-based staging of line hunks.
//!
//! Everything here works on whole texts (HEAD, index and working tree versions of one file) and
//! never on patches: a hunk is staged by taking the lines of the working tree version into the
//! index text where the line diffs line up, and so on.

use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};

/// A hunk of a line diff: the lines it replaces in the old text and the lines that take their
/// place in the new one (line indices; a range is empty for a pure insertion/deletion and then
/// gives the position).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineHunk {
    pub before: Range<u32>,
    pub after: Range<u32>,
}

/// Whether the index has the change of a hunk of HEAD to the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkState {
    Unstaged,
    Staged,
    PartlyStaged,
}

/// The hunks of the line diff from `before` to `after` (the same hunks the diff view shows).
pub fn line_hunks(before: &str, after: &str) -> Vec<LineHunk> {
    let input = InternedInput::new(before, after);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    diff.hunks()
        .map(|hunk| LineHunk {
            before: hunk.before,
            after: hunk.after,
        })
        .collect()
}

/// Whether two line ranges touch: they overlap, or one is empty and lies at or in the other (the
/// ends included). Two empty ranges touch if they are at the same position.
pub fn touches(a: &Range<u32>, b: &Range<u32>) -> bool {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => a.start == b.start,
        (true, false) => b.start <= a.start && a.start <= b.end,
        (false, true) => a.start <= b.start && b.start <= a.end,
        (false, false) => a.start < b.end && b.start < a.end,
    }
}

/// The lines of `text`, with their terminators like imara-diff's tokenizer for `&str` splits them.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The text of `a` where the hunks of the a to b diff for which `select` is true are replaced by
/// the lines of `b`.
fn apply(a: &str, b: &str, hunks: &[LineHunk], select: impl Fn(&LineHunk) -> bool) -> String {
    let a_lines = lines(a);
    let b_lines = lines(b);
    let mut out = String::with_capacity(a.len().max(b.len()));
    let mut pos = 0usize;
    for hunk in hunks.iter().filter(|hunk| select(hunk)) {
        let start = hunk.before.start as usize;
        out.extend(a_lines[pos..start].iter().copied());
        out.extend(
            b_lines[hunk.after.start as usize..hunk.after.end as usize]
                .iter()
                .copied(),
        );
        pos = hunk.before.end as usize;
    }
    out.extend(a_lines[pos..].iter().copied());
    out
}

/// For each hunk of `line_hunks(head, work)`: whether the index has its change.
pub fn hunk_states(head: &str, index: &str, work: &str, hunks: &[LineHunk]) -> Vec<HunkState> {
    let staged = line_hunks(head, index);
    let unstaged = line_hunks(index, work);
    hunks
        .iter()
        .map(|hunk| {
            let has_staged = staged.iter().any(|s| touches(&s.before, &hunk.before));
            let has_unstaged = unstaged.iter().any(|u| touches(&u.after, &hunk.after));
            match (has_staged, has_unstaged) {
                (true, true) => HunkState::PartlyStaged,
                (true, false) => HunkState::Staged,
                (false, _) => HunkState::Unstaged,
            }
        })
        .collect()
}

/// The new index text with the change of `hunk` (a hunk of head to work) staged.
pub fn stage_hunk(index: &str, work: &str, hunk: &LineHunk) -> String {
    let hunks = line_hunks(index, work);
    apply(index, work, &hunks, |h| touches(&h.after, &hunk.after))
}

/// The new index text with the change of `hunk` (head to work) unstaged (back to HEAD there).
pub fn unstage_hunk(head: &str, index: &str, hunk: &LineHunk) -> String {
    let hunks = line_hunks(index, head);
    apply(index, head, &hunks, |h| touches(&h.after, &hunk.before))
}

/// The new working tree text with the unstaged part of `hunk` (head to work) reverted to the index.
pub fn revert_hunk(index: &str, work: &str, hunk: &LineHunk) -> String {
    let hunks = line_hunks(work, index);
    apply(work, index, &hunks, |h| touches(&h.before, &hunk.after))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "a\nb\nc\nd\ne\nf\ng\nh\n";
    const WORK: &str = "a\nB\nc\nd\ne\nf\nG\nh\n";

    #[test]
    fn tokenizer_matches_split_inclusive() {
        for text in [
            "a\nb\n",
            "a\nb",
            "a\r\nb\r\n",
            "a\rb\n",
            "",
            "\n\n",
            "x\r\ny",
        ] {
            let input = InternedInput::new(text, "");
            assert_eq!(input.before.len(), lines(text).len(), "{text:?}");
            for (token, line) in input.before.iter().zip(lines(text)) {
                assert_eq!(input.interner[*token], line);
            }
        }
    }

    #[test]
    fn stage_one_of_two() {
        let hunks = line_hunks(HEAD, WORK);
        assert_eq!(hunks.len(), 2);
        let index = stage_hunk(HEAD, WORK, &hunks[0]);
        assert_eq!(index, "a\nB\nc\nd\ne\nf\ng\nh\n");
        assert_eq!(
            hunk_states(HEAD, &index, WORK, &hunks),
            [HunkState::Staged, HunkState::Unstaged]
        );
        // unstage is the inverse
        assert_eq!(unstage_hunk(HEAD, &index, &hunks[0]), HEAD);
    }

    #[test]
    fn unstage_leaves_other_staged() {
        let hunks = line_hunks(HEAD, WORK);
        let index = stage_hunk(HEAD, WORK, &hunks[0]);
        let index2 = stage_hunk(&index, WORK, &hunks[1]);
        assert_eq!(index2, WORK);
        assert_eq!(unstage_hunk(HEAD, &index2, &hunks[1]), index);
    }

    #[test]
    fn revert_unstaged() {
        let hunks = line_hunks(HEAD, WORK);
        assert_eq!(
            revert_hunk(HEAD, WORK, &hunks[1]),
            "a\nB\nc\nd\ne\nf\ng\nh\n"
        );
        assert_eq!(
            revert_hunk(HEAD, WORK, &hunks[0]),
            "a\nb\nc\nd\ne\nf\nG\nh\n"
        );
    }

    #[test]
    fn revert_staged_is_noop() {
        let hunks = line_hunks(HEAD, WORK);
        let index = stage_hunk(HEAD, WORK, &hunks[0]);
        assert_eq!(revert_hunk(&index, WORK, &hunks[0]), WORK);
    }

    #[test]
    fn partly_staged() {
        let head = "a\nb\nc\nd\n";
        let work = "a\nB\nC\nd\n";
        let index = "a\nB\nc\nd\n";
        let hunks = line_hunks(head, work);
        assert_eq!(hunks.len(), 1);
        assert_eq!(
            hunk_states(head, index, work, &hunks),
            [HunkState::PartlyStaged]
        );
        // staging completes it
        assert_eq!(stage_hunk(index, work, &hunks[0]), work);
        // unstaging goes back to HEAD
        assert_eq!(unstage_hunk(head, index, &hunks[0]), head);
        // reverting restores the index version in the working tree
        assert_eq!(revert_hunk(index, work, &hunks[0]), index);
    }

    #[test]
    fn new_file() {
        let work = "x\ny\n";
        let hunks = line_hunks("", work);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunk_states("", "", work, &hunks), [HunkState::Unstaged]);
        let index = stage_hunk("", work, &hunks[0]);
        assert_eq!(index, work);
        assert_eq!(hunk_states("", &index, work, &hunks), [HunkState::Staged]);
        assert_eq!(unstage_hunk("", &index, &hunks[0]), "");
        assert_eq!(revert_hunk("", work, &hunks[0]), "");
    }

    #[test]
    fn insertion_at_end() {
        let head = "a\nb\n";
        let work = "a\nb\nc\n";
        let hunks = line_hunks(head, work);
        assert_eq!(
            hunks,
            [LineHunk {
                before: 2..2,
                after: 2..3
            }]
        );
        let index = stage_hunk(head, work, &hunks[0]);
        assert_eq!(index, work);
        assert_eq!(unstage_hunk(head, &index, &hunks[0]), head);
        assert_eq!(revert_hunk(head, work, &hunks[0]), head);
    }

    #[test]
    fn deletion_of_all_lines() {
        let head = "a\nb\n";
        let work = "";
        let hunks = line_hunks(head, work);
        assert_eq!(hunks.len(), 1);
        let index = stage_hunk(head, work, &hunks[0]);
        assert_eq!(index, "");
        assert_eq!(hunk_states(head, &index, work, &hunks), [HunkState::Staged]);
        assert_eq!(unstage_hunk(head, &index, &hunks[0]), head);
        assert_eq!(revert_hunk(head, work, &hunks[0]), head);
    }

    #[test]
    fn last_line_without_newline() {
        let head = "a\nb\nc";
        let work = "a\nb\nc\nd";
        let hunks = line_hunks(head, work);
        let index = stage_hunk(head, work, &hunks[0]);
        assert_eq!(index, work);
        assert_eq!(unstage_hunk(head, &index, &hunks[0]), head);
        // unselected regions round trip byte for byte
        let work2 = "A\nb\nc";
        let hunks2 = line_hunks(head, work2);
        assert_eq!(stage_hunk(head, work2, &hunks2[0]), work2);
        let work3 = "a\nb\nC";
        let hunks3 = line_hunks(head, work3);
        assert_eq!(stage_hunk(head, work3, &hunks3[0]), work3);
    }

    #[test]
    fn crlf_preserved() {
        let head = "a\r\nb\r\nc\r\nd\r\ne\r\n";
        let work = "A\r\nb\r\nc\r\nd\r\nE\r\n";
        let hunks = line_hunks(head, work);
        assert_eq!(hunks.len(), 2);
        let index = stage_hunk(head, work, &hunks[1]);
        assert_eq!(index, "a\r\nb\r\nc\r\nd\r\nE\r\n");
        assert_eq!(unstage_hunk(head, &index, &hunks[1]), head);
        assert_eq!(revert_hunk(&index, work, &hunks[0]), index);
    }
}
