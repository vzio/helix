use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio_stream::wrappers::UnboundedReceiverStream;
use vortex_term::{application::Application, config::Config, keymap::KeyTrie};
use vortex_view::{
    blame::{BlameState, LineBlameResult},
    document::Mode,
    editor::{InlineBlameConfig, InlineBlameShow},
    input::parse_macro,
};

use termina::event::{Event, KeyEvent};

use super::*;

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
            state(vortex_view::doc!(app.editor).blame.state())
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

fn doc(app: &Application) -> &vortex_view::Document {
    vortex_view::doc!(app.editor)
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

fn ready_blame(app: &Application) -> Arc<vortex_vcs::FileBlame> {
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
        vortex_vcs::head_commit_id(&file).unwrap()
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

    // another program changes and commits the first line; the buffer has no edits, so it is
    // reloaded when focus returns
    std::fs::write(&file, "ONE\ntwo\nthree\n").unwrap();
    // (the modification time moved on for sure, whatever the clock's resolution)
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
        .unwrap();
    commit(dir.path(), "Bob Smith", "Shout one");
    session.event(FOCUS_EVENTS.0).await;
    session.event(FOCUS_EVENTS.1).await;
    session
        .wait_for("the new blame", |app| {
            shown(app, 0).as_deref() == Some("Bob Smith|Shout one")
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
const HINT_COLOR: vortex_view::graphics::Color = vortex_view::graphics::Color::Rgb(0, 255, 0);

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
    app.editor.theme = vortex_view::Theme::from(
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
    // (it starts with the short hash of the commit)
    let short = git_output(dir.path(), &["rev-parse", "--short", "HEAD"]);
    let start = rows[0].find(&format!("{short} Jane Doe")).unwrap();
    assert!((2..=6).contains(&(start - text_end)), "{:?}", rows[0]);
    let screen = session.app.screen();
    let cell = |x: usize| screen.get(x as u16, 0).unwrap();
    // (the hash has no style of its own in this theme)
    assert_eq!(cell(start).fg, HINT_COLOR);
    assert_eq!(cell(start + short.len() - 1).fg, HINT_COLOR);
    assert_eq!(
        cell(start + short.len() + 1 + "Jane Doe, Add".len()).fg,
        HINT_COLOR
    );
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

// Commit hashes in the blame, and `space i` yanking them.

/// What `git <args>` prints in `dir`.
fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .expect("git can be run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// The full id of `rev` and the abbreviation git makes of it.
fn ids(dir: &Path, rev: &str) -> (String, String) {
    (
        git_output(dir, &["rev-parse", rev]),
        git_output(dir, &["rev-parse", "--short", rev]),
    )
}

/// `numbers.txt` with three commits (Jane Doe: "one", "two" and "three", Bob Smith: "TWO"), and
/// a fourth line that is not committed. Returns the repository, the file and the ids (full and
/// short) of the commit of Jane (lines 0 and 2) and of Bob (line 1).
fn hash_repo() -> (
    tempfile::TempDir,
    PathBuf,
    (String, String),
    (String, String),
) {
    let (dir, file) = repo();
    std::fs::write(&file, "one\nTWO\nthree\n").unwrap();
    commit(dir.path(), "Bob Smith", "Shout two");
    let jane = ids(dir.path(), "HEAD~1");
    let bob = ids(dir.path(), "HEAD");
    std::fs::write(&file, "one\nTWO\nthree\nfresh\n").unwrap();
    (dir, file, jane, bob)
}

const HASH_FORMAT: &str = "{commit} {author}|{title}";

fn hash_config(enable: bool) -> Config {
    let mut config = config(enable, InlineBlameShow::AllLines, 0);
    config.editor.inline_blame.format = HASH_FORMAT.to_owned();
    config
}

/// The values of a register, if it has any.
fn register(app: &Application, name: char) -> Option<Vec<String>> {
    app.editor
        .registers
        .read(name, &app.editor)
        .map(|values| values.map(|value| value.into_owned()).collect())
}

impl Session {
    /// Waits for the blame of every line of the document, of the diff too.
    async fn wait_until_lines_blamed(&mut self) {
        self.wait_until_ready().await;
        self.wait_for("the blame of the lines", |app| {
            doc(app).line_blame(0) != LineBlameResult::Nothing
                && doc(app).line_blame(3) != LineBlameResult::Nothing
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_blame_of_a_committed_line_starts_with_its_short_hash() {
    let (_dir, file, jane, bob) = hash_repo();
    let mut session = Session::new(app_with(&file, hash_config(true)));
    session.wait_until_lines_blamed().await;
    let app = &session.app;

    // git's abbreviation (7 digits at least), a prefix of the whole id
    for short in [&jane.1, &bob.1] {
        assert!(short.len() >= 7);
    }
    assert!(jane.0.starts_with(&jane.1) && bob.0.starts_with(&bob.1));
    assert_ne!(jane.1, bob.1);

    let expect = |line: usize, (_, short): &(String, String), rest: &str| {
        let (text, range) = doc(app)
            .line_blame(line)
            .display_with_commit(HASH_FORMAT)
            .unwrap_or_else(|| panic!("no blame for line {line}"));
        assert_eq!(text, format!("{short} {rest}"), "line {line}");
        assert_eq!(range, Some(0..short.len()), "line {line}");
        // `display` is the same text
        assert_eq!(shown_with(app, line).as_deref(), Some(text.as_str()));
    };
    expect(0, &jane, "Jane Doe|Add numbers");
    expect(1, &bob, "Bob Smith|Shout two");
    expect(2, &jane, "Jane Doe|Add numbers");

    // the default format has the hash first
    let (text, range) = doc(app)
        .line_blame(1)
        .display_with_commit(&InlineBlameConfig::default().format)
        .unwrap();
    assert!(
        text.starts_with(&format!("{} Bob Smith, ", bob.1)),
        "{text}"
    );
    assert!(text.ends_with(" • Shout two"), "{text}");
    assert_eq!(range, Some(0..bob.1.len()));

    // a line that is not committed has none
    assert_eq!(
        doc(app).line_blame(3),
        LineBlameResult::NotCommittedYet,
        "the fourth line"
    );
    assert_eq!(
        doc(app).line_blame(3).display_with_commit(HASH_FORMAT),
        Some(("Not committed yet".to_owned(), None))
    );
    // and neither has a format without the variable
    assert_eq!(
        doc(app).line_blame(0).display_with_commit(FORMAT),
        Some(("Jane Doe|Add numbers".to_owned(), None))
    );
    session.close().await;
}

fn shown_with(app: &Application, line: usize) -> Option<String> {
    doc(app).line_blame(line).display(HASH_FORMAT)
}

const BLAME_COLOR: vortex_view::graphics::Color = vortex_view::graphics::Color::Rgb(0, 255, 0);
const HASH_COLOR: vortex_view::graphics::Color = vortex_view::graphics::Color::Rgb(255, 0, 0);

fn theme_of(toml: &str) -> vortex_view::Theme {
    vortex_view::Theme::from(toml::from_str::<toml::Value>(toml).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hash_is_drawn_with_its_own_style() {
    let (_dir, file, jane, _bob) = hash_repo();
    let mut config = hash_config(true);
    config.editor.inline_blame.format = InlineBlameConfig::default().format;
    let mut session = Session::new(app_with(&file, config));
    session.app.editor.theme = theme_of(
        r##"
"ui.virtual.inline-blame" = { fg = "#00ff00" }
"ui.virtual.inline-blame.hash" = { fg = "#ff0000" }
"ui.selection" = { bg = "#333333" }
"##,
    );
    session
        .wait_for_screen("the blame", |rows| on_screen(rows, &jane.1))
        .await;

    // the colors of the cells of `text` on the row of the line `line`
    let colors = |app: &Application, line: usize, text: &str| {
        let rows = screen_rows(app);
        let row = &rows[line];
        let byte = row
            .find(text)
            .unwrap_or_else(|| panic!("no {text:?} in {row:?}"));
        // (a column, not a byte: the blame has a bullet)
        let x = row[..byte].chars().count();
        let screen = app.screen();
        (x..x + text.chars().count())
            .map(|x| screen.get(x as u16, line as u16).unwrap().fg)
            .collect::<Vec<_>>()
    };
    let rest = "Jane Doe, ";
    // the hash only, the rest has the blame's
    let hash_cells = colors(&session.app, 0, &jane.1);
    assert_eq!(hash_cells.len(), jane.1.len());
    assert!(
        hash_cells.iter().all(|fg| *fg == HASH_COLOR),
        "{hash_cells:?}"
    );
    let rest_cells = colors(&session.app, 0, rest);
    assert!(
        rest_cells.iter().all(|fg| *fg == BLAME_COLOR),
        "{rest_cells:?}"
    );
    let title = colors(&session.app, 0, "Add numbers");
    assert!(title.iter().all(|fg| *fg == BLAME_COLOR), "{title:?}");
    // the space between them is not the hash
    let row = screen_rows(&session.app)[0].clone();
    let after_hash = row.find(&jane.1).unwrap() + jane.1.len();
    assert_eq!(
        session.app.screen().get(after_hash as u16, 0).unwrap().fg,
        BLAME_COLOR
    );
    // the text of the line itself is neither
    let code = colors(&session.app, 0, "one");
    assert!(code
        .iter()
        .all(|fg| *fg != BLAME_COLOR && *fg != HASH_COLOR));

    // the line that is not committed has no hash: all of it is drawn the same
    let rows = screen_rows(&session.app);
    assert!(rows[3].contains("fresh"), "{:?}", rows[3]);
    let nothing = colors(&session.app, 3, "Not committed yet");
    assert!(nothing.iter().all(|fg| *fg == BLAME_COLOR), "{nothing:?}");

    // the scope patches the style of the blame: without a color of its own, the hash is
    // drawn in the color of the blame, and still gets the modifier
    session.app.editor.theme = theme_of(
        r##"
"ui.virtual.inline-blame" = { fg = "#00ff00" }
"ui.virtual.inline-blame.hash" = { modifiers = ["bold"] }
"ui.selection" = { bg = "#333333" }
"##,
    );
    // (a movement is a reason to draw again)
    session.keys("jk").await;
    let hash_cells = colors(&session.app, 0, &jane.1);
    assert!(
        hash_cells.iter().all(|fg| *fg == BLAME_COLOR),
        "{hash_cells:?}"
    );
    let row = screen_rows(&session.app)[0].clone();
    let x = row.find(&jane.1).unwrap() as u16;
    let screen = session.app.screen();
    assert!(screen
        .get(x, 0)
        .unwrap()
        .modifier
        .contains(vortex_view::graphics::Modifier::BOLD));
    let y = row.find("Jane Doe").unwrap() as u16;
    assert!(!screen
        .get(y, 0)
        .unwrap()
        .modifier
        .contains(vortex_view::graphics::Modifier::BOLD));

    // a theme with no scope for the hash: the style of the blame
    session.app.editor.theme = theme_of(
        "\"ui.virtual.inline-blame\" = { fg = \"#00ff00\" }\n\"ui.selection\" = { bg = \"#333333\" }",
    );
    session.keys("jk").await;
    let hash_cells = colors(&session.app, 0, &jane.1);
    assert!(
        hash_cells.iter().all(|fg| *fg == BLAME_COLOR),
        "{hash_cells:?}"
    );
    session.close().await;
}

/// The status line as `(message, is an error)`.
fn status_line(app: &Application) -> (String, bool) {
    let (message, severity) = app.editor.get_status().expect("a status message");
    (
        message.to_string(),
        *severity == vortex_view::editor::Severity::Error,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn space_i_yanks_the_hash_of_the_line_of_the_cursor() {
    let (dir, file, jane, bob) = hash_repo();
    let mut session = Session::new(app_with(&file, hash_config(true)));
    session.wait_until_lines_blamed().await;

    // the whole id, not the abbreviation of the blame, in the register typed before
    session.keys("\"a<space>i").await;
    assert_eq!(register(&session.app, 'a'), Some(vec![jane.0.clone()]));
    assert_eq!(jane.0.len(), 40);
    assert_eq!(jane.0, git_output(dir.path(), &["rev-parse", "HEAD~1"]));
    assert_eq!(
        status_line(&session.app),
        (
            format!(
                "No diagnostic here: yanked the commit {}… of line 1 to register a",
                &jane.0[..12]
            ),
            false
        )
    );

    // the commit of the line it is on
    session.keys("j\"b<space>i").await;
    assert_eq!(register(&session.app, 'b'), Some(vec![bob.0.clone()]));
    assert_eq!(bob.0, git_output(dir.path(), &["rev-parse", "HEAD"]));
    assert_eq!(
        status_line(&session.app),
        (
            format!(
                "No diagnostic here: yanked the commit {}… of line 2 to register b",
                &bob.0[..12]
            ),
            false
        )
    );
    // (the other register stays as it was)
    assert_eq!(register(&session.app, 'a'), Some(vec![jane.0.clone()]));

    // the typable command is the same, with its register
    session.keys("j:yank-diagnostic c<ret>").await;
    assert_eq!(register(&session.app, 'c'), Some(vec![jane.0.clone()]));
    assert_eq!(
        status_line(&session.app),
        (
            format!(
                "No diagnostic here: yanked the commit {}… of line 3 to register c",
                &jane.0[..12]
            ),
            false
        )
    );

    // without a register: the clipboard, which tests may not have: if it is there, it has it
    session.keys("k<space>i").await;
    let (message, is_error) = status_line(&session.app);
    if is_error {
        assert!(message.to_lowercase().contains("clipboard"), "{message}");
    } else {
        assert!(message.ends_with("of line 2 to register +"), "{message}");
        assert_eq!(register(&session.app, '+'), Some(vec![bob.0.clone()]));
    }
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn space_i_on_a_line_that_is_not_committed_yanks_nothing() {
    let (_dir, file, jane, _bob) = hash_repo();
    let mut session = Session::new(app_with(&file, hash_config(true)));
    session.wait_until_lines_blamed().await;

    // something in the register to keep
    session.keys("\"a<space>i").await;
    assert_eq!(register(&session.app, 'a'), Some(vec![jane.0.clone()]));

    session.keys("jjj\"a<space>i").await;
    assert_eq!(
        status_line(&session.app),
        (
            "No diagnostics under primary selection, and the line isn't committed yet".to_owned(),
            true
        )
    );
    assert_eq!(register(&session.app, 'a'), Some(vec![jane.0.clone()]));
    // a register that is empty stays so
    session.keys(":yank-diagnostic d<ret>").await;
    assert!(status_line(&session.app).1);
    assert_eq!(register(&session.app, 'd'), None);
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diagnostic_is_yanked_before_the_hash() {
    use vortex_core::diagnostic::{Diagnostic, DiagnosticProvider, Range as DiagnosticRange};

    let (_dir, file, _jane, bob) = hash_repo();
    let mut session = Session::new(app_with(&file, hash_config(true)));
    session.wait_until_lines_blamed().await;

    // a diagnostic on the first word of the second line ("TWO")
    let start = "one\n".len();
    let diagnostic = Diagnostic {
        range: DiagnosticRange {
            start,
            end: start + "TWO".len(),
        },
        ends_at_word: true,
        starts_at_word: true,
        zero_width: false,
        line: 1,
        message: "unused thing".to_owned(),
        severity: None,
        code: None,
        provider: DiagnosticProvider::Lsp {
            server_id: Default::default(),
            identifier: None,
        },
        tags: Vec::new(),
        source: None,
        data: None,
    };
    vortex_view::doc_mut!(session.app.editor).replace_diagnostics([diagnostic], &[], None);

    // on it: the message
    session.keys("j\"a<space>i").await;
    assert_eq!(
        register(&session.app, 'a'),
        Some(vec!["unused thing".to_owned()])
    );
    assert_eq!(
        status_line(&session.app),
        ("Yanked 1 diagnostic to register a".to_owned(), false)
    );
    session.keys(":yank-diagnostic b<ret>").await;
    assert_eq!(
        register(&session.app, 'b'),
        Some(vec!["unused thing".to_owned()])
    );

    // off it, on the line above: the hash
    session.keys("k\"c<space>i").await;
    let (_, is_error) = status_line(&session.app);
    assert!(!is_error);
    let jane = git_output(_dir.path(), &["rev-parse", "HEAD~1"]);
    assert_eq!(register(&session.app, 'c'), Some(vec![jane]));
    // (and the diagnostic has its commit, Bob's, for when it is gone)
    vortex_view::doc_mut!(session.app.editor).replace_diagnostics([], &[], None);
    session.keys("j\"e<space>i").await;
    assert_eq!(register(&session.app, 'e'), Some(vec![bob.0]));
    session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn space_i_without_inline_blame_keeps_its_error() {
    let (_dir, file, _jane, _bob) = hash_repo();
    let mut session = Session::new(app_with(&file, hash_config(false)));
    session.keys("\"a<space>i").await;
    assert_eq!(
        status_line(&session.app),
        ("No diagnostics under primary selection".to_owned(), true)
    );
    assert_eq!(register(&session.app, 'a'), None);
    // no blame was asked for either
    assert!(matches!(
        doc(&session.app).blame.state(),
        BlameState::Unknown
    ));

    session.keys(":yank-diagnostic b<ret>").await;
    assert_eq!(
        status_line(&session.app),
        (
            "'yank-diagnostic': No diagnostics under primary selection".to_owned(),
            true
        )
    );
    assert_eq!(register(&session.app, 'b'), None);

    // turned on (and the blame there), it yanks
    session.keys(":toggle-inline-blame<ret>").await;
    session.wait_until_lines_blamed().await;
    session.keys("\"a<space>i").await;
    assert_eq!(
        register(&session.app, 'a').map(|values| values[0].len()),
        Some(40)
    );
    // and off again: back to the error
    session.keys(":toggle-inline-blame<ret>").await;
    assert!(!enabled(&session.app));
    session.keys("\"f<space>i").await;
    assert_eq!(
        status_line(&session.app),
        ("No diagnostics under primary selection".to_owned(), true)
    );
    assert_eq!(register(&session.app, 'f'), None);
    session.close().await;
}
