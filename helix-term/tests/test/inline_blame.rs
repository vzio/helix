use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use helix_term::{application::Application, config::Config, keymap::KeyTrie};
use helix_view::{
    blame::{BlameState, LineBlameResult},
    document::Mode,
    editor::{InlineBlameConfig, InlineBlameShow},
    input::parse_macro,
};
use tokio_stream::wrappers::UnboundedReceiverStream;

#[cfg(windows)]
use crossterm::event::{Event, KeyEvent};
#[cfg(not(windows))]
use termina::event::{Event, KeyEvent};

use super::*;

#[cfg(windows)]
const FOCUS_EVENTS: (Event, Event) = (Event::FocusLost, Event::FocusGained);
#[cfg(not(windows))]
const FOCUS_EVENTS: (Event, Event) = (Event::FocusOut, Event::FocusIn);

/// The format the tests use, without anything that changes over time.
const FORMAT: &str = "{author}|{title}";

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("git can be run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(dir: &Path, author: &str, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            &format!("user.name={author}"),
            "-c",
            &format!("user.email={}@example.com", author.replace(' ', ".")),
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

/// A repository with `numbers.txt` committed by Jane Doe.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let file = dir.path().join("numbers.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    commit(dir.path(), "Jane Doe", "Add numbers");
    // not through a symlinked temp dir, paths are compared
    let file = std::fs::canonicalize(file).unwrap();
    (dir, file)
}

fn config(enable: bool, show: InlineBlameShow, delay: u64) -> Config {
    let mut config = test_config();
    config.editor.inline_blame = InlineBlameConfig {
        enable,
        show,
        format: FORMAT.to_owned(),
        delay,
    };
    config
}

fn app_with(file: &Path, config: Config) -> Application {
    helpers::AppBuilder::new()
        .with_config(config)
        .with_file(file, None)
        .build()
        .unwrap()
}

/// A config with `blame_line` on F2.
fn config_with_blame_line_key(mut config: Config) -> Config {
    let keys: HashMap<Mode, KeyTrie> = toml::from_str("[normal]\nF2 = \"blame_line\"").unwrap();
    config.keys = keys;
    config
}

/// Drives an application with keys and waits for the results of background tasks.
struct Session {
    app: Application,
    tx: tokio::sync::mpsc::UnboundedSender<std::io::Result<Event>>,
    input: UnboundedReceiverStream<std::io::Result<Event>>,
}

impl Session {
    fn new(app: Application) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            app,
            tx,
            input: UnboundedReceiverStream::new(rx),
        }
    }

    async fn keys(&mut self, keys: &str) {
        for key in parse_macro(keys).unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    async fn event(&mut self, event: Event) {
        self.tx.send(Ok(event)).unwrap();
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    /// Run until `condition` is true.
    async fn wait_for(&mut self, what: &str, condition: impl Fn(&Application) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition(&self.app) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}, status {:?}",
                doc(&self.app).blame.state(),
                status(&self.app)
            );
            // (the idle timer is off after the last time it ran out)
            self.app.editor.reset_idle_timer();
            assert!(self.app.event_loop_until_idle(&mut self.input).await);
        }
    }

    async fn wait_for_state(&mut self, what: &str, state: impl Fn(&BlameState) -> bool) {
        self.wait_for(what, |app| {
            state(helix_view::doc!(app.editor).blame.state())
        })
        .await;
    }

    async fn wait_until_ready(&mut self) {
        self.wait_for_state("blame", |state| matches!(state, BlameState::Ready(_)))
            .await;
    }

    async fn close(mut self) {
        for key in parse_macro("<esc>:q!<ret>").unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
        // (this one ends the application)
        while self.app.event_loop_until_idle(&mut self.input).await {}
        let errors = self.app.close().await;
        assert!(errors.is_empty(), "{errors:?}");
    }
}

fn doc(app: &Application) -> &helix_view::Document {
    helix_view::doc!(app.editor)
}

fn shown(app: &Application, line: usize) -> Option<String> {
    doc(app).line_blame(line).display(FORMAT)
}

fn status(app: &Application) -> String {
    app.editor
        .get_status()
        .map(|(msg, _)| msg.to_string())
        .unwrap_or_default()
}

fn enabled(app: &Application) -> bool {
    app.editor.config().inline_blame.enable
}

fn ready_blame(app: &Application) -> Arc<helix_vcs::FileBlame> {
    match doc(app).blame.state() {
        BlameState::Ready(blame) => blame.clone(),
        state => panic!("blame is not ready: {state:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn toggle_command_and_space_b_flip_the_config() {
    let mut session = Session::new(app_with(
        &std::env::temp_dir().join("inline-blame-does-not-exist.txt"),
        config(false, InlineBlameShow::CursorLine, 0),
    ));
    assert!(!enabled(&session.app));

    session.keys(":toggle-inline-blame<ret>").await;
    assert!(enabled(&session.app));
    session.keys(":toggle-inline-blame<ret>").await;
    assert!(!enabled(&session.app));

    session.keys("<space>B").await;
    assert!(enabled(&session.app));
    helpers::assert_status_not_error(&session.app.editor);
    session.keys("<space>B").await;
    assert!(!enabled(&session.app));

    // select mode too
    session.keys("v<space>B").await;
    assert!(enabled(&session.app));
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn set_changes_the_config() {
    let mut session = Session::new(helpers::AppBuilder::new().build().unwrap());
    let config = || session.app.editor.config().inline_blame.clone();
    assert_eq!(config(), InlineBlameConfig::default());

    session.keys(":set inline-blame.enable true<ret>").await;
    assert!(session.app.editor.config().inline_blame.enable);
    session.keys(":set inline-blame.show all-lines<ret>").await;
    assert_eq!(
        session.app.editor.config().inline_blame.show,
        InlineBlameShow::AllLines
    );
    session.keys(":set inline-blame.delay 25<ret>").await;
    assert_eq!(session.app.editor.config().inline_blame.delay, 25);
    session
        .keys(":set inline-blame.format {author} {date}<ret>")
        .await;
    assert_eq!(
        session.app.editor.config().inline_blame.format,
        "{author} {date}"
    );
    session.keys(":toggle inline-blame.enable<ret>").await;
    assert!(!session.app.editor.config().inline_blame.enable);
    helpers::assert_status_not_error(&session.app.editor);
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn enabling_computes_the_blame_of_open_files() {
    let (_dir, file) = repo();
    let mut session = Session::new(app_with(
        &file,
        config(false, InlineBlameShow::CursorLine, 0),
    ));

    // nothing is requested while the feature is off
    session.keys("jk").await;
    assert!(matches!(
        doc(&session.app).blame.state(),
        BlameState::Unknown
    ));
    assert!(!doc(&session.app).blame.is_in_flight());

    session.keys(":toggle-inline-blame<ret>").await;
    session.wait_until_ready().await;
    assert_eq!(
        shown(&session.app, 0).as_deref(),
        Some("Jane Doe|Add numbers")
    );
    assert_eq!(
        shown(&session.app, 2).as_deref(),
        Some("Jane Doe|Add numbers")
    );
    // there is no 4th line, nothing known about it
    assert_eq!(doc(&session.app).line_blame(3), LineBlameResult::Nothing);
    assert_eq!(
        ready_blame(&session.app).head(),
        helix_vcs::head_commit_id(&file).unwrap()
    );

    // turning it off drops the result
    session.keys(":toggle-inline-blame<ret>").await;
    assert!(matches!(
        doc(&session.app).blame.state(),
        BlameState::Unknown
    ));
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_a_file_with_the_feature_enabled_computes_its_blame() {
    let (_dir, file) = repo();
    let mut session = Session::new(app_with(&file, config(true, InlineBlameShow::AllLines, 0)));
    session.wait_until_ready().await;
    assert_eq!(
        shown(&session.app, 1).as_deref(),
        Some("Jane Doe|Add numbers")
    );

    // a second file opened later
    let other = file.with_file_name("other.txt");
    std::fs::write(&other, "x\n").unwrap();
    session
        .keys(&format!(":o {}<ret>", other.to_string_lossy()))
        .await;
    session
        .wait_for_state("blame of the second file", |state| {
            matches!(state, BlameState::NotCommitted)
        })
        .await;
    assert_eq!(shown(&session.app, 0).as_deref(), Some("Not committed yet"));
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn edited_and_added_lines_are_not_committed_yet() {
    let (_dir, file) = repo();
    let config = config_with_blame_line_key(config(true, InlineBlameShow::AllLines, 0));
    let mut session = Session::new(app_with(&file, config));
    session.wait_until_ready().await;

    // edit the second line, add a line above the first
    session.keys("jccedited<esc>kOnew<esc>").await;
    session
        .wait_for("the diff", |app| {
            doc(app).line_blame(0) == LineBlameResult::NotCommittedYet
        })
        .await;
    // new, old first line, edited, old third line
    assert_eq!(shown(&session.app, 0).as_deref(), Some("Not committed yet"));
    assert_eq!(
        shown(&session.app, 1).as_deref(),
        Some("Jane Doe|Add numbers")
    );
    assert_eq!(shown(&session.app, 2).as_deref(), Some("Not committed yet"));
    assert_eq!(
        shown(&session.app, 3).as_deref(),
        Some("Jane Doe|Add numbers")
    );

    // blame_line reports the same in the statusline
    session.keys("gg<F2>").await;
    assert_eq!(status(&session.app), "Not committed yet");
    session.keys("j<F2>").await;
    assert_eq!(status(&session.app), "Jane Doe|Add numbers");
    helpers::assert_status_not_error(&session.app.editor);
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blame_line_works_with_inline_blame_off() {
    let (_dir, file) = repo();
    let config = config_with_blame_line_key(config(false, InlineBlameShow::CursorLine, 0));
    let mut session = Session::new(app_with(&file, config));

    // the first request computes the blame and shows it when it is there
    session.keys("j<F2>").await;
    session.wait_until_ready().await;
    session
        .wait_for("the blame in the statusline", |app| {
            status(app) == "Jane Doe|Add numbers"
        })
        .await;
    assert!(!enabled(&session.app));
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn untracked_files_are_not_committed() {
    let (dir, _file) = repo();
    let untracked = dir.path().join("new.txt");
    std::fs::write(&untracked, "fresh\n").unwrap();
    let mut session = Session::new(app_with(
        &untracked,
        config(true, InlineBlameShow::CursorLine, 0),
    ));
    session
        .wait_for_state("blame", |state| matches!(state, BlameState::NotCommitted))
        .await;
    assert_eq!(shown(&session.app, 0).as_deref(), Some("Not committed yet"));
    session.close().await;
}

// (one application per test: background jobs are delivered to the first one of a runtime)
#[tokio::test(flavor = "multi_thread")]
async fn repositories_without_commits_are_not_committed() {
    let empty = tempfile::tempdir().unwrap();
    git(empty.path(), &["init", "-q"]);
    let file = empty.path().join("a.txt");
    std::fs::write(&file, "a\n").unwrap();
    let mut session = Session::new(app_with(
        &file,
        config(true, InlineBlameShow::CursorLine, 0),
    ));
    session
        .wait_for_state("blame", |state| matches!(state, BlameState::NotCommitted))
        .await;
    assert_eq!(shown(&session.app, 0).as_deref(), Some("Not committed yet"));
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn files_outside_of_repositories_show_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "a\nb\n").unwrap();
    let config = config_with_blame_line_key(config(true, InlineBlameShow::AllLines, 0));
    let mut session = Session::new(app_with(&file, config));
    session
        .wait_for_state("blame", |state| matches!(state, BlameState::Unavailable))
        .await;
    assert_eq!(doc(&session.app).line_blame(0), LineBlameResult::Nothing);

    // typing, saving and asking for it all work
    session.keys("ix<esc>:w<ret>j<F2>").await;
    assert!(matches!(
        doc(&session.app).blame.state(),
        BlameState::Unavailable
    ));
    assert!(!doc(&session.app).blame.is_in_flight());
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scratch_buffers_do_nothing() {
    let mut session = Session::new(
        helpers::AppBuilder::new()
            .with_config(config_with_blame_line_key(config(
                true,
                InlineBlameShow::AllLines,
                0,
            )))
            .build()
            .unwrap(),
    );
    session.keys("ihello<esc><F2>").await;
    assert!(doc(&session.app).path().is_none());
    assert!(matches!(
        doc(&session.app).blame.state(),
        BlameState::Unknown
    ));
    assert!(!doc(&session.app).blame.is_in_flight());
    session.keys(":toggle-inline-blame<ret><space>B").await;
    assert!(matches!(
        doc(&session.app).blame.state(),
        BlameState::Unknown
    ));
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn insert_mode_and_all_lines_mode_render() {
    let (_dir, file) = repo();
    let mut session = Session::new(app_with(&file, config(true, InlineBlameShow::AllLines, 0)));
    session.wait_until_ready().await;
    // (every key renders the editor, which draws the blame in normal mode and not in insert mode)
    session.keys("ofoo<esc>ibar<esc>vjd<esc>").await;
    session
        .keys(":set inline-blame.show cursor-line<ret>jjk")
        .await;
    session.keys(":set inline-blame.delay 10<ret>jk").await;
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_line_shows_after_the_delay() {
    let (_dir, file) = repo();
    let mut session = Session::new(app_with(
        &file,
        config(true, InlineBlameShow::CursorLine, 1500),
    ));
    session.wait_until_ready().await;
    let view = session.app.editor.tree.focus;
    let ready_line = |app: &Application| doc(app).blame.ready_line(view);

    // the cursor stays on the first line until the delay is over
    session
        .wait_for("first line", |app| ready_line(app) == Some(0))
        .await;

    // moving hides the blame until the delay is over again
    session.keys("j").await;
    assert_eq!(ready_line(&session.app), None);
    session.keys("j").await;
    assert_eq!(ready_line(&session.app), None);
    session
        .wait_for("third line", |app| ready_line(app) == Some(2))
        .await;

    // without a delay there is nothing to wait for
    session.keys(":set inline-blame.delay 0<ret>k").await;
    assert_eq!(ready_line(&session.app), None);
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_only_recomputes_when_head_changed() {
    let (dir, file) = repo();
    let mut session = Session::new(app_with(
        &file,
        config(true, InlineBlameShow::CursorLine, 0),
    ));
    session.wait_until_ready().await;
    let first = ready_blame(&session.app);

    // saving with the same HEAD keeps the blame
    session.keys("ix<esc>:w<ret>").await;
    session.keys("<ret>").await;
    assert!(Arc::ptr_eq(&first, &ready_blame(&session.app)));

    // after a commit it is computed again
    // (commits the edit that was saved)
    commit(dir.path(), "Bob Smith", "Prefix one");
    session.keys(":w<ret>").await;
    // (the diff is against the new HEAD too)
    session
        .wait_for("the new blame", |app| {
            shown(app, 0).as_deref() == Some("Bob Smith|Prefix one")
        })
        .await;
    assert!(matches!(
        doc(&session.app).blame.state(),
        BlameState::Ready(blame) if blame.head() != first.head()
    ));
    assert_eq!(
        shown(&session.app, 1).as_deref(),
        Some("Jane Doe|Add numbers")
    );
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn regaining_focus_notices_new_commits() {
    let (dir, file) = repo();
    let mut session = Session::new(app_with(
        &file,
        config(true, InlineBlameShow::CursorLine, 0),
    ));
    session.wait_until_ready().await;
    let first = ready_blame(&session.app);
    assert_eq!(
        shown(&session.app, 0).as_deref(),
        Some("Jane Doe|Add numbers")
    );

    // another program changes and commits the first line, the buffer still has the old one
    std::fs::write(&file, "ONE\ntwo\nthree\n").unwrap();
    commit(dir.path(), "Bob Smith", "Shout one");
    session.event(FOCUS_EVENTS.0).await;
    session.event(FOCUS_EVENTS.1).await;
    session
        .wait_for("the new blame", |app| {
            shown(app, 0).as_deref() == Some("Not committed yet")
        })
        .await;
    assert!(matches!(
        doc(&session.app).blame.state(),
        BlameState::Ready(blame) if blame.head() != first.head()
    ));
    assert_eq!(
        shown(&session.app, 1).as_deref(),
        Some("Jane Doe|Add numbers")
    );
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reloading_notices_new_commits() {
    let (dir, file) = repo();
    let mut session = Session::new(app_with(
        &file,
        config(true, InlineBlameShow::CursorLine, 0),
    ));
    session.wait_until_ready().await;
    std::fs::write(&file, "ONE\ntwo\nthree\n").unwrap();
    commit(dir.path(), "Bob Smith", "Shout one");
    session.keys(":reload<ret>").await;
    session
        .wait_for("the new blame", |app| {
            shown(app, 0).as_deref() == Some("Bob Smith|Shout one")
        })
        .await;
    assert_eq!(
        shown(&session.app, 1).as_deref(),
        Some("Jane Doe|Add numbers")
    );
    session.close().await;
}

// Rendering of the whole application, checked on the screen of its test backend.

/// The color of inlay hints in the theme the rendering tests use, which has no style for blame.
const HINT_COLOR: helix_view::graphics::Color = helix_view::graphics::Color::Rgb(0, 255, 0);

impl Session {
    /// Run the application for `duration`, for things that must not happen.
    async fn settle(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.app.editor.reset_idle_timer();
            assert!(self.app.event_loop_until_idle(&mut self.input).await);
        }
    }

    /// Run until `condition` is true of the screen.
    async fn wait_for_screen(&mut self, what: &str, condition: impl Fn(&[String]) -> bool) {
        self.wait_for(what, |app| condition(&screen_rows(app)))
            .await;
    }
}

fn render_config(show: InlineBlameShow, delay: u64) -> Config {
    let mut config = config(true, show, delay);
    config.editor.inline_blame.format = InlineBlameConfig::default().format;
    config
}

/// Hint-colored application: the theme has a style for inlay hints and none for inline blame.
fn render_app(file: &Path, config: Config) -> Application {
    let mut app = app_with(file, config);
    app.editor.theme = helix_view::Theme::from(
        toml::from_str::<toml::Value>(
            r##"
"ui.virtual.inlay-hint" = { fg = "#00ff00" }
"ui.selection" = { bg = "#333333" }
"##,
        )
        .unwrap(),
    );
    app
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

fn on_screen(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

const BLAME: &str = "Jane Doe, ";

#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_line_is_drawn_after_its_text() {
    let (dir, file) = repo();
    let mut session = Session::new(render_app(
        &file,
        render_config(InlineBlameShow::CursorLine, 1000),
    ));

    // line 0 is on row 0, line 1 on row 1 (no bufferline, no wrapping)
    let first = session.wait_for_screen("blame on the first line", |rows| rows[0].contains(BLAME));
    first.await;
    let rows = screen_rows(&session.app);
    assert!(rows[0].contains("one"), "{:?}", rows[0]);
    assert!(
        rows[0].contains("Jane Doe, ") && rows[0].contains("• Add numbers"),
        "{:?}",
        rows[0]
    );
    // only the cursor line has it
    assert!(!rows[1].contains("Jane Doe") && !rows[2].contains("Jane Doe"));

    // a few columns after the end of the text, drawn like an inlay hint
    let text_end = rows[0].find("one").unwrap() + "one".len();
    let start = rows[0].find("Jane Doe").unwrap();
    assert!((2..=6).contains(&(start - text_end)), "{:?}", rows[0]);
    let screen = session.app.screen();
    let cell = |x: usize| screen.get(x as u16, 0).unwrap();
    assert_eq!(cell(start).fg, HINT_COLOR);
    assert_eq!(cell(start + "Jane Doe, Add".len()).fg, HINT_COLOR);
    assert_ne!(cell(text_end - 1).fg, HINT_COLOR);

    // moving hides it until the delay is over, then it follows the cursor
    session.keys("j").await;
    let rows = screen_rows(&session.app);
    assert!(!on_screen(&rows, "Jane Doe"), "{rows:#?}");
    session
        .wait_for_screen("blame on the second line", |rows| {
            rows[1].contains(BLAME) && !rows[0].contains(BLAME)
        })
        .await;

    // not in insert mode
    session.keys("i").await;
    session.settle(Duration::from_millis(100)).await;
    assert!(!on_screen(&screen_rows(&session.app), "Jane Doe"));

    // an edited line is not committed yet, back in normal mode
    session.keys("x<esc>").await;
    session
        .wait_for_screen("the edited line", |rows| {
            rows[1].contains("xtwo") && rows[1].contains("Not committed yet")
        })
        .await;
    assert!(!on_screen(&screen_rows(&session.app), "Jane Doe"));

    // the file tree is over the document: it is not drawn with blame
    session.keys("<space>e").await;
    session
        .wait_for_screen("the file tree", |rows| on_screen(rows, "Cargo.toml"))
        .await;
    session.settle(Duration::from_millis(1500)).await;
    let rows = screen_rows(&session.app);
    assert!(
        !on_screen(&rows, "Not committed yet") && !on_screen(&rows, "Jane Doe"),
        "{rows:#?}"
    );

    // and it is back when the tree is closed
    session.keys("<esc>").await;
    session
        .wait_for_screen("blame after the tree", |rows| {
            rows.iter()
                .any(|row| row.contains("xtwo") && row.contains("Not committed yet"))
        })
        .await;
    assert!(!on_screen(&screen_rows(&session.app), "Cargo.toml"));

    // the diff viewer shows the change and no blame
    session.keys(":w<ret><space>=").await;
    session
        .wait_for_screen("the diff viewer", |rows| {
            on_screen(rows, "xtwo") && on_screen(rows, "│")
        })
        .await;
    session.settle(Duration::from_millis(1500)).await;
    let rows = screen_rows(&session.app);
    assert!(
        !on_screen(&rows, "Not committed yet")
            && !on_screen(&rows, "Jane Doe")
            && !on_screen(&rows, "Add numbers"),
        "{rows:#?}"
    );
    drop(dir);
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn all_lines_are_drawn() {
    let (dir, file) = repo();
    commit_more(dir.path(), &file);
    let mut session = Session::new(render_app(
        &file,
        render_config(InlineBlameShow::AllLines, 1000),
    ));
    session
        .wait_for_screen("blame on every line", |rows| {
            rows[..3].iter().all(|row| row.contains("Jane Doe, "))
                && rows[3].contains("Bob Smith, ")
        })
        .await;
    let rows = screen_rows(&session.app);
    // the line in between has nothing on it
    assert!(
        rows[4].trim().chars().all(|c| c.is_ascii_digit()),
        "{:?}",
        rows[4]
    );
    assert!(
        rows[5].contains("last") && rows[5].contains("Bob Smith, "),
        "{:?}",
        rows[5]
    );
    assert!(rows[5].contains("• Add last line"), "{:?}", rows[5]);
    // (the delay does not matter, it is for the cursor line)

    // not in insert mode, and an edit is not committed yet
    session.keys("i").await;
    assert!(!on_screen(&screen_rows(&session.app), "Jane Doe"));
    session.keys("x<esc>").await;
    session
        .wait_for_screen("the edited line", |rows| {
            rows[0].contains("xone") && rows[0].contains("Not committed yet")
        })
        .await;
    let rows = screen_rows(&session.app);
    assert!(rows[1].contains("Jane Doe, "), "{:?}", rows[1]);
    session.close().await;
}

/// A fourth line by somebody else, an empty line and a last one.
fn commit_more(dir: &Path, file: &Path) {
    std::fs::write(file, "one\ntwo\nthree\nfour\n\nlast\n").unwrap();
    commit(dir, "Bob Smith", "Add last line");
}
