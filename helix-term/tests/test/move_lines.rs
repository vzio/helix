//! `move_lines_up` and `move_lines_down` (`A-k`, `A-j`).

use std::cell::{Cell, RefCell};

use helix_term::application::Application;
use helix_view::{current_ref, doc, document::Mode, input::parse_macro, view};

use super::*;

/// Runs `keys` on `input`: the text is as it is written, `\n` at the end of a file included.
async fn run(input: &str, keys: &str, expected: &str) -> anyhow::Result<()> {
    test((input, keys, expected, LineFeedHandling::AsIs)).await
}

/// Like `run`, with an `<esc>` first, which commits the input text to the history. The text
/// after `keys` is `expected`, and `u` brings back the input, `U` the moved text again.
async fn run_with_undo(input: &str, keys: &str, expected: &str) -> anyhow::Result<()> {
    run(input, &format!("<esc>{keys}"), expected).await?;
    run(input, &format!("<esc>{keys}u"), input).await?;
    run(input, &format!("<esc>{keys}uU"), expected).await
}

fn revision(app: &Application) -> usize {
    let doc = doc!(app.editor);
    let history = doc.history.take();
    let revision = history.current_revision();
    doc.history.set(history);
    revision
}

#[tokio::test(flavor = "multi_thread")]
async fn one_line_up_and_down() -> anyhow::Result<()> {
    run_with_undo(
        "aaa\nbbb\ncc#[|c]#\nddd\n",
        "<A-k>",
        "aaa\ncc#[|c]#\nbbb\nddd\n",
    )
    .await?;
    run_with_undo(
        "aa#[|a]#\nbbb\nccc\nddd\n",
        "<A-j>",
        "bbb\naa#[|a]#\nccc\nddd\n",
    )
    .await?;
    // twice
    run(
        "aa#[|a]#\nbbb\nccc\nddd\n",
        "<A-j><A-j>",
        "bbb\nccc\naa#[|a]#\nddd\n",
    )
    .await?;
    run(
        "aaa\nbbb\nccc\nd#[|d]#d\n",
        "<A-k><A-k>",
        "aaa\nd#[|d]#d\nbbb\nccc\n",
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_column_and_the_direction_are_kept() -> anyhow::Result<()> {
    run("aaa\nb#[bb|]#b\nccc\n", "<A-j>", "aaa\nccc\nb#[bb|]#b\n").await?;
    run("aaa\nb#[|bb]#b\nccc\n", "<A-k>", "b#[|bb]#b\naaa\nccc\n").await?;
    // (a line shorter than the column of the cursor)
    run("a\nabcd#[|e]#f\n", "<A-k>", "abcd#[|e]#f\na\n").await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_selection_over_several_lines() -> anyhow::Result<()> {
    run_with_undo(
        "aaa\nb#[bb\nccc\nd|]#dd\neee\nfff\n",
        "<A-j>",
        "aaa\neee\nb#[bb\nccc\nd|]#dd\nfff\n",
    )
    .await?;
    run_with_undo(
        "aaa\nb#[|bb\nccc\nd]#dd\neee\nfff\n",
        "<A-k>",
        "b#[|bb\nccc\nd]#dd\naaa\neee\nfff\n",
    )
    .await?;
    // a line selected with `x`
    run("aaa\n#[bbb\n|]#ccc\n", "<A-k>", "#[bbb\n|]#aaa\nccc\n").await
}

#[tokio::test(flavor = "multi_thread")]
async fn select_mode() -> anyhow::Result<()> {
    run(
        "aaa\n#[b|]#bb\nccc\nddd\n",
        "vl<A-j>",
        "aaa\nccc\n#[bb|]#b\nddd\n",
    )
    .await?;
    // still in select mode: the next motion extends the selection from where it was
    run(
        "aaa\n#[b|]#bb\nccc\nddd\n",
        "vl<A-j>l",
        "aaa\nccc\n#[bbb|]#\nddd\n",
    )
    .await?;
    // a selection over two lines, moved up
    run(
        "aaa\nbbb\n#[ccc\nd|]#dd\n",
        "v<A-k>l",
        "aaa\n#[ccc\ndd|]#d\nbbb\n",
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_column_of_j_and_k_is_kept() -> anyhow::Result<()> {
    // `j` onto a short line remembers the column of the cursor: after moving that line down,
    // `k` goes to that column in the line that was below
    let mut app = AppBuilder::new()
        .with_input_text("aa\nabcd#[|e]#f\nx\nuvwxyz\ntail\n")
        .build()?;
    test_key_sequences(
        &mut app,
        vec![(
            Some("j<A-j>k"),
            Some(&|app| {
                let (view, doc) = current_ref!(app.editor);
                assert_eq!(doc.text(), "aa\nabcdef\nuvwxyz\nx\ntail\n");
                let cursor = doc
                    .selection(view.id)
                    .primary()
                    .cursor(doc.text().slice(..));
                assert_eq!(cursor, "aa\nabcdef\nuvwx".chars().count());
            }),
        )],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn counts() -> anyhow::Result<()> {
    run_with_undo("#[|a]#\nb\nc\nd\ne\n", "3<A-j>", "b\nc\nd\n#[|a]#\ne\n").await?;
    run_with_undo("a\nb\nc\nd\n#[|e]#\n", "3<A-k>", "a\n#[|e]#\nb\nc\nd\n").await?;
    run_with_undo("#[a\nb|]#\nc\nd\ne\n", "2<A-j>", "c\nd\n#[a\nb|]#\ne\n").await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_count_is_one_undo_step() -> anyhow::Result<()> {
    let before = Cell::new(0);
    let mut app = AppBuilder::new()
        .with_input_text("#[|a]#\nb\nc\nd\ne\n")
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some("<esc>"), Some(&|app| before.set(revision(app)))),
            (
                Some("4<A-j>"),
                Some(&|app| assert_eq!(revision(app), before.get() + 1)),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn clamped_near_the_edge() -> anyhow::Result<()> {
    run_with_undo("a\nb\n#[|c]#\nd\ne\n", "9<A-j>", "a\nb\nd\ne\n#[|c]#\n").await?;
    run_with_undo("a\nb\n#[|c]#\nd\ne\n", "9<A-k>", "#[|c]#\na\nb\nd\ne\n").await?;
    // the nearest block sets how far everything goes
    run(
        "#[|a]#\nb\nc\n#(|d)#\ne\n",
        "9<A-j>",
        "b\n#[|a]#\nc\ne\n#(|d)#\n",
    )
    .await
}

async fn no_change(input: &str, keys: &str) -> anyhow::Result<()> {
    let before = Cell::new(0);
    // (the editor makes the empty selections one char wide, so this is the state after `<esc>`)
    let state = RefCell::new(None);
    let mut app = AppBuilder::new().with_input_text(input).build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("<esc>"),
                Some(&|app| {
                    before.set(revision(app));
                    let (view, doc) = current_ref!(app.editor);
                    *state.borrow_mut() =
                        Some((doc.text().clone(), doc.selection(view.id).clone()));
                }),
            ),
            (
                Some(keys),
                Some(&|app| {
                    let (view, doc) = current_ref!(app.editor);
                    let (text, selection) = state.borrow().clone().unwrap();
                    assert_eq!(&text, doc.text());
                    assert_eq!(doc.selection(view.id), &selection);
                    // (no entry in the history)
                    assert_eq!(revision(app), before.get());
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn at_the_edge_nothing_happens() -> anyhow::Result<()> {
    no_change("#[|a]#\nb\nc\n", "<A-k>").await?;
    no_change("a\nb\n#[|c]#\n", "<A-j>").await?;
    no_change("#[a\nb|]#\nc\n", "<A-k><A-k>").await?;
    no_change("a\n#[b\nc|]#\n", "<A-j><A-j>").await?;
    no_change("a\nb\n#[|]#", "<A-j>").await?;
    no_change("a\nb\n#[|]#", "<A-k>").await?;
    no_change("#[a\nb\nc\n|]#", "<A-j>").await?;
    no_change("#[a\nb\nc\n|]#", "<A-k>").await?;
    no_change("#[|a]#b", "<A-k><A-j>").await?;
    no_change("#[|]#", "<A-k><A-j>").await?;
    no_change("#[|]#\n", "<A-k><A-j>").await?;
    no_change("a\n#[|b]#\n#(|c)#\n", "<A-j>").await
}

#[tokio::test(flavor = "multi_thread")]
async fn last_line_without_a_line_ending() -> anyhow::Result<()> {
    let raw = |input: &str, keys: &str, expected: &str| {
        let (input, keys, expected) = (input.to_string(), keys.to_string(), expected.to_string());
        async move { run_with_undo(&input, &keys, &expected).await }
    };
    raw("a\nb\n#[|c]#", "<A-k>", "a\n#[|c]#\nb").await?;
    raw("a\n#[|b]#\nc", "<A-j>", "a\nc\n#[|b]#").await?;
    raw("a\nb\n#[|c]#", "2<A-k>", "#[|c]#\na\nb").await?;
    raw("#[|a]#\nb\nc", "2<A-j>", "b\nc\n#[|a]#").await?;
    raw("a\n#[b\nc|]#", "<A-k>", "#[b\nc|]#\na").await?;
    raw("#[a\nb|]#\nc", "<A-j>", "c\n#[a\nb|]#").await?;
    // a file of two lines
    raw("a\n#[|b]#", "<A-k>", "#[|b]#\na").await?;
    raw("#[|a]#\nb", "<A-j>", "b\n#[|a]#").await?;
    // the cursor at the very end
    raw("a\nb#[|c]#", "<A-k>", "b#[|c]#\na").await
}

#[tokio::test(flavor = "multi_thread")]
async fn files_of_one_line_and_empty_files() -> anyhow::Result<()> {
    no_change("#[|a]#", "<A-j><A-k>").await?;
    no_change("#[|a]#\n", "<A-j><A-k>").await?;
    no_change("#[|]#", "<A-j><A-k>").await?;
    no_change("#[|]#\n", "<A-j><A-k>").await
}

#[tokio::test(flavor = "multi_thread")]
async fn several_selections() -> anyhow::Result<()> {
    // separate blocks, the lines between them stay between them
    run_with_undo(
        "#[|a]#\nb\nc\n#(|d)#\ne\nf\n",
        "<A-j>",
        "b\n#[|a]#\nc\ne\n#(|d)#\nf\n",
    )
    .await?;
    run_with_undo(
        "a\n#[|b]#\nc\nd\n#(|e)#\nf\n",
        "<A-k>",
        "#[|b]#\na\nc\n#(|e)#\nd\nf\n",
    )
    .await?;
    // adjacent lines are one block
    run_with_undo("a\n#[|b]#\n#(|c)#\nd\n", "<A-j>", "a\nd\n#[|b]#\n#(|c)#\n").await?;
    run_with_undo(
        "a\nb\n#[|c]#\n#(|d)#\ne\n",
        "<A-k>",
        "a\n#[|c]#\n#(|d)#\nb\ne\n",
    )
    .await?;
    // two cursors on the same line
    run_with_undo(
        "a\nb#[|c]#d#(|e)#f\ng\n",
        "<A-k>",
        "b#[|c]#d#(|e)#f\na\ng\n",
    )
    .await?;
    // ranges that overlap on the same lines
    run_with_undo(
        "a\n#[b\nc|]#\n#(c|)#d\ne\nf\n",
        "<A-j>",
        "a\ne\n#[b\nc|]#\n#(c|)#d\nf\n",
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn crlf() -> anyhow::Result<()> {
    run_with_undo("a\r\nb#[|c]#\r\nd\r\n", "<A-k>", "b#[|c]#\r\na\r\nd\r\n").await?;
    run_with_undo("a\r\nb#[|c]#\r\nd\r\n", "<A-j>", "a\r\nd\r\nb#[|c]#\r\n").await?;
    run_with_undo("a\r\nb\r\n#[|c]#", "<A-k>", "a\r\n#[|c]#\r\nb").await?;
    run_with_undo("a\r\n#[|b]#\r\nc", "<A-j>", "a\r\nc\r\n#[|b]#").await?;
    // (clamped by the block at the end, which is the line without line ending)
    run_with_undo(
        "#[|a]#\r\nb\r\nc\r\n#(|d)#\r\ne",
        "2<A-j>",
        "b\r\n#[|a]#\r\nc\r\ne\r\n#(|d)#",
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn registers_and_the_clipboard_are_untouched() -> anyhow::Result<()> {
    // `x` and `y` yank the line `a`, which is still what `p` pastes after moving
    run("#[|a]#\nb\nc\n", "xy<A-j>p", "b\na\n#[a\n|]#c\n").await?;
    let mut app = AppBuilder::new()
        .with_input_text("#[|a]#\nb\nc\n")
        .build()?;
    test_key_sequences(
        &mut app,
        vec![(
            Some("xy<A-j><A-j><A-k>"),
            Some(&|app| {
                let yanked: Vec<String> = app
                    .editor
                    .registers
                    .read('"', &app.editor)
                    .map(|values| values.map(|value| value.to_string()).collect())
                    .unwrap_or_default();
                assert_eq!(yanked, vec!["a\n".to_string()]);
            }),
        )],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn other_views_follow_the_text() -> anyhow::Result<()> {
    let mut app = AppBuilder::new()
        .with_input_text("a\nb#[|b]#b\nc\n")
        .build()?;
    test_key_sequences(
        &mut app,
        vec![
            (
                // (split: the new view is the current one, with the same selection)
                Some("<esc><C-w>v<A-j>"),
                Some(&|app| {
                    let doc = doc!(app.editor);
                    assert_eq!(doc.text(), "a\nc\nbbb\n");
                    let current = view!(app.editor).id;
                    let mut views = 0;
                    for (view, _) in app.editor.tree.views() {
                        views += 1;
                        let selection = doc.selection(view.id);
                        assert_eq!(
                            helix_core::test::plain(doc.text().slice(..), selection),
                            "a\nc\nb#[|b]#b\n",
                            "{}",
                            if view.id == current {
                                "current view"
                            } else {
                                "other view"
                            }
                        );
                    }
                    assert_eq!(views, 2);
                }),
            ),
            // (one view left for the end of the test)
            (Some(":q!<ret>"), None),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_alt_arrow_keys() -> anyhow::Result<()> {
    run_with_undo("a\nb#[|b]#\nc\n", "<A-down>", "a\nc\nb#[|b]#\n").await?;
    run_with_undo("a\nb#[|b]#\nc\n", "<A-up>", "b#[|b]#\na\nc\n").await?;
    run("a\nb#[|b]#\nc\n", "2<A-up>", "b#[|b]#\na\nc\n").await
}

#[test]
fn the_keys() {
    use helix_term::keymap::{default, KeyTrie};

    let keymaps = default();
    for mode in [Mode::Normal, Mode::Select] {
        let command = |key: &str| {
            let key = parse_macro(key).unwrap();
            match keymaps[&mode].search(&key) {
                Some(KeyTrie::MappableCommand(command)) => command.name().to_string(),
                other => format!("{other:?}"),
            }
        };
        assert_eq!(command("<A-j>"), "move_lines_down", "{mode:?}");
        assert_eq!(command("<A-k>"), "move_lines_up", "{mode:?}");
        assert_eq!(command("<A-down>"), "move_lines_down", "{mode:?}");
        assert_eq!(command("<A-up>"), "move_lines_up", "{mode:?}");
        // the syntax tree selection keeps A-o and A-i
        assert_eq!(command("<A-o>"), "expand_selection", "{mode:?}");
        assert_eq!(command("<A-i>"), "shrink_selection", "{mode:?}");
    }
    for key in ["<A-j>", "<A-k>", "<A-down>", "<A-up>"] {
        let key = parse_macro(key).unwrap();
        assert!(keymaps[&Mode::Insert].search(&key).is_none());
    }
}
