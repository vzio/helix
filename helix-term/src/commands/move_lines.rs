//! Moving the lines of the selections up and down (fork feature).
//!
//! `move_lines` is a pure function from a text and a selection to a transaction, the commands
//! only apply its result. Nothing here touches registers or the clipboard.

use helix_core::{
    line_ending::line_end_char_index, movement::Direction, Range, Rope, RopeSlice, Selection,
    SmallVec, Tendril, Transaction,
};

use super::Context;

/// Move the lines of the selections up by `count` lines, in one undo step.
pub fn move_lines_up(cx: &mut Context) {
    move_lines_impl(cx, Direction::Backward)
}

/// Move the lines of the selections down by `count` lines, in one undo step.
pub fn move_lines_down(cx: &mut Context) {
    move_lines_impl(cx, Direction::Forward)
}

fn move_lines_impl(cx: &mut Context, direction: Direction) {
    let count = cx.count();
    let (view, doc) = current!(cx.editor);
    let Some(transaction) = move_lines(doc.text(), doc.selection(view.id), direction, count) else {
        // at the edge of the file, or nothing to move: no change, no history entry
        return;
    };
    doc.apply(&transaction, view.id);
}

/// The transaction that moves every line touched by a range of `selection` by `count` lines,
/// `Backward` is up and `Forward` is down, with the selection on the moved text.
///
/// - The line ranges of the selection that overlap or are adjacent are one block, which moves
///   as a whole. A block moving down swaps places with the line below it, and `count` lines
///   are `count` of these swaps. All the blocks move by the same number of lines, so the lines
///   between them stay between them, in their order.
/// - If the block nearest the edge has less room than `count`, everything moves by that room.
///   `None` if there is no room at all (or no line to move).
/// - The empty line after a final line ending is not a line: it is never moved or swapped with.
/// - Only the lines that the blocks jump over are edited, as a deletion and an insertion. The
///   blocks themselves are not touched, so the selections of other views follow them.
/// - A final line without a line ending stays the final line: when it moves, the line ending is
///   passed on to the line that takes its place (a copy of the one above it is synthesized for
///   the moved line, so a CRLF file stays CRLF).
pub fn move_lines(
    doc: &Rope,
    selection: &Selection,
    direction: Direction,
    count: usize,
) -> Option<Transaction> {
    let text = doc.slice(..);
    let len = text.len_chars();
    let len_lines = text.len_lines();
    let has_final_ending = len_lines > 1 && text.line(len_lines - 1).len_chars() == 0;
    // the number of lines, and the index of the last one
    let lines = len_lines - has_final_ending as usize;
    let last = lines - 1;

    let blocks = blocks(text, selection, lines);
    let (first_block, last_block) = (*blocks.first()?, *blocks.last()?);
    let up = direction == Direction::Backward;
    let room = if up {
        first_block.0
    } else {
        last - last_block.1
    };
    let shift = count.min(room);
    if shift == 0 {
        return None;
    }

    // Old line `i` of a block is line `i -/+ shift` afterwards. Lines `lo..=hi` are the ones
    // that can change place; `at[p - lo]` is the old line that is at line `p` afterwards.
    let (lo, hi) = if up {
        (first_block.0 - shift, last_block.1)
    } else {
        (first_block.0, last_block.1 + shift)
    };
    let mut at = vec![usize::MAX; hi - lo + 1];
    let mut in_block = vec![false; hi - lo + 1];
    for &(start, end) in &blocks {
        for i in start..=end {
            in_block[i - lo] = true;
            at[if up { i - shift } else { i + shift } - lo] = i;
        }
    }
    // the lines that aren't in a block fill what is left, in their order
    let mut others = (lo..=hi).filter(|i| !in_block[i - lo]);
    for slot in at.iter_mut().filter(|slot| **slot == usize::MAX) {
        *slot = others.next()?;
    }
    // a line moves if a block jumps over it
    let is_mover = |p: usize| at[p - lo] != p && !in_block[at[p - lo] - lo];

    let start = |line: usize| text.line_to_char(line);
    let end = |line: usize| text.line_to_char(line + 1);
    let content_end = |line: usize| line_end_char_index(&text, line);

    // Whether the final line, which has no line ending, changes place. If it does, the line that
    // is the final line afterwards must lose its line ending, and the final line gets one.
    let moves_last = !has_final_ending && hi == last;
    let new_last = at[hi - lo];
    // (the ending of the line above the last: it has one as it isn't the last)
    let synthesized = if moves_last {
        text.slice(content_end(last - 1)..start(last)).to_string()
    } else {
        String::new()
    };

    // Where the lines that moved are inserted: before the first line after them that didn't move
    // when going down, after the last before them when going up. `p` is their new line.
    let mut insertions: Vec<(usize, usize, usize)> = Vec::new(); // (offset, old line, new line)
    if up {
        let mut before = None;
        for p in lo..=hi {
            if is_mover(p) {
                let offset = end(before?);
                insertions.push((offset, at[p - lo], p));
            } else {
                before = Some(at[p - lo]);
            }
        }
    } else {
        let mut after = None;
        for p in (lo..=hi).rev() {
            if is_mover(p) {
                let offset = start(after?);
                insertions.push((offset, at[p - lo], p));
            } else {
                after = Some(at[p - lo]);
            }
        }
        insertions.reverse();
    }

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    // the final line gets its ending in front of the first line inserted after it
    let mut ending_pending = moves_last && up;
    for &(offset, line, _) in &insertions {
        edits.push((start(line), end(line), String::new()));
        let mut inserted = text.slice(start(line)..end(line)).to_string();
        if moves_last && line == last {
            // going down: the final line has no ending, give it one
            inserted.push_str(&synthesized);
        } else if moves_last && line == new_last {
            // going up: the line that becomes the final line loses its ending
            inserted = text.slice(start(line)..content_end(line)).to_string();
        }
        if ending_pending && offset == len {
            inserted.insert_str(0, &synthesized);
            ending_pending = false;
        }
        edits.push((offset, offset, inserted));
    }
    if moves_last {
        // the line that is the final line afterwards loses its ending (going up it is one of
        // the lines inserted, whose deletion contains this one)
        edits.push((content_end(new_last), end(new_last), String::new()));
    }

    // Merge the edits that touch into one. (Inserts at the same offset keep their order.)
    edits.sort_by_key(|&(from, to, _)| (from, to));
    let mut merged: Vec<(usize, usize, String)> = Vec::with_capacity(edits.len());
    for (from, to, inserted) in edits {
        match merged.last_mut() {
            Some(last) if from <= last.1 => {
                last.1 = last.1.max(to);
                last.2.push_str(&inserted);
            }
            _ => merged.push((from, to, inserted)),
        }
    }

    // where the blocks start afterwards, to move the selection with them
    let mut new_starts = Vec::with_capacity(blocks.len());
    let mut offset = start(lo);
    for p in lo..=hi {
        let line = at[p - lo];
        if blocks.get(new_starts.len()).is_some_and(|b| b.0 == line) {
            new_starts.push(offset);
        }
        let mut line_len = end(line) - start(line);
        if moves_last && line == last {
            line_len += synthesized.chars().count();
        }
        if moves_last && line == new_last {
            line_len -= end(line) - content_end(line);
        }
        offset += line_len;
    }
    let new_len = merged.iter().fold(len, |new_len, (from, to, inserted)| {
        new_len + inserted.chars().count() - (to - from)
    });
    let ranges: SmallVec<[Range; 1]> = selection
        .iter()
        .map(|range| {
            let line = text.char_to_line(range.from());
            let block = blocks.partition_point(|&(_, end)| end < line);
            match blocks.get(block) {
                Some(&(first, _)) if first <= line => {
                    let delta = new_starts[block] as isize - start(first) as isize;
                    let map = |pos: usize| ((pos as isize + delta) as usize).min(new_len);
                    Range {
                        anchor: map(range.anchor),
                        head: map(range.head),
                        old_visual_position: range.old_visual_position,
                    }
                }
                // (on the empty line after the last line ending)
                _ => *range,
            }
        })
        .collect();

    let changes = merged.into_iter().map(|(from, to, inserted)| {
        let inserted = (!inserted.is_empty()).then(|| Tendril::from(inserted));
        (from, to, inserted)
    });
    Some(
        Transaction::change(doc, changes)
            .with_selection(Selection::new(ranges, selection.primary_index())),
    )
}

/// The blocks of lines (first and last line, inclusive) that the selection touches, in order.
/// The line ranges that overlap or are adjacent are one block. Lines past the last line, the
/// empty one after a final line ending, are not part of a block.
fn blocks(text: RopeSlice, selection: &Selection, lines: usize) -> Vec<(usize, usize)> {
    selection
        .line_ranges(text)
        .filter(|&(start, _)| start < lines)
        .map(|(start, end)| (start, end.min(lines - 1)))
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use helix_core::test;

    use Direction::{Backward as Up, Forward as Down};

    /// The text and selection (in the `#[|` markup) after moving, or `None` for no change.
    fn moved(input: &str, direction: Direction, count: usize) -> Option<String> {
        let (text, selection) = test::print(input);
        let mut doc = Rope::from(text);
        let transaction = move_lines(&doc, &selection, direction, count)?;
        assert!(transaction.apply(&mut doc));
        let selection = transaction.selection().unwrap();
        Some(test::plain(doc.slice(..), selection))
    }

    #[track_caller]
    fn check(input: &str, direction: Direction, count: usize, expected: &str) {
        assert_eq!(moved(input, direction, count).as_deref(), Some(expected));
    }

    #[track_caller]
    fn check_unchanged(input: &str, direction: Direction, count: usize) {
        assert_eq!(moved(input, direction, count), None);
    }

    #[test]
    fn one_line() {
        check("a\nb#[|c]#d\ne\n", Up, 1, "b#[|c]#d\na\ne\n");
        check("a\nb#[|c]#d\ne\n", Down, 1, "a\ne\nb#[|c]#d\n");
        // the column and the direction are kept
        check("a\n#[cd|]#ef\nb\n", Up, 1, "#[cd|]#ef\na\nb\n");
        check("a\n#[|cd]#ef\nb\n", Down, 1, "a\nb\n#[|cd]#ef\n");
    }

    #[test]
    fn several_lines() {
        check(
            "a\nb#[c\nd\nef|]#g\nh\ni\n",
            Up,
            1,
            "b#[c\nd\nef|]#g\na\nh\ni\n",
        );
        check(
            "a\nb#[c\nd\nef|]#g\nh\ni\n",
            Down,
            1,
            "a\nh\nb#[c\nd\nef|]#g\ni\n",
        );
        // a whole line, selected as `x` does, ends at the start of the next line
        check("a\n#[b\n|]#c\nd\n", Down, 1, "a\nc\n#[b\n|]#d\n");
        check("a\nb\n#[c\n|]#d\n", Up, 1, "a\n#[c\n|]#b\nd\n");
    }

    #[test]
    fn counts() {
        check("#[|a]#\nb\nc\nd\ne\n", Down, 3, "b\nc\nd\n#[|a]#\ne\n");
        check("a\nb\nc\nd\n#[|e]#\n", Up, 3, "a\n#[|e]#\nb\nc\nd\n");
        check("#[a\nb|]#\nc\nd\ne\n", Down, 2, "c\nd\n#[a\nb|]#\ne\n");
    }

    #[test]
    fn clamped() {
        check("#[|a]#\nb\nc\n", Down, 10, "b\nc\n#[|a]#\n");
        check("a\nb\n#[|c]#\n", Up, 10, "#[|c]#\na\nb\n");
        check("a\n#[|b]#\nc\n", Up, 3, "#[|b]#\na\nc\n");
        // all the blocks move by what the nearest one can
        check(
            "a\n#[b|]#\nc\nd\n#(e|)#\nf\ng\n",
            Down,
            10,
            "a\nc\nd\n#[b|]#\nf\ng\n#(e|)#\n",
        );
    }

    #[test]
    fn at_the_edges() {
        check_unchanged("#[|a]#\nb\n", Up, 1);
        check_unchanged("a\n#[|b]#\n", Down, 1);
        check_unchanged("#[a\nb|]#\n", Up, 5);
        check_unchanged("#[a\nb|]#\n", Down, 5);
        // (the empty line after the last line ending is no line)
        check_unchanged("a\nb\n#[|]#", Down, 1);
        check_unchanged("a\nb#[|c]#\n", Down, 1);
        check_unchanged("a\n#[b\n|]#", Down, 1);
        // one line, an empty file
        check_unchanged("#[|a]#\n", Up, 1);
        check_unchanged("#[|a]#\n", Down, 1);
        check_unchanged("#[|a]#", Down, 1);
        check_unchanged("#[|]#", Up, 1);
        check_unchanged("#[|]#", Down, 1);
        check_unchanged("#[|]#\n", Down, 1);
        // the first block is the last one and the second can't move
        check_unchanged("a\n#[|b]#\n#(|c)#\n", Down, 1);
    }

    #[test]
    fn a_cursor_after_the_last_line_ending_stays() {
        check("a\n#[|b]#\nc\n#(|)#", Up, 1, "#[|b]#\na\nc\n#(|)#");
        check("a\n#[|b]#\nc\n#(|)#", Down, 1, "a\nc\n#[|b]#\n#(|)#");
        // select all
        check_unchanged("#[a\nb\nc\n|]#", Down, 1);
        check_unchanged("#[a\nb\nc\n|]#", Up, 1);
    }

    #[test]
    fn last_line_without_a_line_ending() {
        check("a\nb\n#[|c]#", Up, 1, "a\n#[|c]#\nb");
        check("a\n#[|b]#\nc", Down, 1, "a\nc\n#[|b]#");
        check("a\n#[b\nc|]#", Up, 1, "#[b\nc|]#\na");
        check("a\nb\n#[c|]#", Up, 1, "a\n#[c|]#\nb");
        check("a#[|]#\nb\nc", Down, 2, "b\nc\na#[|]#");
        check("#[|a]#\nb\nc", Down, 1, "b\n#[|a]#\nc");
        check("a\nb\n#[|c]#", Up, 2, "#[|c]#\na\nb");
        // a cursor at the very end
        check("a\nbc#[|]#", Up, 1, "bc#[|]#\na");
        check("a#[|]#\nbc", Down, 1, "bc\na#[|]#");
        // a selection that includes the line ending of the line above
        check("a\n#[b\n|]#c", Down, 1, "a\nc\n#[b|]#");
    }

    #[test]
    fn crlf() {
        check("a\r\nb#[|c]#\r\nd\r\n", Up, 1, "b#[|c]#\r\na\r\nd\r\n");
        check("a\r\nb#[|c]#\r\nd\r\n", Down, 1, "a\r\nd\r\nb#[|c]#\r\n");
        check("a\r\nb\r\n#[|c]#", Up, 1, "a\r\n#[|c]#\r\nb");
        check("a\r\n#[|b]#\r\nc", Down, 1, "a\r\nc\r\n#[|b]#");
        check("#[a\r\nb|]#\r\nc\r\nd", Down, 2, "c\r\nd\r\n#[a\r\nb|]#");
        check_unchanged("a\r\n#[|b]#\r\n", Down, 1);
    }

    #[test]
    fn blocks() {
        // two blocks, the lines between them stay between them
        check(
            "#[|a]#\nb\nc\n#(|d)#\ne\nf\n",
            Down,
            1,
            "b\n#[|a]#\nc\ne\n#(|d)#\nf\n",
        );
        check(
            "a\n#[|b]#\nc\nd\n#(|e)#\nf\n",
            Up,
            1,
            "#[|b]#\na\nc\n#(|e)#\nd\nf\n",
        );
        // a gap of one line
        check(
            "#[|a]#\nb\n#(|c)#\nd\ne\n",
            Down,
            2,
            "b\nd\n#[|a]#\ne\n#(|c)#\n",
        );
        // adjacent lines are one block, which moves as a whole
        check("a\n#[|b]#\n#(|c)#\nd\n", Down, 1, "a\nd\n#[|b]#\n#(|c)#\n");
        check("a\n#[|b]#\n#(|c)#\nd\n", Up, 1, "#[|b]#\n#(|c)#\na\nd\n");
        // overlapping line ranges, and two cursors on a line
        check(
            "a\n#[b\nc|]#\n#(c|)#d\ne\n",
            Down,
            1,
            "a\ne\n#[b\nc|]#\n#(c|)#d\n",
        );
        check("#[|a]#b#(|c)#\nd\ne\n", Down, 1, "d\n#[|a]#b#(|c)#\ne\n");
        check("x\na#[|b]#c#(|d)#e\ny\n", Up, 1, "a#[|b]#c#(|d)#e\nx\ny\n");
        // many cursors, one block
        check(
            "a\n#[b|]#\n#(c|)#\n#(d|)#\ne\n",
            Up,
            1,
            "#[b|]#\n#(c|)#\n#(d|)#\na\ne\n",
        );
    }

    #[test]
    fn the_text_of_the_blocks_is_not_edited() {
        // so the selections of other views follow it
        let doc = Rope::from("a\nbcd\ne\n");
        let (_, selection) = test::print("a\n#[|b]#cd\ne\n");
        let transaction = move_lines(&doc, &selection, Down, 1).unwrap();
        let changes = transaction.changes();
        assert_eq!(changes.map_pos(3, helix_core::Assoc::After), 5);
        let transaction = move_lines(&doc, &selection, Up, 1).unwrap();
        assert_eq!(
            transaction.changes().map_pos(3, helix_core::Assoc::After),
            1
        );
    }

    #[test]
    fn the_primary_selection_stays() {
        let (text, selection) = test::print("#[|a]#\nb\n#(|c)#\nd\n");
        let mut selection = selection;
        selection.set_primary_index(1);
        let doc = Rope::from(text);
        let transaction = move_lines(&doc, &selection, Down, 1).unwrap();
        assert_eq!(transaction.selection().unwrap().primary_index(), 1);
    }

    // The same, simply, with a list of lines.
    struct Model {
        lines: Vec<String>,
        ending: &'static str,
        final_ending: bool,
    }

    impl Model {
        fn text(&self) -> String {
            let mut text = self.lines.join(self.ending);
            if self.final_ending {
                text.push_str(self.ending);
            }
            text
        }

        /// `count` single moves of the blocks, each swapping with the line next to it.
        fn moved(&self, blocks: &[(usize, usize)], down: bool, count: usize) -> (Self, usize) {
            let mut lines = self.lines.clone();
            let mut blocks = blocks.to_vec();
            let mut done = 0;
            for _ in 0..count {
                if down && blocks.last().unwrap().1 + 1 >= lines.len() || !down && blocks[0].0 == 0
                {
                    break;
                }
                if down {
                    for block in blocks.iter_mut().rev() {
                        let line = lines.remove(block.1 + 1);
                        lines.insert(block.0, line);
                        *block = (block.0 + 1, block.1 + 1);
                    }
                } else {
                    for block in blocks.iter_mut() {
                        let line = lines.remove(block.0 - 1);
                        lines.insert(block.1, line);
                        *block = (block.0 - 1, block.1 - 1);
                    }
                }
                done += 1;
            }
            let model = Model {
                lines,
                ending: self.ending,
                final_ending: self.final_ending,
            };
            (model, done)
        }
    }

    fn random(state: &mut u64, below: usize) -> usize {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*state >> 33) as usize % below
    }

    /// Random texts and selections, compared with the moves of the model.
    #[test]
    fn like_single_moves_of_a_list_of_lines() {
        let mut state = 7;
        let mut moves = 0;
        for _ in 0..20_000 {
            let ending = ["\n", "\r\n"][random(&mut state, 2)];
            let n = 1 + random(&mut state, 7);
            let lines: Vec<String> = (0..n)
                .map(|_| "ab"[..random(&mut state, 3).min(2)].to_string())
                .collect();
            let mut lines = lines;
            let mut final_ending = random(&mut state, 2) == 0;
            if lines.len() > 1 && lines.last().unwrap().is_empty() && !final_ending {
                // (that's the same text as one line less and a line ending at the end)
                lines.pop();
                final_ending = true;
            }
            let model = Model {
                lines,
                ending,
                final_ending,
            };
            let doc = Rope::from(model.text());
            let text = doc.slice(..);

            // positions: (line, column) with the column in the line, also on the empty last line
            let mut positions: Vec<usize> = (0..1 + random(&mut state, 5))
                .map(|_| {
                    let line = random(&mut state, text.len_lines());
                    let start = text.line_to_char(line);
                    start + random(&mut state, 1 + line_end_char_index(&text, line) - start)
                })
                .collect();
            positions.sort_unstable();
            positions.dedup();
            let mut ranges: SmallVec<[Range; 1]> = SmallVec::new();
            let mut i = 0;
            while i < positions.len() {
                if i + 1 < positions.len() && random(&mut state, 2) == 0 {
                    let (a, b) = (positions[i], positions[i + 1]);
                    ranges.push(if random(&mut state, 2) == 0 {
                        Range::new(a, b)
                    } else {
                        Range::new(b, a)
                    });
                    i += 2;
                } else {
                    ranges.push(Range::point(positions[i]));
                    i += 1;
                }
            }
            let selection = Selection::new(ranges, 0);
            let down = random(&mut state, 2) == 0;
            let count = 1 + random(&mut state, 4);
            let direction = if down { Down } else { Up };
            let context = format!(
                "{:?} {} {count} {:?}",
                model.text(),
                if down { "down" } else { "up" },
                selection
            );

            let real_lines = model.lines.len();
            let blocks: Vec<(usize, usize)> = selection
                .line_ranges(text)
                .filter(|&(start, _)| start < real_lines)
                .map(|(start, end)| (start, end.min(real_lines - 1)))
                .collect();
            let result = move_lines(&doc, &selection, direction, count);
            if blocks.is_empty() {
                assert!(result.is_none(), "{context}");
                continue;
            }
            let (expected, shift) = model.moved(&blocks, down, count);
            if shift == 0 {
                assert!(result.is_none(), "{context}");
                continue;
            }
            let transaction = result.expect(&context);
            moves += 1;
            let mut new_doc = doc.clone();
            assert!(transaction.apply(&mut new_doc), "{context}");
            assert_eq!(new_doc.to_string(), expected.text(), "{context}");

            // every range is on the same text, on a line `shift` lines away
            let new_selection = transaction.selection().unwrap();
            assert_eq!(new_selection.len(), selection.len(), "{context}");
            let new_text = new_doc.slice(..);
            for (old, new) in selection.iter().zip(new_selection.iter()) {
                let old_fragment = text.slice(old.from()..old.to()).to_string();
                let new_fragment = new_text.slice(new.from()..new.to()).to_string();
                if new.to() == new_text.len_chars() && !model.final_ending {
                    // (the final line ending goes if the last line moves)
                    assert!(old_fragment.starts_with(&new_fragment), "{context}");
                } else {
                    assert_eq!(old.direction(), new.direction(), "{context}");
                    assert_eq!(old_fragment, new_fragment, "{context}");
                }
                let (old_line, new_line) = (
                    text.char_to_line(old.from()),
                    new_text.char_to_line(new.from()),
                );
                if old_line >= real_lines {
                    assert_eq!((old.from(), old.to()), (new.from(), new.to()), "{context}");
                    continue;
                }
                let moved_by = if down {
                    shift as isize
                } else {
                    -(shift as isize)
                };
                assert_eq!(new_line as isize, old_line as isize + moved_by, "{context}");
                assert_eq!(
                    new.from() - new_text.line_to_char(new_line),
                    old.from() - text.line_to_char(old_line),
                    "{context}"
                );
            }
        }
        assert!(moves > 5_000, "{moves} moves");
    }
}
