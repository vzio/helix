//! History (`ga`, `gA` and its sticky state) and bookmarks (`C-s`, `C-i`, `C-o`, `space j`), end to
//! end through the key events of the application, with real files where several buffers matter.

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use termina::event::{Event, KeyEvent};
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::wrappers::UnboundedReceiverStream;
use vortex_term::{application::Application, history::picker_rows};
use vortex_view::{
    current_ref,
    graphics::Style,
    history::{Jump, Place},
    input::parse_macro,
};

use super::helpers::AppBuilder;

/// The editor driven key by key, so that a test looks at it between two batches of keys.
struct H {
    app: Application,
    tx: UnboundedSender<io::Result<Event>>,
    rx: UnboundedReceiverStream<io::Result<Event>>,
}

impl H {
    fn new(builder: AppBuilder) -> anyhow::Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            app: builder.build()?,
            tx,
            rx: UnboundedReceiverStream::new(rx),
        })
    }

    /// An editor with `text` in a buffer without a file.
    fn scratch(text: &str) -> anyhow::Result<Self> {
        let (first, rest) = text.split_at(1);
        Self::new(AppBuilder::new().with_input_text(format!("#[|{first}]#{rest}")))
    }

    /// An editor with the file open.
    fn file(path: &Path) -> anyhow::Result<Self> {
        Self::new(AppBuilder::new().with_file(path, None))
    }

    /// Sends keys (macro notation) and runs the editor until it is idle. The status is cleared
    /// first, so that `status()` is what these keys said.
    async fn keys(&mut self, keys: &str) -> &mut Self {
        self.app.editor.clear_status();
        for key in parse_macro(keys).unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
        let idle = tokio::time::timeout(
            Duration::from_secs(5),
            self.app.event_loop_until_idle(&mut self.rx),
        );
        assert!(idle.await.expect("the editor never went idle"));
        self
    }

    /// Lets jobs and pickers catch up, for `duration`.
    async fn settle(&mut self, duration: Duration) -> &mut Self {
        let start = std::time::Instant::now();
        while start.elapsed() < duration {
            let step = tokio::time::timeout(
                Duration::from_millis(50),
                self.app.event_loop_until_idle(&mut self.rx),
            );
            let _ = step.await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self
    }

    /// Opens the picker with `keys`, types `query`, waits for the matches and presses `Enter`.
    async fn pick(&mut self, keys: &str, query: &str) -> &mut Self {
        self.keys(keys).await;
        self.keys(query).await;
        self.settle(Duration::from_millis(300)).await;
        self.keys("<ret>").await;
        self.settle(Duration::from_millis(100)).await
    }

    async fn finish(mut self) -> anyhow::Result<()> {
        for key in parse_macro("<esc><esc>:q!<ret>")? {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key))))?;
        }
        let _ =
            tokio::time::timeout(Duration::from_secs(2), self.app.event_loop(&mut self.rx)).await;
        let errors = self.app.close().await;
        anyhow::ensure!(errors.is_empty(), "errors closing the app: {errors:?}");
        Ok(())
    }

    /// The line of the primary cursor.
    fn line(&self) -> usize {
        let (view, doc) = current_ref!(self.app.editor);
        let text = doc.text().slice(..);
        text.char_to_line(doc.selection(view.id).primary().cursor(text))
    }

    /// The column of the primary cursor.
    fn col(&self) -> usize {
        let (view, doc) = current_ref!(self.app.editor);
        let text = doc.text().slice(..);
        let cursor = doc.selection(view.id).primary().cursor(text);
        cursor - text.line_to_char(text.char_to_line(cursor))
    }

    /// The file name of the focused buffer, empty for a buffer without a file.
    fn name(&self) -> String {
        let (_, doc) = current_ref!(self.app.editor);
        doc.path()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The file and line of the focused cursor, like `alpha.txt:3` (line 3 is the third line).
    fn at(&self) -> String {
        format!("{}:{}", self.name(), self.line() + 1)
    }

    fn status(&self) -> String {
        self.app
            .editor
            .get_status()
            .map(|(message, _)| message.to_string())
            .unwrap_or_default()
    }

    /// The lines of the places in the focused view's history, oldest first.
    fn history(&self) -> Vec<usize> {
        let editor = &self.app.editor;
        let view = editor.tree.get(editor.tree.focus);
        view.history
            .places()
            .map(|place| match place {
                Place::Text { doc, selection } => {
                    let text = editor.documents[doc].text().slice(..);
                    text.char_to_line(selection.primary().cursor(text))
                }
                Place::Screen(_) => usize::MAX,
            })
            .collect()
    }

    fn walking(&self) -> bool {
        let editor = &self.app.editor;
        editor.tree.get(editor.tree.focus).history.is_walking()
    }

    fn menu_open(&self) -> bool {
        self.app.editor.history_menu
    }

    fn infobox(&self) -> Option<String> {
        self.app
            .editor
            .autoinfo
            .as_ref()
            .map(|info| format!("{}: {}", info.title, info.text))
    }

    /// The bookmarks as `(line, number)` of the focused buffer.
    fn numbers(&self) -> Vec<(usize, usize)> {
        let (_, doc) = current_ref!(self.app.editor);
        self.app.editor.bookmark_numbers(doc.id())
    }

    fn bookmarks(&self) -> usize {
        self.app.editor.bookmarks().len()
    }

    fn rows(&self) -> Vec<String> {
        picker_rows(&self.app.editor)
    }

    fn text(&self) -> String {
        let (_, doc) = current_ref!(self.app.editor);
        doc.text().to_string()
    }
}

/// `n` lines "line 1", "line 2"...
fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn lines_of(prefix: &str, n: usize) -> String {
    (1..=n).map(|i| format!("{prefix} {i}\n")).collect()
}

struct Files {
    _dir: tempfile::TempDir,
    alpha: PathBuf,
    beta: PathBuf,
}

/// `alpha.txt` and `beta.txt`, 20 lines each ("alpha 1"...).
fn files() -> anyhow::Result<Files> {
    let dir = tempfile::tempdir()?;
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    std::fs::write(&alpha, lines_of("alpha", 20))?;
    std::fs::write(&beta, lines_of("beta", 20))?;
    Ok(Files {
        _dir: dir,
        alpha,
        beta,
    })
}

fn open(path: &Path) -> String {
    format!(":o {}<ret>", path.display())
}

/// A file of 40 lines open with the cursor on its first.
fn forty() -> anyhow::Result<(tempfile::TempDir, H)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("forty.txt");
    std::fs::write(&path, lines(40))?;
    Ok((dir, H::file(&path)?))
}

// Recording

#[tokio::test(flavor = "multi_thread")]
async fn file_edges_and_searches_record_and_ga_returns() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;

    h.keys("5jge").await;
    let end = h.line();
    assert!(end > 5);
    assert_eq!(h.history(), [5]);
    h.keys("ga").await;
    assert_eq!(h.line(), 5);
    h.keys("ga").await;
    assert_eq!(h.line(), end, "ga again returns");

    // gg from the end, a search, n
    h.keys("gg").await;
    assert_eq!(h.line(), 0);
    h.keys("/line 3<ret>").await;
    assert_eq!(h.line(), 2);
    h.keys("n").await;
    assert_eq!(h.line(), 29, "the next match is 'line 30'");
    h.keys("N").await;
    assert_eq!(h.line(), 2);
    let recorded = h.history();
    assert_eq!(&recorded[recorded.len() - 3..], [0, 2, 29]);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_count_with_gg_and_a_line_command_record() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("12gg").await;
    assert_eq!(h.line(), 11);
    assert_eq!(h.history(), [0]);
    h.keys(":30<ret>").await;
    assert_eq!(h.line(), 29);
    assert_eq!(h.history(), [0, 11]);
    h.keys("20G").await;
    assert_eq!(h.line(), 19);
    assert_eq!(h.history(), [0, 11, 29]);
    h.keys("ga").await;
    assert_eq!(h.line(), 29);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn goto_last_modification_records() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("3jix<esc>ge").await;
    let end = h.line();
    assert_eq!(h.history(), [3]);
    h.keys("g.").await;
    assert_eq!(h.line(), 3);
    assert_eq!(h.history(), [3, end]);
    h.keys("ga").await;
    assert_eq!(h.line(), end);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn motions_record_nothing() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("jjjjhlwwbb}}{{fetl").await;
    h.keys("5j10k3j").await;
    assert!(h.history().is_empty());
    h.keys("ga").await;
    assert_eq!(h.status(), "No jumps to go back to");
    h.keys("gA").await;
    assert_eq!(h.status(), "No jumps to go back to");
    assert!(!h.menu_open(), "a failed gA opens no menu");
    assert!(h.infobox().is_none());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn selecting_scrolling_editing_and_star_record_nothing() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("3jvjj<esc>%<esc>5j<C-d><C-u><C-f><C-b>zt").await;
    h.keys("wiab<esc>o<esc>*").await;
    assert!(h.history().is_empty(), "{:?}", h.history());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn two_jumps_from_the_same_line_record_once() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("3j/line 11<ret>").await;
    assert_eq!(h.history(), [3]);
    // a motion back to line 3, then another jump from it: the same place replaces the last
    h.keys("7kge").await;
    assert_eq!(h.history(), [3], "{:?}", h.history());
    // from another line it is a new place
    h.keys("gg").await;
    let end = h.line();
    assert_eq!(end, 0);
    assert_eq!(h.history().len(), 2);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jump_that_lands_on_its_own_line_records_nothing() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    // gg on the first line, searching what is on the cursor's own line
    h.keys("gg").await;
    assert!(h.history().is_empty());
    h.keys("/line 1<ret>").await;
    // the first match after the cursor is line 10: that is a jump
    assert_eq!(h.line(), 9);
    assert_eq!(h.history(), [0]);
    h.keys(":10<ret>").await;
    assert_eq!(h.history(), [0], "line 10 is where we are");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn word_labels_do_not_record() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    let before = (h.line(), h.col());
    h.keys("gwab").await;
    assert_ne!((h.line(), h.col()), before, "the label moved the cursor");
    assert!(h.history().is_empty());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn tree_sitter_objects_do_not_record() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("lib.rs");
    std::fs::write(
        &path,
        "fn one() {\n    1;\n}\n\nfn two() {\n    2;\n}\n\nfn three() {\n    3;\n}\n",
    )?;
    let mut h = H::file(&path)?;
    h.keys("]f").await;
    assert!(h.line() > 0, "]f moved: the Rust grammar is not available");
    assert!(h.history().is_empty());
    h.keys("]f").await;
    assert!(h.history().is_empty());
    // a real jump does record from where the objects got to
    h.keys("gg").await;
    assert_eq!(h.history().len(), 1);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_column_goto_does_not_record() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("3g|").await;
    assert_eq!(h.col(), 2);
    assert!(h.history().is_empty());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pick_in_the_buffer_picker_records() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    // a second buffer, without going there: split and close, or open and come back
    h.keys(&open(&f.beta)).await;
    assert_eq!(h.name(), "beta.txt");
    h.keys("ga").await;
    assert_eq!(h.name(), "alpha.txt");
    assert!(h.walking(), "ga started a walk: beta is ahead");
    h.pick("<space>b", "beta").await;
    assert_eq!(h.name(), "beta.txt", "the picker went to beta");
    // the pick is a new jump: it replaces what was ahead of the walk
    assert_eq!(h.history(), [0]);
    assert!(!h.walking());
    h.keys("ga").await;
    assert_eq!(h.name(), "alpha.txt", "ga returns to where the pick left");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_palette_runs_commands_and_records_nothing_itself() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.pick("<space>?", "move_line_down").await;
    assert_eq!(h.line(), 1, "the palette ran a command: {}", h.status());
    assert!(h.history().is_empty());
    // a command that jumps records its own jump, once
    h.pick("<space>?", "goto_last_line").await;
    assert!(h.line() > 1, "{}", h.status());
    assert_eq!(h.history(), [1]);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn selecting_with_a_regex_records_nothing() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    let selections = |h: &H| {
        let (view, doc) = current_ref!(h.app.editor);
        doc.selection(view.id).len()
    };
    // `s` selects the matches inside the selection: "line 1", "line 10".."line 19"
    h.keys("%sline 1<ret>").await;
    assert_eq!(selections(&h), 11);
    assert!(h.history().is_empty(), "s: {:?}", h.history());
    // `S` splits it on the line ends
    h.keys("%S\\n<ret>").await;
    assert_eq!(selections(&h), 40);
    assert!(h.history().is_empty(), "S: {:?}", h.history());
    // `K` keeps the selections that match
    h.keys("Kline 2<ret>").await;
    assert_eq!(selections(&h), 11);
    assert!(h.history().is_empty(), "K: {:?}", h.history());
    h.keys("ga").await;
    assert_eq!(h.status(), "No jumps to go back to");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_view_remembers_thirty_places() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    for line in 2..=37 {
        h.keys(&format!(":{line}<ret>")).await;
    }
    assert_eq!(h.line(), 36);
    let places = h.history();
    assert_eq!(places.len(), 30);
    assert_eq!(places[0], 6, "the oldest six went");
    assert_eq!(places[29], 35);
    h.finish().await
}

// ga, gA

#[tokio::test(flavor = "multi_thread")]
async fn ga_toggles_between_two_places() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("4j/line 20<ret>").await;
    assert_eq!(h.line(), 19);
    h.keys("ga").await;
    assert_eq!(h.line(), 4);
    h.keys("ga").await;
    assert_eq!(h.line(), 19);
    h.keys("gaga").await;
    assert_eq!(h.line(), 19, "ga ga is back where you were");
    assert!(!h.menu_open(), "ga opens no menu");
    h.finish().await
}

/// Jumps from lines 3, 9, 19 to 29.
async fn walkable(h: &mut H) {
    h.keys("3j/line 10<ret>/line 20<ret>/line 30<ret>").await;
    assert_eq!(h.line(), 29);
    assert_eq!(h.history(), [3, 9, 19]);
}

#[tokio::test(flavor = "multi_thread")]
async fn ga_after_a_walk_returns_to_where_the_walk_started() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gAA").await;
    assert_eq!(h.line(), 9);
    assert!(h.walking());
    h.keys("<esc>ga").await;
    assert_eq!(h.line(), 29, "back to where the walk started");
    assert!(!h.walking());
    h.keys("ga").await;
    assert_eq!(h.line(), 19, "and ga is a plain toggle again");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn ga_right_after_the_sticky_state_also_returns() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    // `g` closes the state and runs as the key it is
    h.keys("gAAga").await;
    assert_eq!(h.line(), 29);
    assert!(!h.menu_open());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn gaa_walks_back_and_the_sticky_state_opens() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gA").await;
    assert_eq!(h.line(), 19);
    assert!(h.menu_open());
    assert_eq!(
        h.infobox().as_deref(),
        Some("History: A back · a forward · Esc done")
    );
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn gaaa_is_back_three_and_a_steps_forward() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gAAA").await;
    assert_eq!(h.line(), 3, "three places back");
    assert!(h.menu_open());
    h.keys("A").await;
    assert_eq!(h.line(), 3);
    assert_eq!(h.status(), "At the oldest jump");
    assert!(h.menu_open(), "the state stays at the end");
    h.keys("a").await;
    assert_eq!(h.line(), 9);
    h.keys("aa").await;
    assert_eq!(
        h.line(),
        29,
        "the last place forward is where the walk started"
    );
    assert!(!h.walking());
    h.keys("a").await;
    assert_eq!(h.status(), "At the newest jump");
    assert_eq!(h.line(), 29);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn back_three_and_forward_one() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gAAAa").await;
    assert_eq!(h.line(), 9);
    assert!(h.menu_open());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_closes_the_sticky_state() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gAA").await;
    assert!(h.menu_open());
    h.keys("<esc>").await;
    assert!(!h.menu_open());
    assert!(h.infobox().is_none());
    assert_eq!(h.line(), 9, "Esc stays where it is");
    // the keys are normal again: A is not a step
    h.keys("A<esc>").await;
    assert_eq!(h.line(), 9);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn another_key_closes_the_sticky_state_and_runs() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gAAj").await;
    assert!(!h.menu_open());
    assert!(h.infobox().is_none());
    assert_eq!(h.line(), 10, "back two (line 10), then down one");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_count_goes_back_that_many_places() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("2gA").await;
    assert_eq!(h.line(), 9);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_jump_while_walking_drops_the_places_ahead() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gAA<esc>").await;
    assert_eq!(h.line(), 9);
    h.keys("/line 35<ret>").await;
    assert_eq!(h.line(), 34);
    assert_eq!(h.history(), [3, 9], "the places ahead (19 and 29) are gone");
    assert!(!h.walking());
    // and there is nothing ahead to walk to
    h.keys("gAa").await;
    assert_eq!(h.line(), 34);
    h.keys("a").await;
    assert_eq!(h.status(), "At the newest jump");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn walking_back_records_the_present_once() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gA<esc>").await;
    assert_eq!(h.history(), [3, 9, 19, 29]);
    h.keys("gA<esc>").await;
    assert_eq!(h.history(), [3, 9, 19, 29]);
    h.finish().await
}

// Switching files

#[tokio::test(flavor = "multi_thread")]
async fn switching_files_records() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    h.keys("3j").await;
    h.keys(&open(&f.beta)).await;
    assert_eq!(h.at(), "beta.txt:1");
    h.keys("ga").await;
    assert_eq!(h.at(), "alpha.txt:4", "back to the first file at its line");
    h.keys("ga").await;
    assert_eq!(h.at(), "beta.txt:1");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn buffer_next_and_previous_record() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    h.keys(&open(&f.beta)).await;
    h.keys("gp").await;
    assert_eq!(h.name(), "alpha.txt");
    h.keys("gn").await;
    assert_eq!(h.name(), "beta.txt");
    assert!(h.history().len() >= 2 || !h.history().is_empty());
    h.keys("ga").await;
    assert_eq!(h.name(), "alpha.txt");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn history_is_per_split() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("ge").await;
    assert_eq!(h.history(), [0]);
    h.keys("<C-w>v").await;
    assert_eq!(h.history(), [0], "a new split starts with a copy");
    h.keys("/line 5<ret>").await;
    assert_eq!(h.history(), [0, 39], "the copy grows on its own");
    h.keys("<C-w>w").await;
    assert_eq!(h.history(), [0], "the first split is untouched");
    h.keys("gg").await;
    assert_eq!(h.history(), [0, 39]);
    h.keys("<C-w>w").await;
    assert_eq!(h.history(), [0, 39], "and so is the second");
    h.keys("ga").await;
    assert_eq!(h.line(), 39);
    h.finish().await
}

// Bookmarks

#[tokio::test(flavor = "multi_thread")]
async fn bookmarks_are_numbered_in_creation_order_across_files() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    h.keys("5j<C-s>").await;
    assert_eq!(h.status(), "Bookmark 1 set (1 in all)");
    h.keys(&open(&f.beta)).await;
    h.keys("2j<C-s>").await;
    assert_eq!(h.status(), "Bookmark 2 set (2 in all)");
    // `:o` starts at the top of the file; `gp` comes back to where the cursor was
    h.keys("gp").await;
    assert_eq!(h.at(), "alpha.txt:6");
    h.keys("2k<C-s>").await;
    assert_eq!(h.status(), "Bookmark 3 set (3 in all)");

    // creation order, not file and line order
    let rows = h.rows();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows[0].starts_with("1  ") && rows[0].contains("alpha.txt:6  alpha 6"));
    assert!(rows[1].starts_with("2  ") && rows[1].contains("beta.txt:3  beta 3"));
    assert!(rows[2].starts_with("3  ") && rows[2].contains("alpha.txt:4  alpha 4"));
    // and the gutter of alpha says so
    assert_eq!(h.numbers(), [(5, 1), (3, 3)]);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ring_wraps_with_ctrl_i_and_ctrl_o_and_tab_is_ctrl_i() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    h.keys("5j<C-s>").await;
    h.keys(&open(&f.beta)).await;
    h.keys("2j<C-s>").await;
    h.keys("gp").await;
    h.keys("2k<C-s>").await;
    assert_eq!(h.at(), "alpha.txt:4");

    // 3 -> 1 (wraps) -> 2 (another file) -> 3 -> 1
    h.keys("<C-i>").await;
    assert_eq!(
        (h.at().as_str(), h.status().as_str()),
        ("alpha.txt:6", "Bookmark 1 of 3")
    );
    h.keys("<C-i>").await;
    assert_eq!(
        (h.at().as_str(), h.status().as_str()),
        ("beta.txt:3", "Bookmark 2 of 3")
    );
    h.keys("<C-i>").await;
    assert_eq!(
        (h.at().as_str(), h.status().as_str()),
        ("alpha.txt:4", "Bookmark 3 of 3")
    );
    h.keys("<C-i>").await;
    assert_eq!(h.at(), "alpha.txt:6");

    // backwards: 1 -> 3 (wraps) -> 2 -> 1
    h.keys("<C-o>").await;
    assert_eq!(
        (h.at().as_str(), h.status().as_str()),
        ("alpha.txt:4", "Bookmark 3 of 3")
    );
    h.keys("<C-o>").await;
    assert_eq!(h.at(), "beta.txt:3");
    h.keys("<C-o>").await;
    assert_eq!(h.at(), "alpha.txt:6");

    // Tab does what C-i does
    h.keys("<tab>").await;
    assert_eq!(
        (h.at().as_str(), h.status().as_str()),
        ("beta.txt:3", "Bookmark 2 of 3")
    );
    h.keys("<tab><tab>").await;
    assert_eq!(h.at(), "alpha.txt:6");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ring_goes_to_the_first_non_blank_character() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("code.txt");
    std::fs::write(&path, "top\n        indented\nbottom\n")?;
    let mut h = H::file(&path)?;
    h.keys("jgl<C-s>gg").await;
    assert_eq!(h.line(), 0);
    h.keys("<C-i>").await;
    assert_eq!((h.line(), h.col()), (1, 8));
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn no_bookmarks_says_so() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("<C-i>").await;
    assert_eq!(h.status(), "No bookmarks");
    h.keys("<C-o>").await;
    assert_eq!(h.status(), "No bookmarks");
    h.keys("<space>j").await;
    assert_eq!(h.status(), "No bookmarks");
    assert_eq!(h.line(), 0);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_s_toggles_a_bookmark_off() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("3j<C-s>").await;
    assert_eq!(h.bookmarks(), 1);
    assert_eq!(h.numbers(), [(3, 1)]);
    h.keys("<C-s>").await;
    assert_eq!(h.status(), "Bookmark removed (0 in all)");
    assert_eq!(h.bookmarks(), 0);
    assert!(h.numbers().is_empty());
    // and it can be set again
    h.keys("<C-s>").await;
    assert_eq!(h.bookmarks(), 1);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn several_cursors_toggle_several_lines() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("CC<C-s>").await;
    assert_eq!(h.status(), "3 bookmarks set (3 in all)");
    assert_eq!(h.numbers(), [(0, 1), (1, 2), (2, 3)]);
    // the same three cursors again: all three go
    h.keys("<C-s>").await;
    assert_eq!(h.status(), "3 bookmarks removed (0 in all)");
    assert_eq!(h.bookmarks(), 0);
    // two of three already set: one set, two removed
    h.keys(",ggj<C-s>ggCC<C-s>").await;
    assert_eq!(h.status(), "2 bookmarks set, 1 removed (2 in all)");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_bookmark_closes_the_gap_in_the_numbers() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("2j<C-s>3j<C-s>3j<C-s>").await;
    assert_eq!(h.numbers(), [(2, 1), (5, 2), (8, 3)]);
    // the middle one off
    h.keys("3k<C-s>").await;
    assert_eq!(h.numbers(), [(2, 1), (8, 2)]);
    let rows = h.rows();
    assert!(
        rows[0].starts_with("1  ") && rows[0].contains("line 3"),
        "{rows:?}"
    );
    assert!(
        rows[1].starts_with("2  ") && rows[1].contains("line 9"),
        "{rows:?}"
    );
    // a new one goes last
    h.keys("jj<C-s>").await;
    assert_eq!(h.numbers(), [(2, 1), (8, 2), (7, 3)]);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn going_to_a_bookmark_records_a_history_jump() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("10j<C-s>10k").await;
    assert_eq!(h.line(), 0);
    assert!(h.history().is_empty(), "motions record nothing");
    h.keys("<C-i>").await;
    assert_eq!(h.line(), 10);
    assert_eq!(h.history(), [0]);
    h.keys("ga").await;
    assert_eq!(h.line(), 0, "ga returns");
    h.keys("ga").await;
    assert_eq!(h.line(), 10);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn going_to_a_bookmark_in_another_file_records_and_ga_returns() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.beta)?;
    h.keys("4j<C-s>").await;
    h.keys(&open(&f.alpha)).await;
    h.keys("2j<C-i>").await;
    assert_eq!(h.at(), "beta.txt:5");
    h.keys("ga").await;
    assert_eq!(h.at(), "alpha.txt:3");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bookmark_picker_lists_goes_and_removes() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("2j<C-s>5j<C-s>5j<C-s>gg").await;
    assert_eq!(h.bookmarks(), 3);
    assert!(h.rows()[2].contains("line 13"), "{:?}", h.rows());

    // C-x removes the selected one (the first), and the picker opens again
    h.keys("<space>j").await;
    h.settle(Duration::from_millis(300)).await;
    h.keys("<C-x>").await;
    h.settle(Duration::from_millis(300)).await;
    assert_eq!(h.bookmarks(), 2);
    assert_eq!(h.status(), "Bookmark 1 removed");
    let rows = h.rows();
    assert!(
        rows[0].starts_with("1  ") && rows[0].contains("line 8"),
        "{rows:?}"
    );
    assert!(
        rows[1].starts_with("2  ") && rows[1].contains("line 13"),
        "{rows:?}"
    );

    // the picker is up again: Enter goes to the selected one
    h.keys("<ret>").await;
    h.settle(Duration::from_millis(100)).await;
    assert_eq!(h.line(), 7);
    // the `gg` that went to the top, then the pick that left it
    assert_eq!(
        h.history(),
        [12, 0],
        "a pick of the bookmark picker records"
    );
    h.keys("ga").await;
    assert_eq!(h.line(), 0);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_file_keeps_its_bookmarks_and_opening_it_restores_them() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    h.keys(&open(&f.beta)).await;
    h.keys("2j<C-s>").await;
    assert_eq!(h.numbers(), [(2, 1)]);
    h.keys(":bc<ret>").await;
    assert_eq!(h.name(), "alpha.txt", "beta is closed");
    assert_eq!(h.app.editor.documents.len(), 1);
    // kept by path and line
    assert_eq!(h.bookmarks(), 1);
    assert!(h.rows()[0].contains("beta.txt:3  beta 3"), "{:?}", h.rows());
    assert_eq!(h.app.editor.bookmark_state.closed.len(), 1);
    // the ring opens the file again, with its bookmark on the buffer
    h.keys("<C-i>").await;
    assert_eq!(h.at(), "beta.txt:3");
    assert_eq!(h.numbers(), [(2, 1)]);
    assert!(h.app.editor.bookmark_state.closed.is_empty());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_a_file_again_restores_its_bookmarks_in_order() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    h.keys("<C-s>").await;
    h.keys(&open(&f.beta)).await;
    h.keys("4j<C-s>:bc<ret>").await;
    assert_eq!(h.name(), "alpha.txt");
    h.keys("3j<C-s>").await;
    // the closed file's bookmark is #2, in the middle
    assert_eq!(h.numbers(), [(0, 1), (3, 3)]);
    h.keys(&open(&f.beta)).await;
    assert_eq!(h.numbers(), [(4, 2)]);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_buffer_without_a_file_loses_its_bookmarks_when_it_closes() -> anyhow::Result<()> {
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    h.keys(":new<ret>").await;
    assert_eq!(h.name(), "");
    h.keys("ione<ret>two<esc><C-s>").await;
    assert_eq!(h.bookmarks(), 1);
    assert_eq!(h.rows().len(), 1);
    h.keys(":bc!<ret>").await;
    assert_eq!(h.name(), "alpha.txt");
    assert_eq!(h.bookmarks(), 0, "{:?}", h.rows());
    assert!(h.app.editor.bookmark_state.closed.is_empty());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn insert_mode_ctrl_s_commits_an_undo_checkpoint_and_sets_no_bookmark() -> anyhow::Result<()>
{
    let (_dir, mut h) = forty()?;
    h.keys("ia<C-s>b<esc>").await;
    assert_eq!(h.bookmarks(), 0);
    assert!(h.text().starts_with("abline 1"));
    h.keys("u").await;
    assert!(
        h.text().starts_with("aline 1"),
        "the checkpoint split the insert"
    );
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_s_in_select_mode_toggles_too() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("2jv<C-s>").await;
    assert_eq!(h.bookmarks(), 1);
    assert_eq!(h.numbers(), [(2, 1)]);
    h.keys("<C-s>").await;
    assert_eq!(h.bookmarks(), 0);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_removed_jumplist_keys_and_commands_are_gone() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    // C-o and C-i with no bookmarks are the ring, not a jumplist: nothing moves
    h.keys("ge").await;
    let end = h.line();
    h.keys("<C-o>").await;
    assert_eq!(h.line(), end);
    h.keys("<C-i>").await;
    assert_eq!(h.line(), end);
    for name in [
        "jump_forward",
        "jump_backward",
        "save_selection",
        "jumplist_picker",
    ] {
        assert!(
            vortex_term::commands::MappableCommand::STATIC_COMMAND_LIST
                .iter()
                .all(|command| command.name() != name),
            "{name} exists"
        );
    }
    h.finish().await
}

// Bookmarks and edits

fn ten() -> String {
    lines(10)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bookmark_stays_on_its_line_when_lines_are_opened_above() -> anyhow::Result<()> {
    let mut h = H::scratch(&ten())?;
    h.keys("gg3j<C-s>gg").await;
    assert_eq!(h.numbers(), [(3, 1)]);
    h.keys("O<esc>").await;
    assert_eq!(h.numbers(), [(4, 1)]);
    // a line opened below line 2: still above the bookmark
    h.keys("jjo<esc>").await;
    assert_eq!(h.numbers(), [(5, 1)]);
    // and one opened below it leaves it alone
    h.keys("6jo<esc>").await;
    assert_eq!(h.numbers(), [(5, 1)]);
    h.keys("ggO<esc>").await;
    assert_eq!(h.numbers(), [(6, 1)]);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bookmark_follows_its_line_when_lines_are_deleted_above() -> anyhow::Result<()> {
    let mut h = H::scratch(&ten())?;
    h.keys("gg5j<C-s>gg").await;
    assert_eq!(h.numbers(), [(5, 1)]);
    // in this fork x selects the line and d deletes it
    h.keys("xd").await;
    assert_eq!(h.numbers(), [(4, 1)]);
    assert!(h.rows()[0].contains("line 6"), "{:?}", h.rows());
    h.keys("xdxd").await;
    assert_eq!(h.numbers(), [(2, 1)]);
    // still on the same text
    h.keys("2j").await;
    assert!(h.text().lines().nth(2).unwrap().ends_with('6'));
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_bookmarked_line_removes_the_bookmark_and_undo_leaves_it_gone(
) -> anyhow::Result<()> {
    let mut h = H::scratch(&ten())?;
    h.keys("gg2j<C-s>5j<C-s>gg").await;
    assert_eq!(h.numbers(), [(2, 1), (7, 2)]);
    // delete line 3, the first bookmark's line
    h.keys("2jxd").await;
    assert_eq!(
        h.numbers(),
        [(6, 1)],
        "the first is gone, the second moved up and is #1 now"
    );
    assert_eq!(h.bookmarks(), 1);
    // undo brings the text back, not the bookmark; the other still marks its own line
    h.keys("u").await;
    assert!(h.text().contains("line 3\n"));
    assert_eq!(h.numbers(), [(7, 1)], "{:?}", h.numbers());
    h.keys("U").await;
    assert_eq!(h.numbers(), [(6, 1)]);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn joined_bookmarked_lines_keep_the_older_bookmark() -> anyhow::Result<()> {
    let mut h = H::scratch(&ten())?;
    // #1 on line 4, #2 on line 3
    h.keys("gg3j<C-s>k<C-s>").await;
    assert_eq!(h.numbers(), [(3, 1), (2, 2)]);
    // join the lines: both are on one line, the older (#1) stays
    h.keys("J").await;
    assert_eq!(h.bookmarks(), 1);
    assert_eq!(h.numbers().len(), 1);
    h.finish().await
}

// Palette and the sticky state share the keys

#[tokio::test(flavor = "multi_thread")]
async fn the_sticky_state_does_not_swallow_keys_after_it_closes() -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    walkable(&mut h).await;
    h.keys("gAA<esc>jj").await;
    assert_eq!(h.line(), 11);
    // a bare `a` after the state is the append command, not a step
    h.keys("a<esc>").await;
    assert_eq!(h.line(), 11);
    assert!(!h.menu_open());
    h.finish().await
}

// The Jump kinds are what the spec says

#[test]
fn the_motions_do_not_record_and_the_rest_do() {
    use vortex_view::history::records;
    for jump in [Jump::Object, Jump::Label, Jump::Column] {
        assert!(!records(jump), "{jump:?}");
    }
    for jump in [
        Jump::Goto,
        Jump::Search,
        Jump::FileEdge,
        Jump::Line,
        Jump::Modification,
        Jump::Diagnostic,
        Jump::Change,
        Jump::Pick,
        Jump::Switch,
        Jump::Screen,
        Jump::Bookmark,
    ] {
        assert!(records(jump), "{jump:?}");
    }
}

// The gutter

/// The cell of the diagnostics gutter (with the bookmarks) of a line of the focused view.
fn cell(app: &Application, line: usize) -> Option<(String, Style)> {
    let editor = &app.editor;
    let (view, doc) = current_ref!(editor);
    let mut gutter =
        vortex_view::gutter::diagnostics_or_breakpoints(editor, doc, view, &editor.theme, true);
    let mut out = String::new();
    let style = gutter(line, false, true, &mut out)?;
    Some((out, style))
}

fn diagnostic(
    line: usize,
    start: usize,
    severity: vortex_core::diagnostic::Severity,
) -> vortex_core::diagnostic::Diagnostic {
    use vortex_core::diagnostic::{Diagnostic, DiagnosticProvider, Range};
    Diagnostic {
        range: Range {
            start,
            end: start + 1,
        },
        ends_at_word: true,
        starts_at_word: true,
        zero_width: false,
        line,
        message: "a problem".to_owned(),
        severity: Some(severity),
        code: None,
        // not a language server's, so that it needs none to show
        provider: DiagnosticProvider::Conflict,
        tags: Vec::new(),
        source: None,
        data: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_gutter_shows_the_numbers_one_to_nine_and_then_a_marker() -> anyhow::Result<()> {
    let mut h = H::scratch(&lines(14))?;
    h.keys("gg").await;
    for _ in 0..12 {
        h.keys("<C-s>j").await;
    }
    let style = h.app.editor.theme.get("ui.gutter.bookmark");
    for line in 0..12 {
        let (text, got) = cell(&h.app, line).unwrap_or_else(|| panic!("line {line} has a cell"));
        let expected = if line < 9 {
            (line + 1).to_string()
        } else {
            vortex_view::gutter::BOOKMARK_SYMBOL.to_string()
        };
        assert_eq!(text, expected, "line {line}");
        assert_eq!(got, style, "line {line}");
    }
    assert!(cell(&h.app, 12).is_none());
    assert!(cell(&h.app, 13).is_none());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_gutter_numbers_follow_the_creation_order() -> anyhow::Result<()> {
    let mut h = H::scratch(&lines(6))?;
    h.keys("gg4j<C-s>2k<C-s>").await;
    assert_eq!(cell(&h.app, 4).unwrap().0, "1");
    assert_eq!(cell(&h.app, 2).unwrap().0, "2");
    h.keys("<C-s>").await;
    assert_eq!(cell(&h.app, 4).unwrap().0, "1");
    assert!(cell(&h.app, 2).is_none());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bookmark_on_a_line_with_a_diagnostic_takes_its_color() -> anyhow::Result<()> {
    use vortex_core::diagnostic::Severity;
    let mut h = H::scratch(&lines(6))?;
    h.keys("gg<C-s>j<C-s>j<C-s>").await;
    {
        let editor = &mut h.app.editor;
        let (_, doc) = vortex_view::current!(editor);
        let at = |line: usize| doc.text().line_to_char(line);
        let diagnostics = [
            diagnostic(1, at(1), Severity::Error),
            diagnostic(2, at(2), Severity::Warning),
        ];
        doc.replace_diagnostics(diagnostics, &[], None);
    }
    let theme = &h.app.editor.theme;
    let base = theme.get("ui.gutter.bookmark");
    let (text, plain) = cell(&h.app, 0).unwrap();
    assert_eq!((text.as_str(), plain), ("1", base));
    let (text, error) = cell(&h.app, 1).unwrap();
    assert_eq!(text, "2", "still the number, not the diagnostic's dot");
    assert_eq!(error.fg, theme.get("error").fg);
    assert_ne!(error.fg, base.fg);
    assert_eq!(
        error.add_modifier, base.add_modifier,
        "the rest of the style stays"
    );
    let (text, warning) = cell(&h.app, 2).unwrap();
    assert_eq!(text, "3");
    assert_eq!(warning.fg, theme.get("warning").fg);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diagnostic_without_a_bookmark_is_a_dot() -> anyhow::Result<()> {
    use vortex_core::diagnostic::Severity;
    let mut h = H::scratch(&lines(3))?;
    {
        let editor = &mut h.app.editor;
        let (_, doc) = vortex_view::current!(editor);
        let start = doc.text().line_to_char(1);
        doc.replace_diagnostics([diagnostic(1, start, Severity::Error)], &[], None);
    }
    assert_eq!(cell(&h.app, 1).unwrap().0, "●");
    h.keys("jj<C-s>").await;
    assert_eq!(
        cell(&h.app, 1).unwrap().0,
        "●",
        "another line's bookmark changes nothing"
    );
    h.keys("k<C-s>").await;
    assert_eq!(cell(&h.app, 1).unwrap().0, "2", "now the bookmark is on it");
    h.finish().await
}

/// The gutter's column is the bookmarks' outside debug mode (breakpoints aren't drawn there),
/// and the breakpoints' in debug mode (bookmarks aren't drawn there).
#[tokio::test(flavor = "multi_thread")]
async fn the_gutter_shows_bookmarks_outside_debug_mode_and_breakpoints_in_it() -> anyhow::Result<()>
{
    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    let path = {
        let (_, doc) = current_ref!(h.app.editor);
        doc.path().unwrap().to_owned()
    };
    h.app.editor.breakpoints.insert(
        path,
        vec![
            vortex_view::editor::Breakpoint {
                line: 1,
                verified: true,
                ..Default::default()
            },
            vortex_view::editor::Breakpoint {
                line: 2,
                verified: true,
                ..Default::default()
            },
        ],
    );
    assert!(h.app.editor.debug_adapters.get_active_client().is_none());
    // outside debug mode a breakpoint isn't drawn
    assert_eq!(cell(&h.app, 1), None);
    assert_eq!(cell(&h.app, 2), None);
    // a bookmark on line 2 only
    h.keys("j<C-s>").await;
    assert_eq!(cell(&h.app, 1).unwrap().0, "1", "the bookmark shows");
    assert_eq!(cell(&h.app, 2), None, "the other line's breakpoint doesn't");
    // in debug mode the breakpoints own the column
    h.keys("<space>G").await;
    assert!(h.app.editor.debug.mode_on());
    assert_eq!(
        cell(&h.app, 1).unwrap().0,
        "●",
        "the breakpoint, not the bookmark"
    );
    assert_eq!(cell(&h.app, 2).unwrap().0, "●");
    h.keys("<esc>").await;
    assert_eq!(cell(&h.app, 1).unwrap().0, "1", "the bookmark again");
    h.finish().await
}

/// The gutter of a line with a breakpoint and a bookmark while a debug session is active: the
/// bookmark outside debug mode, the breakpoint in it (the fake adapter of the debug tests is the
/// session).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_a_debug_session_the_gutter_follows_debug_mode() -> anyhow::Result<()> {
    use vortex_core::syntax::config::DebugAdapterConfig;
    use vortex_view::debug::Status;

    use super::debug::fake_adapter::{FakeAdapter, Script, Stop};

    let f = files()?;
    let mut h = H::file(&f.alpha)?;
    let path = {
        let (_, doc) = current_ref!(h.app.editor);
        doc.path().unwrap().to_owned()
    };
    let fake = FakeAdapter::start(Script::new(path.to_string_lossy()).stop(Stop::new(15, "entry")))
        .await?;
    h.app.editor.breakpoints.insert(
        path,
        vec![vortex_view::editor::Breakpoint {
            line: 2,
            ..Default::default()
        }],
    );
    // bookmarks on line 3 (a breakpoint is there too) and line 5 (none)
    h.keys("2j<C-s>2j<C-s>").await;
    assert_eq!(h.numbers(), [(2, 1), (4, 2)]);
    assert_eq!(
        cell(&h.app, 2).unwrap().0,
        "1",
        "no session: the bookmark wins"
    );

    // the test is about the gutter, not playback
    h.app.editor.debug.engine.kind = vortex_view::debug::engine::EngineKind::Stepping;
    vortex_term::debug::start(
        &mut h.app.editor,
        Some(fake.addr),
        &DebugAdapterConfig {
            name: "fake".into(),
            transport: "tcp".into(),
            command: String::new(),
            args: Vec::new(),
            port_arg: None,
            templates: Vec::new(),
            quirks: Default::default(),
            own_paths: None,
            std_paths: None,
            lib_paths: None,
            capabilities: None,
        },
        "fake",
        "launch",
        serde_json::json!({"program": "prog"}),
    )?;
    let start = std::time::Instant::now();
    while !(h.app.editor.debug_adapters.get_active_client().is_some()
        && matches!(h.app.editor.debug.status, Status::Stopped { .. }))
    {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the session never stopped: {:?}",
            h.app.editor.debug.status
        );
        h.settle(Duration::from_millis(100)).await;
    }

    assert_eq!(
        cell(&h.app, 2).unwrap().0,
        "1",
        "a session outside debug mode: the bookmark"
    );
    h.keys("<space>G").await;
    assert_eq!(
        cell(&h.app, 2).unwrap().0,
        "●",
        "debug mode: the breakpoint"
    );
    assert_eq!(
        cell(&h.app, 4),
        None,
        "a bookmark alone isn't drawn in debug mode"
    );
    h.keys("<esc>").await;
    assert_eq!(cell(&h.app, 4).unwrap().0, "2", "outside it again");
    assert_eq!(h.numbers(), [(2, 1), (4, 2)], "the bookmarks are kept");
    h.finish().await
}

/// The rows of the screen.
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

fn on_screen(app: &Application, text: &str) -> bool {
    screen_rows(app).iter().any(|row| row.contains(text))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bookmark_picker_is_modal_has_no_header_and_question_mark_shows_its_keys(
) -> anyhow::Result<()> {
    let (_dir, mut h) = forty()?;
    h.keys("2j<C-s>5j<C-s>gg").await;
    assert_eq!(h.bookmarks(), 2);

    h.keys("<space>j").await;
    h.settle(Duration::from_millis(300)).await;
    // no header with the names of the columns, and nothing in the status line
    let rows = screen_rows(&h.app);
    assert!(!rows.iter().any(|row| row.contains(" file ")), "{rows:#?}");
    assert!(
        on_screen(&h.app, "forty.txt:3") && on_screen(&h.app, "forty.txt:8"),
        "{rows:#?}"
    );
    let top = rows.iter().position(|row| row.starts_with('┌')).unwrap();
    assert!(rows[top + 3].contains("forty.txt:3"), "{rows:#?}");
    assert_eq!(h.status(), "");

    // normal mode: `?` lists the keys, any key closes it and goes no further
    h.keys("?").await;
    assert!(on_screen(&h.app, "Bookmarks"));
    assert!(on_screen(&h.app, "any key closes"));
    assert!(on_screen(&h.app, "remove it"));
    h.keys("j").await;
    assert!(!on_screen(&h.app, "any key closes"));
    assert!(on_screen(&h.app, "forty.txt:8"));
    assert_eq!(h.bookmarks(), 2);

    // filter mode: `?` is typed into the query
    h.keys("s?").await;
    assert!(!on_screen(&h.app, "any key closes"));
    h.finish().await
}
