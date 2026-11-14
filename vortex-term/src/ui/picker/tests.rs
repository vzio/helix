use nucleo::pattern::{CaseMatching, Normalization};

use super::*;

/// A picker of strings in which the ones that start with `#` are dividers.
fn picker(items: &[&str]) -> Picker<String, ()> {
    let columns = [Column::new("text", |item: &String, _: &()| {
        if item.starts_with('#') {
            "".into()
        } else {
            item.as_str().into()
        }
    })];
    let mut picker = Picker::new(
        columns,
        0,
        items.iter().map(|item| item.to_string()),
        (),
        |_, _, _| {},
    )
    .with_dividers(|item, _| item.strip_prefix('#').map(Into::into));
    settle(&mut picker);
    picker
}

/// What rendering does before drawing: let the matcher finish and update the dividers.
fn settle(picker: &mut Picker<String, ()>) {
    while picker.matcher.tick(10).running {}
    picker.update_order();
    picker.update_dividers();
}

fn query(picker: &mut Picker<String, ()>, query: &str) {
    picker
        .matcher
        .pattern
        .reparse(0, query, CaseMatching::Smart, Normalization::Smart, false);
    picker.cursor = 0;
    settle(picker);
}

fn at(picker: &Picker<String, ()>) -> Option<&str> {
    picker.selection().map(String::as_str)
}

const LOG: [&str; 7] = ["#new", "a", "b", "#old", "c", "d", "e"];

#[tokio::test]
async fn the_cursor_starts_on_the_first_entry_when_the_first_row_is_a_divider() {
    let picker = picker(&LOG);
    assert_eq!(picker.cursor_index(), 1);
    assert_eq!(at(&picker), Some("a"));
}

#[tokio::test]
async fn movements_skip_dividers() {
    let mut picker = picker(&LOG);
    let mut seen = Vec::new();
    for _ in 0..6 {
        picker.move_by(1, Direction::Forward);
        seen.push(at(&picker).unwrap().to_string());
    }
    // the last entry is followed by the first, not by the first divider
    assert_eq!(seen, ["b", "c", "d", "e", "a", "b"]);

    let mut seen = Vec::new();
    for _ in 0..4 {
        picker.move_by(1, Direction::Backward);
        seen.push(at(&picker).unwrap().to_string());
    }
    assert_eq!(seen, ["a", "e", "d", "c"]);
    picker.move_by(1, Direction::Backward);
    assert_eq!(at(&picker), Some("b"));
}

#[tokio::test]
async fn home_end_and_pages_skip_dividers() {
    let mut picker = picker(&LOG);
    picker.to_end();
    assert_eq!(at(&picker), Some("e"));
    picker.to_start();
    assert_eq!(at(&picker), Some("a"));

    // a page lands on the divider (index 3) or past the ends
    picker.completion_height = 3;
    picker.page_down();
    assert_eq!(picker.cursor_index(), 4);
    assert_eq!(at(&picker), Some("c"));
    picker.page_up();
    assert_eq!(picker.cursor_index(), 1);
    picker.completion_height = 2;
    picker.page_down();
    assert_eq!(at(&picker), Some("c"));
    picker.completion_height = 4;
    picker.page_up();
    // index 0 is a divider: going backwards wraps to the last entry
    assert_eq!(at(&picker), Some("e"));
}

#[tokio::test]
async fn a_divider_is_never_selected() {
    let mut picker = picker(&LOG);
    for index in 0..10 {
        picker.set_cursor_index(index);
        let selected = at(&picker).unwrap_or_else(|| panic!("an entry at {index}"));
        assert!(!selected.starts_with('#'), "{selected}");
    }
    // past the end is the last entry
    assert_eq!(at(&picker), Some("e"));
    picker.set_cursor_index(3);
    assert_eq!(picker.cursor_index(), 4);
}

#[tokio::test]
async fn a_divider_at_the_end_is_skipped_to_the_start() {
    let mut picker = picker(&["a", "b", "#last"]);
    picker.move_by(1, Direction::Forward);
    assert_eq!(at(&picker), Some("b"));
    picker.move_by(1, Direction::Forward);
    assert_eq!(at(&picker), Some("a"));
    picker.move_by(1, Direction::Backward);
    assert_eq!(at(&picker), Some("b"));
    picker.set_cursor_index(2);
    assert_eq!(at(&picker), Some("a"));
}

#[tokio::test]
async fn only_dividers_leave_nothing_selected() {
    let mut picker = picker(&["#a", "#b"]);
    assert_eq!(at(&picker), None);
    picker.move_by(1, Direction::Forward);
    picker.to_end();
    picker.to_start();
    assert_eq!(at(&picker), None);
    assert_eq!(picker.counts(), (0, 0));
}

#[tokio::test]
async fn filtering_hides_dividers() {
    let mut picker = picker(&LOG);
    assert_eq!(picker.matcher.snapshot().matched_item_count(), 7);
    query(&mut picker, "c");
    let matched: Vec<_> = picker
        .matcher
        .snapshot()
        .matched_items(..)
        .map(|item| item.data.clone())
        .collect();
    assert_eq!(matched, ["c"]);
    assert_eq!(at(&picker), Some("c"));

    // and they are back when the query is
    query(&mut picker, "");
    assert_eq!(picker.matcher.snapshot().matched_item_count(), 7);
    assert_eq!(at(&picker), Some("a"));
}

#[tokio::test]
async fn dividers_are_not_counted() {
    let mut picker = picker(&LOG);
    assert_eq!(picker.counts(), (5, 5));
    query(&mut picker, "c");
    assert_eq!(picker.counts(), (1, 5));
}

#[tokio::test]
async fn items_are_found_by_the_order_they_were_added_in() {
    let mut picker = picker(&LOG);
    query(&mut picker, "d");
    assert_eq!(picker.item(0).map(String::as_str), Some("#new"));
    assert_eq!(picker.item(5).map(String::as_str), Some("d"));
    assert_eq!(picker.item(7), None);
}

#[tokio::test]
async fn pickers_without_dividers_move_as_before() {
    let columns = [Column::new("text", |item: &String, _: &()| {
        item.as_str().into()
    })];
    let mut picker = Picker::new(columns, 0, ["#a", "b"].map(String::from), (), |_, _, _| {});
    settle(&mut picker);
    assert_eq!(picker.cursor_index(), 0);
    assert_eq!(at(&picker), Some("#a"));
    picker.move_by(1, Direction::Backward);
    assert_eq!(at(&picker), Some("b"));
}

#[tokio::test]
async fn negated_queries_match_dividers_but_the_cursor_skips_them() {
    let mut picker = picker(&LOG);
    // nothing in a divider contradicts "not c"
    query(&mut picker, "!c");
    assert_eq!(picker.matcher.snapshot().matched_item_count(), 6);
    assert_eq!(picker.counts(), (4, 5));
    let mut seen = Vec::new();
    for _ in 0..8 {
        seen.push(at(&picker).unwrap().to_string());
        picker.move_by(1, Direction::Forward);
    }
    assert!(seen
        .iter()
        .all(|item| !item.starts_with('#') && item != "c"));
}

#[tokio::test]
async fn pickers_have_no_modes_unless_asked() {
    let mut picker = picker(&LOG);
    // letters go to the prompt, as in any picker
    assert_eq!(picker.mode(), PickerMode::Filter);
    picker.set_mode(PickerMode::Normal);
    assert_eq!(picker.mode(), PickerMode::Filter);
    assert!(!picker.pending_key());
}

#[tokio::test]
async fn pickers_with_modes_open_in_normal_mode() {
    let mut picker = picker(&LOG).with_modes();
    assert_eq!(picker.mode(), PickerMode::Normal);
    picker.set_mode(PickerMode::Filter);
    assert_eq!(picker.mode(), PickerMode::Filter);
    picker.set_mode(PickerMode::Normal);
    assert_eq!(picker.mode(), PickerMode::Normal);
    // a pending `g` is forgotten when the mode changes
    picker.goto = true;
    picker.set_mode(PickerMode::Filter);
    assert!(!picker.pending_key());
}

#[tokio::test]
async fn matched_items_are_in_the_order_they_are_listed_in() {
    let mut picker = picker(&LOG);
    assert_eq!(picker.matched_item(0).map(String::as_str), Some("#new"));
    assert_eq!(picker.matched_item(6).map(String::as_str), Some("e"));
    assert_eq!(picker.matched_item(7), None);
    // dividers don't match a query, and the indexes count the items that do
    query(&mut picker, "d");
    assert_eq!(picker.matched_item(0).map(String::as_str), Some("d"));
    assert_eq!(picker.matched_item(1), None);
}

/// A picker of strings in groups: the ones that start with `#` are dividers with entries after
/// them, the ones that start with `!` are dividers that are entries, a group of their own.
fn grouped(items: &[&str]) -> Picker<String, ()> {
    let columns = [Column::new("text", |item: &String, _: &()| {
        if item.starts_with('#') {
            "".into()
        } else {
            item.trim_start_matches('!').into()
        }
    })];
    let mut picker = Picker::new(
        columns,
        0,
        items.iter().map(|item| item.to_string()),
        (),
        |_, _, _| {},
    )
    .with_dividers(|item, _| {
        item.strip_prefix('#')
            .or_else(|| item.strip_prefix('!'))
            .map(Into::into)
    })
    .with_groups(|item| item.starts_with('!'))
    .with_section_keys()
    .with_modes();
    settle(&mut picker);
    picker
}

/// The rows as they are listed.
fn listed(picker: &Picker<String, ()>) -> Vec<&str> {
    (0..)
        .map_while(|i| picker.matched_item(i))
        .map(String::as_str)
        .collect()
}

const FILES: [&str; 7] = [
    "#a.rs",
    "a.rs:1",
    "a.rs:9",
    "#b.rs",
    "b.rs:4",
    "!Cargo.lock",
    "!logo.png",
];

#[tokio::test]
async fn a_query_lists_the_groups_with_a_match_in_order() {
    let mut picker = grouped(&FILES);
    assert_eq!(listed(&picker), FILES);
    // a path: all the entries of its group, under their divider
    query(&mut picker, "a.rs");
    assert_eq!(listed(&picker), ["#a.rs", "a.rs:1", "a.rs:9"]);
    assert_eq!(picker.counts(), (2, 5));
    // one entry of a group
    query(&mut picker, "a.rs:9");
    assert_eq!(listed(&picker), ["#a.rs", "a.rs:9"]);
    assert_eq!(at(&picker), Some("a.rs:9"));
    // the order of the groups, not the matcher's
    query(&mut picker, "rs:");
    assert_eq!(
        listed(&picker),
        ["#a.rs", "a.rs:1", "a.rs:9", "#b.rs", "b.rs:4"]
    );
    // a divider that is an entry matches by its own text
    query(&mut picker, "lock");
    assert_eq!(listed(&picker), ["!Cargo.lock"]);
    assert_eq!(at(&picker), Some("!Cargo.lock"));
    assert_eq!(picker.counts(), (1, 5));
    // no match: no divider
    query(&mut picker, "zzz");
    assert!(listed(&picker).is_empty());
    assert_eq!(at(&picker), None);
}

#[tokio::test]
async fn dividers_that_are_entries_are_stops_and_counted() {
    let mut picker = grouped(&FILES);
    assert_eq!(at(&picker), Some("a.rs:1"));
    let mut seen = Vec::new();
    for _ in 0..5 {
        picker.move_by(1, Direction::Forward);
        seen.push(at(&picker).unwrap().to_string());
    }
    assert_eq!(
        seen,
        ["a.rs:9", "b.rs:4", "!Cargo.lock", "!logo.png", "a.rs:1"]
    );
    // the dividers with entries are not counted, the others are
    assert_eq!(picker.counts(), (5, 5));
}

#[tokio::test]
async fn w_and_b_move_between_the_groups() {
    let mut picker = grouped(&FILES);
    let mut seen = Vec::new();
    for _ in 0..4 {
        picker.move_to_section(Direction::Forward);
        seen.push(at(&picker).unwrap().to_string());
    }
    // to the first entry of each group, and no further than the last
    assert_eq!(seen, ["b.rs:4", "!Cargo.lock", "!logo.png", "!logo.png"]);
    let mut seen = Vec::new();
    for _ in 0..4 {
        picker.move_to_section(Direction::Backward);
        seen.push(at(&picker).unwrap().to_string());
    }
    assert_eq!(seen, ["!Cargo.lock", "b.rs:4", "a.rs:1", "a.rs:1"]);
    // from inside a group, `b` goes to its first entry first
    picker.move_by(1, Direction::Forward);
    assert_eq!(at(&picker), Some("a.rs:9"));
    picker.move_to_section(Direction::Backward);
    assert_eq!(at(&picker), Some("a.rs:1"));
    // with a query, among the groups listed
    query(&mut picker, "4");
    picker.move_to_section(Direction::Forward);
    assert_eq!(at(&picker), Some("b.rs:4"));
}

/// A picker of strings whose tie rank is the position in `recent` (the others have none).
fn ranked(items: &[&str], recent: &[&str]) -> Picker<String, ()> {
    let columns = [Column::new("text", |item: &String, _: &()| {
        item.as_str().into()
    })];
    let recent: Vec<String> = recent.iter().map(|item| item.to_string()).collect();
    let mut picker = Picker::new(
        columns,
        0,
        items.iter().map(|item| item.to_string()),
        (),
        |_, _, _| {},
    )
    .with_tie_rank(move |item| {
        recent
            .iter()
            .position(|recent| recent == item)
            .map_or(u32::MAX, |rank| rank as u32)
    });
    settle(&mut picker);
    picker
}

#[tokio::test]
async fn without_a_rank_the_matchers_order_is_kept_for_equal_scores() {
    let mut picker = ranked(&["abc-x", "abc-y", "abc-z"], &[]);
    query(&mut picker, "abc");
    assert_eq!(listed(&picker), ["abc-x", "abc-y", "abc-z"]);
}

#[tokio::test]
async fn a_ranked_item_wins_a_tie() {
    let mut picker = ranked(&["abc-x", "abc-y", "abc-z"], &["abc-y"]);
    query(&mut picker, "abc");
    assert_eq!(listed(&picker), ["abc-y", "abc-x", "abc-z"]);
    // the lowest rank first among the ranked
    let mut picker = ranked(&["abc-x", "abc-y", "abc-z"], &["abc-z", "abc-y"]);
    query(&mut picker, "abc");
    assert_eq!(listed(&picker), ["abc-z", "abc-y", "abc-x"]);
}

#[tokio::test]
async fn a_better_score_beats_a_better_rank() {
    let mut picker = ranked(&["a-b-c", "abc-x"], &["a-b-c"]);
    query(&mut picker, "abc");
    // the contiguous match scores higher than the scattered one, ranked or not
    assert_eq!(listed(&picker), ["abc-x", "a-b-c"]);
}

#[tokio::test]
async fn without_a_query_the_order_is_the_one_the_items_were_added_in() {
    let mut picker = ranked(&["abc-x", "abc-y"], &["abc-y"]);
    assert_eq!(listed(&picker), ["abc-x", "abc-y"]);
    query(&mut picker, "abc");
    assert_eq!(listed(&picker), ["abc-y", "abc-x"]);
    query(&mut picker, "");
    assert_eq!(listed(&picker), ["abc-x", "abc-y"]);
}
