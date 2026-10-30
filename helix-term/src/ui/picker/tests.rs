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
