//! `move_lines_up` and `move_lines_down`, and the move mode (`M`) that they are bound in.
//!
//! The tests of what the commands do bind them to `C-j` and `C-k` (a config like the one a user
//! can write, which also shows that they are rebindable); the tests of the move mode use `M`.

use std::cell::{Cell, RefCell};

use helix_term::application::Application;
use helix_view::{current_ref, doc, document::Mode, input::parse_macro, view};

use super::*;

/// The default config, with the commands bound to `C-j` and `C-k` in normal and select mode.
fn rebound() -> Config {
    let mut config = test_config();
    config.keys = toml::from_str(
        r#"
        [normal]
        C-j = "move_lines_down"
        C-k = "move_lines_up"

        [select]
        C-j = "move_lines_down"
        C-k = "move_lines_up"
        "#,
    )
    .unwrap();
    config
}

fn app(input: &str) -> anyhow::Result<Application> {
    AppBuilder::new()
        .with_config(rebound())
        .with_input_text(input)
        .build()
}

/// Runs `keys` on `input`: the text is as it is written, `\n` at the end of a file included.
async fn run(input: &str, keys: &str, expected: &str) -> anyhow::Result<()> {
    test_with_config(
        AppBuilder::new().with_config(rebound()),
        (input, keys, expected, LineFeedHandling::AsIs),
    )
    .await
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
        "<C-k>",
        "aaa\ncc#[|c]#\nbbb\nddd\n",
    )
    .await?;
    run_with_undo(
        "aa#[|a]#\nbbb\nccc\nddd\n",
        "<C-j>",
        "bbb\naa#[|a]#\nccc\nddd\n",
    )
    .await?;
    // twice
    run(
        "aa#[|a]#\nbbb\nccc\nddd\n",
        "<C-j><C-j>",
        "bbb\nccc\naa#[|a]#\nddd\n",
    )
    .await?;
    run(
        "aaa\nbbb\nccc\nd#[|d]#d\n",
        "<C-k><C-k>",
        "aaa\nd#[|d]#d\nbbb\nccc\n",
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_column_and_the_direction_are_kept() -> anyhow::Result<()> {
    run("aaa\nb#[bb|]#b\nccc\n", "<C-j>", "aaa\nccc\nb#[bb|]#b\n").await?;
    run("aaa\nb#[|bb]#b\nccc\n", "<C-k>", "b#[|bb]#b\naaa\nccc\n").await?;
    // (a line shorter than the column of the cursor)
    run("a\nabcd#[|e]#f\n", "<C-k>", "abcd#[|e]#f\na\n").await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_selection_over_several_lines() -> anyhow::Result<()> {
    run_with_undo(
        "aaa\nb#[bb\nccc\nd|]#dd\neee\nfff\n",
        "<C-j>",
        "aaa\neee\nb#[bb\nccc\nd|]#dd\nfff\n",
    )
    .await?;
    run_with_undo(
        "aaa\nb#[|bb\nccc\nd]#dd\neee\nfff\n",
        "<C-k>",
        "b#[|bb\nccc\nd]#dd\naaa\neee\nfff\n",
    )
    .await?;
    // a line selected with `x`
    run("aaa\n#[bbb\n|]#ccc\n", "<C-k>", "#[bbb\n|]#aaa\nccc\n").await
}

#[tokio::test(flavor = "multi_thread")]
async fn select_mode() -> anyhow::Result<()> {
    run(
        "aaa\n#[b|]#bb\nccc\nddd\n",
        "vl<C-j>",
        "aaa\nccc\n#[bb|]#b\nddd\n",
    )
    .await?;
    // still in select mode: the next motion extends the selection from where it was
    run(
        "aaa\n#[b|]#bb\nccc\nddd\n",
        "vl<C-j>l",
        "aaa\nccc\n#[bbb|]#\nddd\n",
    )
    .await?;
    // a selection over two lines, moved up
    run(
        "aaa\nbbb\n#[ccc\nd|]#dd\n",
        "v<C-k>l",
        "aaa\n#[ccc\ndd|]#d\nbbb\n",
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_column_of_j_and_k_is_kept() -> anyhow::Result<()> {
    // `j` onto a short line remembers the column of the cursor: after moving that line down,
    // `k` goes to that column in the line that was below
    let mut app = app("aa\nabcd#[|e]#f\nx\nuvwxyz\ntail\n")?;
    test_key_sequences(
        &mut app,
        vec![(
            Some("j<C-j>k"),
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
    run_with_undo("#[|a]#\nb\nc\nd\ne\n", "3<C-j>", "b\nc\nd\n#[|a]#\ne\n").await?;
    run_with_undo("a\nb\nc\nd\n#[|e]#\n", "3<C-k>", "a\n#[|e]#\nb\nc\nd\n").await?;
    run_with_undo("#[a\nb|]#\nc\nd\ne\n", "2<C-j>", "c\nd\n#[a\nb|]#\ne\n").await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_count_is_one_undo_step() -> anyhow::Result<()> {
    let before = Cell::new(0);
    let mut app = app("#[|a]#\nb\nc\nd\ne\n")?;
    test_key_sequences(
        &mut app,
        vec![
            (Some("<esc>"), Some(&|app| before.set(revision(app)))),
            (
                Some("4<C-j>"),
                Some(&|app| assert_eq!(revision(app), before.get() + 1)),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn clamped_near_the_edge() -> anyhow::Result<()> {
    run_with_undo("a\nb\n#[|c]#\nd\ne\n", "9<C-j>", "a\nb\nd\ne\n#[|c]#\n").await?;
    run_with_undo("a\nb\n#[|c]#\nd\ne\n", "9<C-k>", "#[|c]#\na\nb\nd\ne\n").await?;
    // the nearest block sets how far everything goes
    run(
        "#[|a]#\nb\nc\n#(|d)#\ne\n",
        "9<C-j>",
        "b\n#[|a]#\nc\ne\n#(|d)#\n",
    )
    .await
}

async fn no_change(input: &str, keys: &str) -> anyhow::Result<()> {
    let before = Cell::new(0);
    // (the editor makes the empty selections one char wide, so this is the state after `<esc>`)
    let state = RefCell::new(None);
    let mut app = app(input)?;
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
    no_change("#[|a]#\nb\nc\n", "<C-k>").await?;
    no_change("a\nb\n#[|c]#\n", "<C-j>").await?;
    no_change("#[a\nb|]#\nc\n", "<C-k><C-k>").await?;
    no_change("a\n#[b\nc|]#\n", "<C-j><C-j>").await?;
    no_change("a\nb\n#[|]#", "<C-j>").await?;
    no_change("a\nb\n#[|]#", "<C-k>").await?;
    no_change("#[a\nb\nc\n|]#", "<C-j>").await?;
    no_change("#[a\nb\nc\n|]#", "<C-k>").await?;
    no_change("#[|a]#b", "<C-k><C-j>").await?;
    no_change("#[|]#", "<C-k><C-j>").await?;
    no_change("#[|]#\n", "<C-k><C-j>").await?;
    no_change("a\n#[|b]#\n#(|c)#\n", "<C-j>").await
}

#[tokio::test(flavor = "multi_thread")]
async fn last_line_without_a_line_ending() -> anyhow::Result<()> {
    let raw = |input: &str, keys: &str, expected: &str| {
        let (input, keys, expected) = (input.to_string(), keys.to_string(), expected.to_string());
        async move { run_with_undo(&input, &keys, &expected).await }
    };
    raw("a\nb\n#[|c]#", "<C-k>", "a\n#[|c]#\nb").await?;
    raw("a\n#[|b]#\nc", "<C-j>", "a\nc\n#[|b]#").await?;
    raw("a\nb\n#[|c]#", "2<C-k>", "#[|c]#\na\nb").await?;
    raw("#[|a]#\nb\nc", "2<C-j>", "b\nc\n#[|a]#").await?;
    raw("a\n#[b\nc|]#", "<C-k>", "#[b\nc|]#\na").await?;
    raw("#[a\nb|]#\nc", "<C-j>", "c\n#[a\nb|]#").await?;
    // a file of two lines
    raw("a\n#[|b]#", "<C-k>", "#[|b]#\na").await?;
    raw("#[|a]#\nb", "<C-j>", "b\n#[|a]#").await?;
    // the cursor at the very end
    raw("a\nb#[|c]#", "<C-k>", "b#[|c]#\na").await
}

#[tokio::test(flavor = "multi_thread")]
async fn files_of_one_line_and_empty_files() -> anyhow::Result<()> {
    no_change("#[|a]#", "<C-j><C-k>").await?;
    no_change("#[|a]#\n", "<C-j><C-k>").await?;
    no_change("#[|]#", "<C-j><C-k>").await?;
    no_change("#[|]#\n", "<C-j><C-k>").await
}

#[tokio::test(flavor = "multi_thread")]
async fn several_selections() -> anyhow::Result<()> {
    // separate blocks, the lines between them stay between them
    run_with_undo(
        "#[|a]#\nb\nc\n#(|d)#\ne\nf\n",
        "<C-j>",
        "b\n#[|a]#\nc\ne\n#(|d)#\nf\n",
    )
    .await?;
    run_with_undo(
        "a\n#[|b]#\nc\nd\n#(|e)#\nf\n",
        "<C-k>",
        "#[|b]#\na\nc\n#(|e)#\nd\nf\n",
    )
    .await?;
    // adjacent lines are one block
    run_with_undo("a\n#[|b]#\n#(|c)#\nd\n", "<C-j>", "a\nd\n#[|b]#\n#(|c)#\n").await?;
    run_with_undo(
        "a\nb\n#[|c]#\n#(|d)#\ne\n",
        "<C-k>",
        "a\n#[|c]#\n#(|d)#\nb\ne\n",
    )
    .await?;
    // two cursors on the same line
    run_with_undo(
        "a\nb#[|c]#d#(|e)#f\ng\n",
        "<C-k>",
        "b#[|c]#d#(|e)#f\na\ng\n",
    )
    .await?;
    // ranges that overlap on the same lines
    run_with_undo(
        "a\n#[b\nc|]#\n#(c|)#d\ne\nf\n",
        "<C-j>",
        "a\ne\n#[b\nc|]#\n#(c|)#d\nf\n",
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn crlf() -> anyhow::Result<()> {
    run_with_undo("a\r\nb#[|c]#\r\nd\r\n", "<C-k>", "b#[|c]#\r\na\r\nd\r\n").await?;
    run_with_undo("a\r\nb#[|c]#\r\nd\r\n", "<C-j>", "a\r\nd\r\nb#[|c]#\r\n").await?;
    run_with_undo("a\r\nb\r\n#[|c]#", "<C-k>", "a\r\n#[|c]#\r\nb").await?;
    run_with_undo("a\r\n#[|b]#\r\nc", "<C-j>", "a\r\nc\r\n#[|b]#").await?;
    // (clamped by the block at the end, which is the line without line ending)
    run_with_undo(
        "#[|a]#\r\nb\r\nc\r\n#(|d)#\r\ne",
        "2<C-j>",
        "b\r\n#[|a]#\r\nc\r\ne\r\n#(|d)#",
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn registers_and_the_clipboard_are_untouched() -> anyhow::Result<()> {
    // `x` and `y` yank the line `a`, which is still what `p` pastes after moving
    run("#[|a]#\nb\nc\n", "xy<C-j>p", "b\na\n#[a\n|]#c\n").await?;
    let mut app = app("#[|a]#\nb\nc\n")?;
    test_key_sequences(
        &mut app,
        vec![(
            Some("xy<C-j><C-j><C-k>"),
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
    let mut app = app("a\nb#[|b]#b\nc\n")?;
    test_key_sequences(
        &mut app,
        vec![
            (
                // (split: the new view is the current one, with the same selection)
                Some("<esc><C-w>v<C-j>"),
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

// The move mode: `M`, a sticky minor mode like `Z` (view mode).

/// Runs `keys` (which end in the move mode or have left it) and checks the text, then that `u`
/// brings back `input`: the moves are `undos` undo steps.
async fn run_in_mode(input: &str, keys: &str, expected: &str, undos: usize) -> anyhow::Result<()> {
    run(input, &format!("<esc>{keys}"), expected).await?;
    run(input, &format!("<esc>{keys}{}", "u".repeat(undos)), input).await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_move_mode() -> anyhow::Result<()> {
    run_in_mode(
        "aaa\nbbb\ncc#[|c]#\nddd\n",
        "Mk<esc>",
        "aaa\ncc#[|c]#\nbbb\nddd\n",
        1,
    )
    .await?;
    run_in_mode(
        "aa#[|a]#\nbbb\nccc\nddd\n",
        "Mj<esc>",
        "bbb\naa#[|a]#\nccc\nddd\n",
        1,
    )
    .await?;
    // the arrow keys
    run_in_mode(
        "aa#[|a]#\nbbb\nccc\nddd\n",
        "M<down><down><esc>",
        "bbb\nccc\naa#[|a]#\nddd\n",
        2,
    )
    .await?;
    run_in_mode(
        "aaa\nbbb\nccc\nd#[|d]#d\n",
        "M<up><up><esc>",
        "aaa\nd#[|d]#d\nbbb\nccc\n",
        2,
    )
    .await?;
    // a selection over lines, the direction is kept
    run_in_mode(
        "aaa\n#[|bbb\nc]#cc\nddd\neee\n",
        "Mj<esc>",
        "aaa\nddd\n#[|bbb\nc]#cc\neee\n",
        1,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn counts_in_the_move_mode() -> anyhow::Result<()> {
    run_in_mode(
        "#[|a]#\nb\nc\nd\ne\n",
        "M3j<esc>",
        "b\nc\nd\n#[|a]#\ne\n",
        1,
    )
    .await?;
    run_in_mode(
        "a\nb\nc\nd\n#[|e]#\n",
        "M3k<esc>",
        "a\n#[|e]#\nb\nc\nd\n",
        1,
    )
    .await?;
    // the count is used up by the move: the next one is a single move
    run_in_mode(
        "#[|a]#\nb\nc\nd\ne\n",
        "M2jj<esc>",
        "b\nc\nd\n#[|a]#\ne\n",
        2,
    )
    .await?;
    // a count of two digits, clamped at the end
    run_in_mode("#[|a]#\nb\nc\n", "M12j<esc>", "b\nc\n#[|a]#\n", 1).await
}

#[tokio::test(flavor = "multi_thread")]
async fn several_moves_in_the_move_mode_are_several_undo_steps() -> anyhow::Result<()> {
    run_in_mode(
        "#[|a]#\nb\nc\nd\ne\n",
        "Mjjj<esc>",
        "b\nc\nd\n#[|a]#\ne\n",
        3,
    )
    .await?;
    let before = Cell::new(0);
    let mut app = app("#[|a]#\nb\nc\nd\ne\n")?;
    test_key_sequences(
        &mut app,
        vec![
            (Some("<esc>"), Some(&|app| before.set(revision(app)))),
            (
                Some("Mj"),
                Some(&|app| assert_eq!(revision(app), before.get() + 1)),
            ),
            (
                Some("jj"),
                Some(&|app| assert_eq!(revision(app), before.get() + 3)),
            ),
            (
                Some("3k"),
                Some(&|app| assert_eq!(revision(app), before.get() + 4)),
            ),
            (
                Some("<esc>"),
                Some(&|app| {
                    assert_eq!(revision(app), before.get() + 4);
                    assert_eq!(doc!(app.editor).text(), "a\nb\nc\nd\ne\n");
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_leaves_the_move_mode() -> anyhow::Result<()> {
    // `j` moves the cursor again, not the line (the selection of a motion has the visual position
    // of the cursor in it, so it is the line of the cursor that is checked)
    let cursor_line = |app: &Application| {
        let (view, doc) = current_ref!(app.editor);
        doc.text().char_to_line(
            doc.selection(view.id)
                .primary()
                .cursor(doc.text().slice(..)),
        )
    };
    let mut app = app("a\nb\n#[|c]#\nd\ne\n")?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("Mk<esc>"),
                Some(&|app| {
                    assert_eq!(doc!(app.editor).text(), "a\nc\nb\nd\ne\n");
                    assert_eq!(cursor_line(app), 1);
                }),
            ),
            (
                Some("j"),
                Some(&|app| {
                    assert_eq!(doc!(app.editor).text(), "a\nc\nb\nd\ne\n");
                    assert_eq!(cursor_line(app), 2);
                }),
            ),
            // (the count is not kept by the mode either)
            (
                Some("M2j<esc>2k"),
                Some(&|app| {
                    assert_eq!(doc!(app.editor).text(), "a\nc\nd\ne\nb\n");
                    assert_eq!(cursor_line(app), 2);
                }),
            ),
        ],
        false,
    )
    .await?;
    // it can be entered again
    run(
        "a\nb\n#[|c]#\nd\ne\n",
        "Mk<esc>Mk<esc>",
        "#[|c]#\na\nb\nd\ne\n",
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn other_keys_do_nothing_in_the_move_mode() -> anyhow::Result<()> {
    // like in the view mode, a key that is not in the mode is dropped and the mode stays
    run("a\nb\n#[|c]#\nd\ne\n", "Mxwk<esc>", "a\n#[|c]#\nb\nd\ne\n").await?;
    // (`M` again too)
    run("a\nb\n#[|c]#\nd\ne\n", "MMk<esc>", "a\n#[|c]#\nb\nd\ne\n").await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_move_mode_in_select_mode() -> anyhow::Result<()> {
    run(
        "aaa\n#[b|]#bb\nccc\nddd\n",
        "<esc>vlMj<esc>",
        "aaa\nccc\n#[bb|]#b\nddd\n",
    )
    .await?;
    // (undo brings back the selection that was there when the lines were moved)
    run(
        "aaa\n#[b|]#bb\nccc\nddd\n",
        "<esc>vlMj<esc>u",
        "aaa\n#[bb|]#b\nccc\nddd\n",
    )
    .await?;
    // still select mode in the move mode: `Esc` leaves both (as it does from the sticky view mode),
    // so `l` is a normal mode motion
    run(
        "aaa\n#[b|]#bb\nccc\nddd\n",
        "vlMj<esc>l",
        "aaa\nccc\nbb#[b|]#\nddd\n",
    )
    .await?;
    // `v` goes back to select mode, and the selection is extended from where it was
    run(
        "aaa\n#[b|]#bb\nccc\nddd\n",
        "vlMj<esc>vl",
        "aaa\nccc\n#[bbb|]#\nddd\n",
    )
    .await?;
    run(
        "aaa\nbbb\n#[ccc\nd|]#dd\n",
        "vMk<esc>",
        "aaa\n#[ccc\nd|]#dd\nbbb\n",
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn several_selections_in_the_move_mode() -> anyhow::Result<()> {
    run_in_mode(
        "a\n#[|b]#\nc\n#(|d)#\ne\nf\n",
        "M2j<esc>",
        "a\nc\ne\n#[|b]#\nf\n#(|d)#\n",
        1,
    )
    .await
}

fn screen_rows(app: &Application) -> Vec<String> {
    let screen = app.screen();
    (0..screen.area.height)
        .map(|y| {
            (0..screen.area.width)
                .map(|x| screen.get(x, y).map_or(" ", |cell| cell.symbol.as_str()))
                .collect()
        })
        .collect()
}

/// The row of the statusline: the one with the mode, the last but one.
fn statusline(app: &Application) -> String {
    let rows = screen_rows(app);
    rows[rows.len() - 2].clone()
}

/// The style of the cell where `label` is in the statusline.
fn label_style(app: &Application, label: &str) -> helix_view::graphics::Style {
    let line = statusline(app);
    let x = line
        .find(label)
        .unwrap_or_else(|| panic!("{label:?} not in {line:?}"));
    let cell = app.screen().get(x as u16, app.screen().area.height - 2);
    let cell = cell.unwrap();
    helix_view::graphics::Style::default()
        .fg(cell.fg)
        .bg(cell.bg)
        .add_modifier(cell.modifier)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_statusline_shows_the_move_mode() -> anyhow::Result<()> {
    let mut config = rebound();
    config.editor.color_modes = true;
    let styles = RefCell::new(Vec::new());
    let mut app = AppBuilder::new()
        .with_config(config)
        .with_input_text("a\n#[|b]#\nc\n")
        .build()?;
    let check = |label: &'static str, absent: &'static [&'static str]| {
        move |app: &Application| {
            let line = statusline(app);
            assert!(line.contains(&format!(" {label} ")), "{line:?}");
            for other in absent {
                assert!(!line.contains(other), "{line:?}");
            }
        }
    };
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("<esc>"),
                Some(&|app| {
                    check("NOR", &["MOV", "SEL"])(app);
                    styles.borrow_mut().push(label_style(app, "NOR"));
                }),
            ),
            (
                Some("M"),
                Some(&|app| {
                    check("MOV", &["NOR", "SEL"])(app);
                    // the colors of normal mode
                    assert_eq!(label_style(app, "MOV"), styles.borrow()[0]);
                }),
            ),
            // it stays after a move, and a count is pending
            (Some("j"), Some(&|app| check("MOV", &["NOR"])(app))),
            (Some("2"), Some(&|app| check("MOV", &["NOR"])(app))),
            (Some("k"), Some(&|app| check("MOV", &["NOR"])(app))),
            (Some("<esc>"), Some(&|app| check("NOR", &["MOV"])(app))),
            // select mode
            (
                Some("v"),
                Some(&|app| {
                    check("SEL", &["MOV", "NOR"])(app);
                    styles.borrow_mut().push(label_style(app, "SEL"));
                }),
            ),
            (
                Some("M"),
                Some(&|app| {
                    check("MOV", &["NOR", "SEL"])(app);
                    // the colors of select mode
                    assert_eq!(label_style(app, "MOV"), styles.borrow()[1]);
                    assert_ne!(styles.borrow()[0], styles.borrow()[1]);
                }),
            ),
            (
                Some("<esc>"),
                Some(&|app| check("NOR", &["MOV", "SEL"])(app)),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_move_mode_is_only_shown_while_it_is_active() -> anyhow::Result<()> {
    // other sticky modes keep the name of the editor mode: only the move mode has a label
    let mut app = app("a\n#[|b]#\nc\n")?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("Z"),
                Some(&|app| {
                    let line = statusline(app);
                    assert!(line.contains(" NOR ") && !line.contains("MOV"), "{line:?}");
                }),
            ),
            (Some("<esc>"), None),
            (
                Some("M<esc>"),
                Some(&|app| assert!(!statusline(app).contains("MOV"))),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_infobox_of_the_move_mode() -> anyhow::Result<()> {
    let mut app = app("a\n#[|b]#\nc\n")?;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("M"),
                Some(&|app| {
                    let rows = screen_rows(app);
                    let on_screen = |text: &str| rows.iter().any(|row| row.contains(text));
                    assert!(on_screen("Move"), "{rows:#?}");
                    assert!(on_screen("Move selected lines down"), "{rows:#?}");
                    assert!(on_screen("Move selected lines up"), "{rows:#?}");
                }),
            ),
            // it stays while moving
            (
                Some("j"),
                Some(&|app| {
                    let rows = screen_rows(app);
                    assert!(rows
                        .iter()
                        .any(|row| row.contains("Move selected lines up")));
                }),
            ),
            (
                Some("<esc>"),
                Some(&|app| {
                    let rows = screen_rows(app);
                    assert!(
                        !rows.iter().any(|row| row.contains("Move selected lines")),
                        "{rows:#?}"
                    );
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_infobox_of_the_move_mode_at_a_narrow_width() -> anyhow::Result<()> {
    let mut app = app("a\n#[|b]#\nc\n")?;
    app.resize_screen(50, 20);
    test_key_sequences(
        &mut app,
        vec![(
            Some("M"),
            Some(&|app| {
                let rows = screen_rows(app);
                assert!(rows.iter().any(|row| row.contains("Move")), "{rows:#?}");
                assert!(rows.iter().any(|row| row.contains("MOV")), "{rows:#?}");
            }),
        )],
        false,
    )
    .await
}

// The typable commands.

#[tokio::test(flavor = "multi_thread")]
async fn the_typable_commands() -> anyhow::Result<()> {
    run_in_mode(
        "a\nb#[|b]#\nc\nd\n",
        ":move-lines-down<ret>",
        "a\nc\nb#[|b]#\nd\n",
        1,
    )
    .await?;
    run_in_mode(
        "a\nb#[|b]#\nc\nd\n",
        ":move-lines-up<ret>",
        "b#[|b]#\na\nc\nd\n",
        1,
    )
    .await?;
    run_in_mode(
        "#[|a]#\nb\nc\nd\ne\n",
        ":move-lines-down 3<ret>",
        "b\nc\nd\n#[|a]#\ne\n",
        1,
    )
    .await?;
    run_in_mode(
        "a\nb\nc\nd\n#[|e]#\n",
        ":move-lines-up 3<ret>",
        "a\n#[|e]#\nb\nc\nd\n",
        1,
    )
    .await?;
    // clamped by the room of the last block, like the keys, and several selections
    run_in_mode(
        "a\n#[|b]#\nc\n#(|d)#\ne\n",
        ":move-lines-down 9<ret>",
        "a\nc\n#[|b]#\ne\n#(|d)#\n",
        1,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_typable_commands_with_a_bad_count_do_nothing() -> anyhow::Result<()> {
    for command in [
        ":move-lines-down 0<ret>",
        ":move-lines-up x<ret>",
        ":move-lines-up -1<ret>",
        ":move-lines-down 1 2<ret>",
    ] {
        no_change("a\n#[|b]#\nc\n", command).await?;
    }
    // at the edge: nothing, and no history entry
    no_change("#[|a]#\nb\nc\n", ":move-lines-up<ret>").await?;
    no_change("a\nb\n#[|c]#\n", ":move-lines-down 2<ret>").await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_typable_commands_leave_the_registers_alone() -> anyhow::Result<()> {
    run(
        "#[|a]#\nb\nc\n",
        "xy:move-lines-down<ret>p",
        "b\na\n#[a\n|]#c\n",
    )
    .await
}

// The keys.

#[test]
fn the_keys() {
    use helix_term::keymap::{default, KeyTrie, KeyTrieNode};

    fn walk(node: &KeyTrieNode, path: &mut Vec<String>, found: &mut Vec<(String, String)>) {
        for (key, trie) in node.iter() {
            path.push(key.to_string());
            match trie {
                KeyTrie::MappableCommand(command) => {
                    found.push((path.join(" "), command.name().to_string()))
                }
                KeyTrie::Sequence(commands) => {
                    for command in commands {
                        found.push((path.join(" "), command.name().to_string()))
                    }
                }
                KeyTrie::Node(node) => walk(node, path, found),
            }
            path.pop();
        }
    }

    let keymaps = default();
    for mode in [Mode::Normal, Mode::Select, Mode::Insert] {
        let command = |key: &str| {
            let key = parse_macro(key).unwrap();
            match keymaps[&mode].search(&key) {
                Some(KeyTrie::MappableCommand(command)) => command.name().to_string(),
                other => format!("{other:?}"),
            }
        };

        // the commands are only in the move mode, no Alt (or Ctrl) keys
        let mut found = Vec::new();
        let KeyTrie::Node(root) = &keymaps[&mode] else {
            panic!()
        };
        walk(root, &mut Vec::new(), &mut found);
        let mut moves: Vec<_> = found
            .into_iter()
            .filter(|(_, command)| command.starts_with("move_lines"))
            .collect();
        moves.sort();
        let expected: &[(&str, &str)] = if mode == Mode::Insert {
            &[]
        } else {
            &[
                ("M down", "move_lines_down"),
                ("M j", "move_lines_down"),
                ("M k", "move_lines_up"),
                ("M up", "move_lines_up"),
            ]
        };
        let expected: Vec<_> = expected
            .iter()
            .map(|(keys, command)| (keys.to_string(), command.to_string()))
            .collect();
        assert_eq!(moves, expected, "{mode:?}");

        if mode == Mode::Insert {
            continue;
        }
        // A-j and A-k are not bound, A-up and A-down are the syntax tree selection like upstream
        for key in ["<A-j>", "<A-k>"] {
            let key = parse_macro(key).unwrap();
            assert!(keymaps[&mode].search(&key).is_none(), "{key:?} {mode:?}");
        }
        assert_eq!(command("<A-o>"), "expand_selection", "{mode:?}");
        assert_eq!(command("<A-up>"), "expand_selection", "{mode:?}");
        assert_eq!(command("<A-i>"), "shrink_selection", "{mode:?}");
        assert_eq!(command("<A-down>"), "shrink_selection", "{mode:?}");

        // `M` is a sticky node, with the label of the statusline
        let Some(KeyTrie::Node(node)) = keymaps[&mode].search(&parse_macro("M").unwrap()) else {
            panic!("{mode:?}")
        };
        assert!(node.is_sticky);
        assert_eq!(node.mode_label, Some("MOV"));
        assert_eq!(node.infobox().title, "Move");
        // (the view mode is sticky too, and has no label)
        let Some(KeyTrie::Node(view)) = keymaps[&mode].search(&parse_macro("Z").unwrap()) else {
            panic!("{mode:?}")
        };
        assert!(view.is_sticky && view.mode_label.is_none());
    }
}
