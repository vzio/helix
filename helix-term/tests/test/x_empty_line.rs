//! `x` (`extend_line_below`) on an empty line selects only that line
//! (<https://github.com/helix-editor/helix/issues/3077>).

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn x_selects_only_the_empty_line() -> anyhow::Result<()> {
    let text = "a\n#[\n|]#b\nc\nd\n";

    test((text, "xd", "a\n#[b|]#\nc\nd\n")).await?;
    test((text, "x", "a\n#[\n|]#b\nc\nd\n")).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn xx_extends_one_line_at_a_time() -> anyhow::Result<()> {
    let text = "a\n#[\n|]#b\nc\nd\n";

    test((text, "xx", "a\n#[\nb\n|]#c\nd\n")).await?;
    test((text, "xxx", "a\n#[\nb\nc\n|]#d\n")).await?;
    test((text, "xxxd", "a\n#[d|]#\n")).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn count_selects_exactly_count_lines() -> anyhow::Result<()> {
    let text = "a\n#[\n|]#b\nc\nd\ne\n";

    test((text, "1x", "a\n#[\n|]#b\nc\nd\ne\n")).await?;
    test((text, "3x", "a\n#[\nb\nc\n|]#d\ne\n")).await?;
    test((text, "3xd", "a\n#[d|]#\ne\n")).await?;
    // `x` after a count extends from the selected lines, as for any other lines
    test((text, "2xx", "a\n#[\nb\nc\n|]#d\ne\n")).await?;
    test((text, "2x2x", "a\n#[\nb\nc\nd\n|]#e\n")).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn x_k_j_x_selects_one_line() -> anyhow::Result<()> {
    let text = "a\n#[\n|]#b\nc\n";

    test((text, "xkjx", "a\n#[\n|]#b\nc\n")).await?;
    test((text, "xkjxd", "a\n#[b|]#\nc\n")).await?;
    test((text, "xjkx", "a\n#[\n|]#b\nc\n")).await?;
    test((text, "xjkxd", "a\n#[b|]#\nc\n")).await?;
    // the same, but with the other movement keys
    test((text, "xkjxx", "a\n#[\nb\n|]#c\n")).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_forgets_the_selected_line() -> anyhow::Result<()> {
    // x, change nothing visible, then x again: still a fresh `x` after an edit and undo
    test(("a\n#[\n|]#b\nc\n", "xdux", "a\n#[\n|]#b\nc\n")).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_line_at_the_end_of_the_file() -> anyhow::Result<()> {
    // the last line (after the last line ending) is empty and has no line ending to select
    test(("a\n#[\n|]#", "x", "a\n#[\n|]#")).await?;
    test(("a\n#[\n|]#", "xd", "a\n#[|]#")).await?;
    test(("a\n#[\n|]#", "xx", "a\n#[\n|]#")).await?;
    test(("a\n#[\n|]#", "3x", "a\n#[\n|]#")).await?;
    test(("a\n\n#[|]#", "x", "a\n\n#[|]#")).await?;
    test(("a\n\n#[|]#", "xx", "a\n\n#[|]#")).await?;
    test(("a\n\n#[|]#", "xd", "a\n\n#[|]#")).await?;
    test(("#[\n|]#", "x", "#[\n|]#")).await?;
    test(TestCase {
        in_text: "\n".into(),
        in_selection: Selection::single(0, 1),
        in_keys: "xd".into(),
        out_text: String::new(),
        out_selection: Selection::single(0, 0),
        line_feed_handling: LineFeedHandling::AsIs,
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn whitespace_only_line_is_a_text_line() -> anyhow::Result<()> {
    let text = "a\n#[ |]# \nb\nc\n";

    test((text, "x", "a\n#[  \n|]#b\nc\n")).await?;
    test((text, "xx", "a\n#[  \nb\n|]#c\n")).await?;
    test((text, "3x", "a\n#[  \nb\nc\n|]#")).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn multiple_cursors() -> anyhow::Result<()> {
    // two cursors on empty lines and one on a text line, far enough apart not to merge
    let text = "#[a|]#bc\nd\ne\nf\n#(\n|)#g\nh\ni\n#(\n|)#j\nk\nl\n";

    test((
        text,
        "x",
        "#[abc\n|]#d\ne\nf\n#(\n|)#g\nh\ni\n#(\n|)#j\nk\nl\n",
    ))
    .await?;
    test((
        text,
        "xx",
        "#[abc\nd\n|]#e\nf\n#(\ng\n|)#h\ni\n#(\nj\n|)#k\nl\n",
    ))
    .await?;
    test((
        text,
        "xxx",
        "#[abc\nd\ne\n|]#f\n#(\ng\nh\n|)#i\n#(\nj\nk\n|)#l\n",
    ))
    .await?;
    test((
        text,
        "2x",
        "#[abc\nd\n|]#e\nf\n#(\ng\n|)#h\ni\n#(\nj\n|)#k\nl\n",
    ))
    .await?;
    test((text, "xd", "#[d|]#\ne\nf\n#(g|)#\nh\ni\n#(j|)#\nk\nl\n")).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn crlf_empty_line() -> anyhow::Result<()> {
    // "a\r\n" (0..3), "\r\n" (3..5), "b\r\n" (5..8), "c\r\n" (8..11)
    let case = |keys: &str, out_text: &str, from: usize, to: usize| TestCase {
        in_text: "a\r\n\r\nb\r\nc\r\n".into(),
        in_selection: Selection::single(3, 5),
        in_keys: keys.into(),
        out_text: out_text.into(),
        out_selection: Selection::single(from, to),
        line_feed_handling: LineFeedHandling::AsIs,
    };

    test(case("x", "a\r\n\r\nb\r\nc\r\n", 3, 5)).await?;
    test(case("xx", "a\r\n\r\nb\r\nc\r\n", 3, 8)).await?;
    test(case("xxx", "a\r\n\r\nb\r\nc\r\n", 3, 11)).await?;
    test(case("3x", "a\r\n\r\nb\r\nc\r\n", 3, 11)).await?;
    test(case("xd", "a\r\nb\r\nc\r\n", 3, 4)).await?;
    test(case("xkjx", "a\r\n\r\nb\r\nc\r\n", 3, 5)).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn text_lines_are_unchanged() -> anyhow::Result<()> {
    let text = "#[a|]#bc\ndef\nghi\njkl\n";

    test((text, "x", "#[abc\n|]#def\nghi\njkl\n")).await?;
    test((text, "xx", "#[abc\ndef\n|]#ghi\njkl\n")).await?;
    test((text, "xxx", "#[abc\ndef\nghi\n|]#jkl\n")).await?;
    test((text, "3x", "#[abc\ndef\nghi\n|]#jkl\n")).await?;
    test((text, "xd", "#[d|]#ef\nghi\njkl\n")).await?;
    // a selection of whole lines that is not an empty line extends, marked or not
    test(("#[abc\n|]#def\nghi\n", "x", "#[abc\ndef\n|]#ghi\n")).await?;
    // the lines above: `x` extends a backwards selection like any other
    test(("abc\n#[|def\n]#ghi\n", "x", "abc\n#[def\nghi\n|]#")).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn other_line_commands_are_unchanged() -> anyhow::Result<()> {
    // `X` keeps the empty line, whatever was selected before
    test(("a\n#[\n|]#b\nc\n", "X", "a\n#[\n|]#b\nc\n")).await?;
    test(("a\n#[\n|]#b\nc\n", "Xd", "a\n#[b|]#\nc\n")).await?;
    // `X` is not `x`: `x` after it selects just the empty line, `X` after `x` keeps it
    test(("a\n#[\n|]#b\nc\n", "Xx", "a\n#[\n|]#b\nc\n")).await?;
    test(("a\n#[\n|]#b\nc\n", "xX", "a\n#[\n|]#b\nc\n")).await?;
    test(("a\n#[\n|]#b\nc\n", "Xxd", "a\n#[b|]#\nc\n")).await?;
    Ok(())
}
