//! Tests of the git commit log picker (`space l`): the dividers between the branches, the range
//! of commits, and what Enter lists for them.
//!
//! The log is of the repository of the working directory, which every test of this process
//! shares and can't change. So each test runs itself again in a process of its own, with the
//! working directory in a repository made for it: see [`Repo::enter`].

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use tokio_stream::wrappers::UnboundedReceiverStream;
use vortex_term::application::Application;
use vortex_view::input::parse_macro;

#[cfg(windows)]
use crossterm::event::{Event, KeyEvent};
#[cfg(not(windows))]
use termina::event::{Event, KeyEvent};

use super::*;

const IN_REPO: &str = "VORTEX_GIT_LOG_TEST_IN_REPO";

pub(super) fn git(dir: &Path, date: &str, args: &[&str]) -> String {
    let mut command = Command::new("git");
    if !date.is_empty() {
        command
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
    }
    let output = command
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Jane Doe")
        .env("GIT_AUTHOR_EMAIL", "jane@example.com")
        .env("GIT_COMMITTER_NAME", "Jane Doe")
        .env("GIT_COMMITTER_EMAIL", "jane@example.com")
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
    String::from_utf8(output.stdout).unwrap()
}

/// The repository the tests look at: a working copy on the branch `feature`, with a remote.
///
/// ```text
/// * Feature: tests            ┐
/// *   Merge main into feature │ feature (unpushed)
/// |\                          │
/// * | WIP: more feature       ┘
/// * | Feature: handle errors   ┐ feature (pushed)
/// * | Start the feature        ┘
/// | * Fix a crash on empty input ┐
/// |/                             │ origin/main
/// * Add the parser               │
/// * Initial commit               ┘
/// ```
pub(super) struct Repo {
    /// The working directory, which is the working directory of the process too.
    pub(super) work: PathBuf,
    minute: u32,
}

impl Repo {
    fn commit(&mut self, file: &str, message: &str) {
        std::fs::write(self.work.join(file), format!("{file}\n")).unwrap();
        let date = self.date();
        git(&self.work, &date, &["add", "-A"]);
        git(&self.work, &date, &["commit", "-q", "-m", message]);
    }

    fn date(&mut self) -> String {
        self.minute += 1;
        format!("2024-03-01T12:{:02}:00+00:00", self.minute)
    }

    /// Makes the repository in `parent`.
    fn make(parent: &Path) -> PathBuf {
        let remote = parent.join("remote.git");
        let work = parent.join("work");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&remote, "", &["init", "-q", "--bare"]);
        git(&work, "", &["init", "-q"]);
        let mut repo = Repo { work, minute: 0 };
        let work = repo.work.clone();
        repo.commit("m1.txt", "Initial commit");
        git(
            &work,
            "",
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        repo.commit("m2.txt", "Add the parser");
        git(&work, "", &["push", "-q", "-u", "origin", "main"]);
        git(&work, "", &["remote", "set-head", "origin", "main"]);
        git(&work, "", &["checkout", "-q", "-b", "feature"]);
        repo.commit("f1.txt", "Start the feature");
        repo.commit("f2.txt", "Feature: handle errors");
        git(&work, "", &["push", "-q", "-u", "origin", "feature"]);
        repo.commit("f3.txt", "WIP: more feature");
        git(&work, "", &["checkout", "-q", "main"]);
        repo.commit("m3.txt", "Fix a crash on empty input");
        git(&work, "", &["push", "-q", "origin", "main"]);
        git(&work, "", &["checkout", "-q", "feature"]);
        let date = repo.date();
        git(
            &work,
            &date,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "Merge main into feature",
                "main",
            ],
        );
        repo.commit("f4.txt", "Feature: tests");
        work
    }

    /// In the process that runs the test: the directory of the repository. In the process of
    /// `cargo test`: makes the repository, runs the test again with it as the working directory
    /// and checks that it passes, and returns `None`.
    ///
    /// `test` is the name of the calling function.
    fn enter(test: &str) -> Option<PathBuf> {
        Self::enter_in(module_path!(), test)
    }

    /// [`Repo::enter`] for a test of another module of the tests: `module` is its
    /// `module_path!()`.
    pub(super) fn enter_in(module: &str, test: &str) -> Option<PathBuf> {
        Self::enter_with(module, test, Repo::make)
    }

    /// [`Repo::enter_in`] with a repository of its own: `make` makes it in a directory and
    /// returns the working directory.
    pub(super) fn enter_with(
        module: &str,
        test: &str,
        make: fn(&Path) -> PathBuf,
    ) -> Option<PathBuf> {
        if std::env::var_os(IN_REPO).is_some() {
            return Some(vortex_stdx::env::current_working_dir());
        }
        let dir = tempfile::tempdir().unwrap();
        // (not through a symlinked temp dir: paths are compared)
        let work = make(&std::fs::canonicalize(dir.path()).unwrap());
        // the name of the test as libtest knows it, without the name of the crate
        let name = format!("{}::{test}", module.split_once("::").unwrap().1);
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--test-threads=1", "--nocapture"])
            .current_dir(&work)
            .env(IN_REPO, "1")
            .env("PWD", &work)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name} failed in {}:\n{}\n{}",
            work.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // the child doesn't run when the name is wrong
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{name} didn't run:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        None
    }
}

/// The whole id of the commit with this summary.
pub(super) fn full_id(summary: &str) -> String {
    let log = git(
        &vortex_stdx::env::current_working_dir(),
        "",
        &["log", "--all", "--format=%H %s"],
    );
    let (id, _) = log
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(_, message)| *message == summary)
        .unwrap_or_else(|| panic!("no commit {summary:?} in {log}"));
    id.to_string()
}

/// The id of the commit by summary, abbreviated as git does (`git rev-parse --short`), as the
/// picker shows it.
pub(super) fn id(summary: &str) -> String {
    git(
        &vortex_stdx::env::current_working_dir(),
        "",
        &["rev-parse", "--short", &full_id(summary)],
    )
    .trim()
    .to_string()
}

/// The theme of the tests: only what they look at, with colors that are easy to tell apart.
fn theme() -> vortex_view::Theme {
    vortex_view::Theme::from(
        toml::from_str::<toml::Value>(
            r##"
"ui.selection" = { bg = "#333333" }
"ui.picker.divider" = { fg = "#00ff00" }
"##,
        )
        .unwrap(),
    )
}

const SELECTED: vortex_view::graphics::Color = vortex_view::graphics::Color::Rgb(0x33, 0x33, 0x33);
const DIVIDER: vortex_view::graphics::Color = vortex_view::graphics::Color::Rgb(0, 0xff, 0);

/// Drives an application with keys and waits for the results of background tasks.
pub(super) struct Session {
    pub(super) app: Application,
    tx: tokio::sync::mpsc::UnboundedSender<std::io::Result<Event>>,
    input: UnboundedReceiverStream<std::io::Result<Event>>,
}

impl Session {
    pub(super) fn new() -> Self {
        Self::with_config(helpers::test_config())
    }

    pub(super) fn with_config(config: vortex_term::config::Config) -> Self {
        let mut app = helpers::AppBuilder::new()
            .with_config(config)
            .build()
            .unwrap();
        app.editor.theme = theme();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            app,
            tx,
            input: UnboundedReceiverStream::new(rx),
        }
    }

    /// Opens the log, waits for all of it to be listed, and moves the cursor from the
    /// uncommitted changes (the first row, where it starts) to the newest commit.
    pub(super) async fn open_log() -> Self {
        let mut session = Self::open_log_on_uncommitted().await;
        session.keys("j").await;
        session
    }

    /// Opens the log and waits for all of it to be listed: the cursor is on the first row.
    pub(super) async fn open_log_on_uncommitted() -> Self {
        Self::open_log_with(Self::new()).await
    }

    pub(super) async fn open_log_with(mut session: Self) -> Self {
        session.keys("<space>l").await;
        session
            .wait_for_screen("the log", |rows| on_screen(rows, "Initial commit"))
            .await;
        session
    }

    pub(super) async fn keys(&mut self, keys: &str) {
        for key in parse_macro(keys).unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    /// Types `key` the way terminals send capital letters: with shift.
    async fn shifted(&mut self, key: char) {
        let key = vortex_view::input::KeyEvent {
            code: vortex_view::keyboard::KeyCode::Char(key),
            modifiers: vortex_view::keyboard::KeyModifiers::SHIFT,
        };
        self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    /// Run until `condition` is true of the screen.
    pub(super) async fn wait_for_screen(
        &mut self,
        what: &str,
        condition: impl Fn(&[String]) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition(&screen_rows(&self.app)) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}:\n{}\nstatus: {:?}",
                screen_rows(&self.app)
                    .iter()
                    .map(|row| row.trim_end())
                    .filter(|row| !row.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
                status(&self.app)
            );
            // (the idle timer is off after the last time it ran out)
            self.app.editor.reset_idle_timer();
            assert!(self.app.event_loop_until_idle(&mut self.input).await);
        }
    }

    /// Runs the application for `time`, for background tasks and watches to have their say.
    pub(super) async fn run_for(&mut self, time: Duration) {
        let end = Instant::now() + time;
        while Instant::now() < end {
            self.app.editor.reset_idle_timer();
            assert!(self.app.event_loop_until_idle(&mut self.input).await);
        }
    }

    pub(super) fn rows(&self) -> Vec<String> {
        screen_rows(&self.app)
    }

    /// The lines inside of the frame of the picker on top: the prompt, the line below it, the
    /// header, and the rows, without their trailing spaces.
    pub(super) fn frame(&self) -> Vec<String> {
        let rows = self.rows();
        let top = rows
            .iter()
            .position(|row| row.starts_with('┌'))
            .expect("a picker on the screen");
        rows[top + 1..]
            .iter()
            .take_while(|row| !row.starts_with('└'))
            .map(|row| {
                row.trim_start_matches('│')
                    .trim_end()
                    .trim_end_matches('│')
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The rows of the commits and dividers of the commit picker.
    pub(super) fn log(&self) -> Vec<String> {
        let frame = self.frame();
        assert!(frame[2].contains("commit"), "{frame:#?}");
        frame[3..]
            .iter()
            .filter(|row| !row.is_empty())
            .cloned()
            .collect()
    }

    /// The summary of the commit under the cursor, or `Uncommitted changes` (without the count)
    /// for the first row. Panics if the cursor is on a divider.
    pub(super) fn cursor(&self) -> String {
        let log = self.log();
        let mut cursors = log.iter().filter(|row| row.starts_with(" > "));
        let row = cursors
            .next()
            .unwrap_or_else(|| panic!("no cursor: {log:#?}"));
        assert!(cursors.next().is_none(), "two cursors: {log:#?}");
        assert!(!row.contains('─'), "the cursor is on a divider: {row}");
        if row.contains(UNCOMMITTED) {
            return UNCOMMITTED.to_string();
        }
        let (_, summary) = row.split_once("Jane Doe").unwrap();
        summary.trim().to_string()
    }

    /// The row on the screen (from 0) with `text` in it.
    pub(super) fn row_of(&self, text: &str) -> usize {
        self.rows()
            .iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("no row with {text:?}:\n{:#?}", self.frame()))
    }

    /// The background of the middle of the row of the commit `summary`.
    fn background_of(&self, summary: &str) -> vortex_view::graphics::Color {
        let row = self.row_of(summary);
        let text = &self.rows()[row];
        let x = text[..text.find(summary).unwrap()].chars().count();
        self.app.screen().get(x as u16, row as u16).unwrap().bg
    }

    /// The commits on the screen that are marked as part of the range, newest first.
    fn marked(&self) -> Vec<&'static str> {
        COMMITS
            .iter()
            .copied()
            .filter(|summary| on_screen(&self.rows(), summary))
            .filter(|summary| self.background_of(summary) == SELECTED)
            .collect()
    }
}

/// The first row of the log, which has the count of files after it.
pub(super) const UNCOMMITTED: &str = "Uncommitted changes";

pub(super) const COMMITS: [&str; 8] = [
    "Feature: tests",
    "Merge main into feature",
    "WIP: more feature",
    "Feature: handle errors",
    "Start the feature",
    "Fix a crash on empty input",
    "Add the parser",
    "Initial commit",
];

pub(super) fn screen_rows(app: &Application) -> Vec<String> {
    let screen = app.screen();
    (0..screen.area.height)
        .map(|y| {
            (0..screen.area.width)
                .map(|x| screen.get(x, y).map_or(" ", |cell| cell.symbol.as_str()))
                .collect()
        })
        .collect()
}

pub(super) fn on_screen(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

pub(super) fn status(app: &Application) -> String {
    app.editor
        .get_status()
        .map(|(msg, _)| msg.to_string())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_log_has_a_divider_for_each_branch() {
    let Some(_) = Repo::enter("the_log_has_a_divider_for_each_branch") else {
        return;
    };
    let session = Session::open_log_on_uncommitted().await;

    // the uncommitted changes come first, above the first divider, and have the cursor
    assert_eq!(session.cursor(), UNCOMMITTED);
    assert!(session.log()[0].contains(UNCOMMITTED));
    assert!(!session.log()[0].contains('─'));
    // dividers are full width lines, the commits follow in order
    let frame = session.frame();
    let log = session.log()[1..].to_vec();
    let expected = [
        ("feature (unpushed)", 0..3),
        ("feature", 3..5),
        ("origin/main", 5..8),
    ];
    let mut at = 0;
    for (title, commits) in expected {
        let divider = &log[at];
        assert!(
            divider.starts_with(&format!("── {title} ─")) && divider.ends_with('─'),
            "{divider:?}"
        );
        // as wide as the picker: the same as the line below the prompt
        assert_eq!(divider.chars().count(), frame[1].chars().count());
        at += 1;
        for summary in &COMMITS[commits] {
            assert!(log[at].contains(summary), "{:?} is not {summary}", log[at]);
            at += 1;
        }
    }
    assert_eq!(at, log.len(), "{log:#?}");

    // the theme scope of the divider
    let row = session.row_of("── feature (unpushed)");
    let x = session.rows()[row].find("──").unwrap();
    assert_eq!(
        session.app.screen().get(x as u16, row as u16).unwrap().fg,
        DIVIDER
    );
    // and the numbers don't count them (the uncommitted changes are a row)
    assert!(frame[0].ends_with("9/9"), "{:?}", frame[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_never_rests_on_a_divider() {
    let Some(_) = Repo::enter("the_cursor_never_rests_on_a_divider") else {
        return;
    };
    let mut session = Session::open_log_on_uncommitted().await;
    // (`cursor` panics when it's on a divider)
    assert_eq!(session.cursor(), UNCOMMITTED);

    // down and up, over the dividers
    for expected in COMMITS {
        session.keys("<down>").await;
        assert_eq!(session.cursor(), expected);
    }
    // the last commit is followed by the uncommitted changes, not by the first divider
    session.keys("<down>").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    session.keys("<up>").await;
    assert_eq!(session.cursor(), COMMITS[7]);
    for expected in COMMITS[..7].iter().rev() {
        session.keys("<up>").await;
        assert_eq!(session.cursor(), *expected);
    }
    session.keys("<up>").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    session.keys("<down>").await;
    assert_eq!(session.cursor(), COMMITS[0]);

    // the other keys for it
    session.keys("<C-n><C-n><C-n>").await;
    assert_eq!(session.cursor(), COMMITS[3]);
    session.keys("<tab>").await;
    assert_eq!(session.cursor(), COMMITS[4]);
    session.keys("<C-p><S-tab>").await;
    assert_eq!(session.cursor(), COMMITS[2]);
    session.keys("<end>").await;
    assert_eq!(session.cursor(), COMMITS[7]);
    session.keys("<home>").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    // (the page is longer than the log, so these go round and round)
    for _ in 0..12 {
        session.keys("<pagedown>").await;
        session.cursor();
        session.keys("<pageup><pageup>").await;
        session.cursor();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_query_hides_the_dividers() {
    let Some(_) = Repo::enter("a_query_hides_the_dividers") else {
        return;
    };
    let mut session = Session::open_log().await;
    session.keys("<down><down><down><down>").await;

    session.keys("scrash").await;
    session
        .wait_for_screen("the match", |rows| !on_screen(rows, "Add the parser"))
        .await;
    let log = session.log();
    assert_eq!(log.len(), 1, "{log:#?}");
    assert_eq!(session.cursor(), "Fix a crash on empty input");
    assert!(
        session.frame()[0].ends_with("1/9"),
        "{:?}",
        session.frame()[0]
    );

    session
        .keys("<backspace><backspace><backspace><backspace><backspace>")
        .await;
    session
        .wait_for_screen("the whole log", |rows| on_screen(rows, "Add the parser"))
        .await;
    assert_eq!(session.log().len(), 12, "{:#?}", session.log());
    // the first row (the uncommitted changes), not the divider below it
    assert_eq!(session.cursor(), UNCOMMITTED);
    assert!(session.frame()[0].ends_with("9/9"));
}

/// The range label of the status line, oldest..newest.
pub(super) fn range(oldest: &str, newest: &str, commits: usize) -> String {
    format!("{}..{} ({commits} commits)", id(oldest), id(newest))
}

#[tokio::test(flavor = "multi_thread")]
async fn extend_mode_selects_a_range() {
    let Some(_) = Repo::enter("extend_mode_selects_a_range") else {
        return;
    };
    let mut session = Session::open_log().await;
    assert_eq!(session.marked(), [] as [&str; 0]);
    assert_eq!(status(&session.app), HINT);

    session.keys("<A-v>").await;
    assert_eq!(status(&session.app), format!("[extend] {}", id(COMMITS[0])));
    session.keys("<down><down>").await;
    assert_eq!(
        status(&session.app),
        format!("[extend] {}", range(COMMITS[2], COMMITS[0], 3))
    );
    // the rows between the anchor and the cursor, not the cursor's own
    assert_eq!(session.marked(), &COMMITS[..2]);
    assert_eq!(session.cursor(), COMMITS[2]);

    // over a divider: it's not a commit
    session.keys("<down>").await;
    assert_eq!(
        status(&session.app),
        format!("[extend] {}", range(COMMITS[3], COMMITS[0], 4))
    );
    assert_eq!(session.marked(), &COMMITS[..3]);

    // going back shrinks it
    session.keys("<up><up>").await;
    assert_eq!(session.marked(), &COMMITS[..1]);
    session.keys("<down><down>").await;

    // no extend mode: the range stays until the next movement
    session.keys("<A-v>").await;
    assert_eq!(status(&session.app), range(COMMITS[3], COMMITS[0], 4));
    assert_eq!(session.marked(), &COMMITS[..3]);
    session.keys("<down>").await;
    assert_eq!(status(&session.app), "");
    assert_eq!(session.marked(), [] as [&str; 0]);
    assert_eq!(session.cursor(), COMMITS[4]);

    // Esc collapses and leaves extend mode, and then closes
    session.keys("<A-v><down><down>").await;
    assert_eq!(
        status(&session.app),
        format!("[extend] {}", range(COMMITS[6], COMMITS[4], 3))
    );
    assert_eq!(session.marked(), &COMMITS[4..6]);
    session.keys("<esc>").await;
    assert_eq!(status(&session.app), "");
    assert_eq!(session.marked(), [] as [&str; 0]);
    assert_eq!(session.cursor(), COMMITS[6]);
    session.keys("<up>").await;
    assert_eq!(session.cursor(), COMMITS[5]);
    assert_eq!(session.marked(), [] as [&str; 0]);
    session.keys("<esc>").await;
    assert!(!on_screen(&session.rows(), "origin/main"));
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_arrows_extend_by_one_commit() {
    let Some(_) = Repo::enter("shift_arrows_extend_by_one_commit") else {
        return;
    };
    let mut session = Session::open_log().await;

    session.keys("<S-down><S-down>").await;
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[0], 3));
    assert_eq!(session.marked(), &COMMITS[..2]);
    session.keys("<S-up>").await;
    assert_eq!(status(&session.app), range(COMMITS[1], COMMITS[0], 2));
    assert_eq!(session.cursor(), COMMITS[1]);
    session.keys("<S-up>").await;
    assert_eq!(status(&session.app), "");
    // extending onto the uncommitted changes, past the divider, is still the commit alone
    session.keys("<S-up>").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    assert_eq!(status(&session.app), "");
    assert_eq!(session.marked(), [] as [&str; 0]);
    session.keys("<down>").await;
    assert_eq!(session.cursor(), COMMITS[0]);
    // up to the uncommitted changes, which are not a commit
    session.keys("<up>").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    assert_eq!(status(&session.app), "");
    // no wrapping around the log
    session.keys("<S-up>").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    assert_eq!(status(&session.app), "");
    assert_eq!(session.marked(), [] as [&str; 0]);
    session.keys("<down>").await;
    assert_eq!(session.cursor(), COMMITS[0]);

    // over the divider, and a plain movement is the commit alone again
    session.keys("<S-down><S-down><S-down>").await;
    assert_eq!(status(&session.app), range(COMMITS[3], COMMITS[0], 4));
    session.keys("<up>").await;
    assert_eq!(status(&session.app), "");
    assert_eq!(session.marked(), [] as [&str; 0]);
    assert_eq!(session.cursor(), COMMITS[2]);

    session.keys("<end><S-down>").await;
    assert_eq!(session.cursor(), COMMITS[7]);
    assert_eq!(status(&session.app), "");
}

#[tokio::test(flavor = "multi_thread")]
async fn alt_s_selects_the_section_of_the_cursor() {
    let Some(_) = Repo::enter("alt_s_selects_the_section_of_the_cursor") else {
        return;
    };
    let mut session = Session::open_log().await;

    session.keys("<A-s>").await;
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[0], 3));
    assert_eq!(session.marked(), &COMMITS[1..3]);

    session.keys("<esc><down><down><down>").await;
    assert_eq!(session.cursor(), COMMITS[3]);
    session.keys("<A-s>").await;
    assert_eq!(status(&session.app), range(COMMITS[4], COMMITS[3], 2));
    assert_eq!(session.marked(), &COMMITS[4..5]);

    session.keys("<end><A-s>").await;
    assert_eq!(status(&session.app), range(COMMITS[7], COMMITS[5], 3));
    assert_eq!(session.marked(), &COMMITS[5..7]);

    // (extending goes on from the oldest commit of the section)
    session.keys("<esc><home><down><A-s><S-down>").await;
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[1], 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_range_covers_the_commits_the_query_hides() {
    let Some(_) = Repo::enter("the_range_covers_the_commits_the_query_hides") else {
        return;
    };
    let mut session = Session::open_log().await;
    session.keys("sFeature:").await;
    session
        .wait_for_screen("the matches", |rows| !on_screen(rows, "Add the parser"))
        .await;
    assert_eq!(session.log().len(), 2, "{:#?}", session.log());

    session.keys("<A-v><down>").await;
    assert_eq!(
        status(&session.app),
        format!("[extend] {}", range(COMMITS[3], COMMITS[0], 4))
    );
    session.keys("<ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "f2.txt"))
        .await;
    assert_eq!(
        session.list(),
        new_files(&["f2.txt", "f3.txt", "f4.txt", "m3.txt"])
    );
}

/// The rows of a list of changes of commits that add these files, which have one line each: a
/// divider and a hunk for each.
pub(super) fn new_files(paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .flat_map(|path| {
            [
                format!("── {path} (new)  1 hunk  +1"),
                format!("++ {path}:1"),
            ]
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_on_a_range_lists_the_combined_change_and_diffs_it() {
    let Some(_) = Repo::enter("enter_on_a_range_lists_the_combined_change_and_diffs_it") else {
        return;
    };
    let mut session = Session::open_log().await;
    session.keys("<A-s><ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "f3.txt"))
        .await;
    // the change from the parent of the oldest commit to the newest one (f3, f4 and what the
    // merge brought: m3)
    let expected = new_files(&["f3.txt", "f4.txt", "m3.txt"]);
    assert_eq!(session.list(), expected);
    assert_eq!(
        status(&session.app),
        format!(
            "{}: 3 changes in 3 files, +3",
            range(COMMITS[2], COMMITS[0], 3)
        )
    );

    // the diff of a file, with the base on the left and the newest commit on the right
    session.keys("<ret>").await;
    let left = format!("{}^: (new file)", id(COMMITS[2]));
    let right = format!("{}: f3.txt", id(COMMITS[0]));
    session
        .wait_for_screen("the diff", |rows| {
            on_screen(rows, &left) && on_screen(rows, &right)
        })
        .await;
    assert!(
        on_screen(&session.rows(), "1 + f3"),
        "{:#?}",
        session.rows()
    );

    // and back to the commit picker, with the range still selected
    session.keys("q").await;
    assert_eq!(session.list(), expected);
    session.keys("<esc>").await;
    assert_eq!(session.marked(), &COMMITS[1..3]);
    // (the status line keeps what the list said)
    assert_eq!(
        status(&session.app),
        format!(
            "{}: 3 changes in 3 files, +3",
            range(COMMITS[2], COMMITS[0], 3)
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_from_the_root_commit() {
    let Some(_) = Repo::enter("a_range_from_the_root_commit") else {
        return;
    };
    let mut session = Session::open_log().await;
    session.keys("<end><A-s><ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "m3.txt"))
        .await;
    assert_eq!(session.list(), new_files(&["m1.txt", "m2.txt", "m3.txt"]));
    session.keys("<ret>").await;
    let left = format!("{}^: (new file)", id(COMMITS[7]));
    let right = format!("{}: m1.txt", id(COMMITS[5]));
    session
        .wait_for_screen("the diff", |rows| {
            on_screen(rows, &left) && on_screen(rows, &right)
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_on_one_commit_is_what_it_was() {
    let Some(_) = Repo::enter("enter_on_one_commit_is_what_it_was") else {
        return;
    };
    let mut session = Session::open_log().await;

    // a merge commit: compared with its first parent
    session.keys("<down><ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "m3.txt"))
        .await;
    assert_eq!(session.list(), new_files(&["m3.txt"]));
    assert_eq!(
        status(&session.app),
        format!(
            "{}: 1 change in 1 file, +1 · {}",
            id(COMMITS[1]),
            COMMITS[1]
        )
    );
    session.keys("<ret>").await;
    let left = format!("{}^: (new file)", id(COMMITS[1]));
    let right = format!("{}: m3.txt", id(COMMITS[1]));
    session
        .wait_for_screen("the diff", |rows| {
            on_screen(rows, &left) && on_screen(rows, &right)
        })
        .await;
    session.keys("q<esc>").await;

    // extend mode with one commit is one commit
    session.keys("<A-v><ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "m3.txt"))
        .await;
    assert_eq!(
        status(&session.app),
        format!(
            "{}: 1 change in 1 file, +1 · {}",
            id(COMMITS[1]),
            COMMITS[1]
        )
    );
    // (Esc closes the files, then leaves extend mode)
    session.keys("<esc><esc>").await;

    // the root commit
    session.keys("<end><ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "m1.txt"))
        .await;
    assert_eq!(session.list(), new_files(&["m1.txt"]));
    session.keys("<ret>").await;
    let left = format!("{}^: (new file)", id(COMMITS[7]));
    session
        .wait_for_screen("the diff", |rows| on_screen(rows, &left))
        .await;
}

pub(super) const HINT: &str = "s filter, v extend, w/b section, enter files";

/// The query in the prompt of the picker on top.
pub(super) fn prompt(session: &Session) -> String {
    session.frame()[0].trim().to_string()
}

impl Session {
    /// Runs until the commits on the screen are `expected`, newest first.
    async fn wait_for_commits(&mut self, expected: &[&str]) {
        self.wait_for_screen("the commits", |rows| {
            let shown: Vec<_> = COMMITS
                .iter()
                .filter(|summary| on_screen(rows, summary))
                .collect();
            shown == expected.iter().collect::<Vec<_>>()
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_log_opens_in_normal_mode_where_the_letters_are_keys() {
    let Some(_) = Repo::enter("the_log_opens_in_normal_mode_where_the_letters_are_keys") else {
        return;
    };
    let mut session = Session::open_log().await;
    assert_eq!(status(&session.app), HINT);
    assert!(HINT.len() <= 50);

    // letters don't reach the prompt (`x` and `p` are no keys of the log), pasting neither
    session.keys("xp").await;
    assert_eq!(prompt(&session).split_whitespace().next(), Some("9/9"));
    assert_eq!(session.log().len(), 12);
    // moving keeps the hint
    session.keys("j").await;
    assert_eq!(status(&session.app), HINT);

    // `s` is the filter, where letters (also the ones that are keys) are typed
    session.keys("scrash").await;
    session
        .wait_for_screen("the match", |rows| !on_screen(rows, "Add the parser"))
        .await;
    assert_eq!(prompt(&session).split_whitespace().next(), Some("crash"));
    assert_eq!(session.cursor(), "Fix a crash on empty input");
    session.keys("vwjk").await;
    assert_eq!(
        prompt(&session).split_whitespace().next(),
        Some("crashvwjk")
    );
    session
        .keys("<backspace><backspace><backspace><backspace>")
        .await;
    assert_eq!(status(&session.app), HINT);

    // Esc is back to normal mode with the query: the letters are keys again
    session.keys("<esc>").await;
    assert_eq!(prompt(&session).split_whitespace().next(), Some("crash"));
    session.keys("xp").await;
    assert_eq!(prompt(&session).split_whitespace().next(), Some("crash"));
    assert_eq!(session.log().len(), 1);
    assert_eq!(session.cursor(), "Fix a crash on empty input");
    // and `s` resumes the editing
    session.keys("s!").await;
    assert_eq!(prompt(&session).split_whitespace().next(), Some("crash!"));
    session.keys("<backspace><esc>").await;

    // Esc in normal mode clears the query, then closes
    session.keys("<esc>").await;
    session
        .wait_for_screen("the whole log", |rows| on_screen(rows, "Add the parser"))
        .await;
    assert_eq!(session.log().len(), 12);
    assert_eq!(prompt(&session).split_whitespace().next(), Some("9/9"));
    session.keys("<esc>").await;
    assert!(!on_screen(&session.rows(), "origin/main"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cursor_cannot_be_seen_in_normal_mode() {
    let Some(_) = Repo::enter("the_cursor_cannot_be_seen_in_normal_mode") else {
        return;
    };
    let mut session = Session::open_log().await;
    let cursor = |session: &Session| session.app.cursor_kind();
    // the prompt shows it in filter mode
    assert_eq!(cursor(&session), vortex_view::graphics::CursorKind::Hidden);
    session.keys("s").await;
    assert_ne!(cursor(&session), vortex_view::graphics::CursorKind::Hidden);
    session.keys("<esc>").await;
    assert_eq!(cursor(&session), vortex_view::graphics::CursorKind::Hidden);
}

#[tokio::test(flavor = "multi_thread")]
async fn j_k_and_the_goto_keys_move_over_the_dividers() {
    let Some(_) = Repo::enter("j_k_and_the_goto_keys_move_over_the_dividers") else {
        return;
    };
    let mut session = Session::open_log().await;
    for expected in &COMMITS[1..] {
        session.keys("j").await;
        assert_eq!(session.cursor(), *expected);
    }
    for expected in COMMITS[..7].iter().rev() {
        session.keys("k").await;
        assert_eq!(session.cursor(), *expected);
    }
    session.keys("k").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    session.keys("G").await;
    assert_eq!(session.cursor(), COMMITS[7]);
    session.keys("gg").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    session.shifted('G').await;
    assert_eq!(session.cursor(), COMMITS[7]);
    session.keys("gg").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    // (the `e` of `ge` is not the key that extends to the end of the section)
    session.keys("ge").await;
    assert_eq!(session.cursor(), COMMITS[7]);
    assert_eq!(session.marked(), [] as [&str; 0]);
    // `g` and any other key does nothing
    session.keys("gxk").await;
    assert_eq!(session.cursor(), COMMITS[6]);
    // the other keys of the picker work as ever, and the preview key is not typed
    session.keys("<down><down>").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
}

#[tokio::test(flavor = "multi_thread")]
async fn v_extends_the_range_with_j_and_k() {
    let Some(_) = Repo::enter("v_extends_the_range_with_j_and_k") else {
        return;
    };
    let mut session = Session::open_log().await;
    session.keys("v").await;
    assert_eq!(status(&session.app), format!("[extend] {}", id(COMMITS[0])));
    session.keys("jj").await;
    assert_eq!(
        status(&session.app),
        format!("[extend] {}", range(COMMITS[2], COMMITS[0], 3))
    );
    assert_eq!(session.marked(), &COMMITS[..2]);
    // over a divider
    session.keys("j").await;
    assert_eq!(session.marked(), &COMMITS[..3]);
    // and back
    session.keys("kk").await;
    assert_eq!(
        status(&session.app),
        format!("[extend] {}", range(COMMITS[1], COMMITS[0], 2))
    );
    assert_eq!(session.marked(), &COMMITS[..1]);

    // Esc collapses to the commit under the cursor, and leaves extend mode
    session.keys("<esc>").await;
    assert_eq!(status(&session.app), "");
    assert_eq!(session.marked(), [] as [&str; 0]);
    assert_eq!(session.cursor(), COMMITS[1]);
    session.keys("j").await;
    assert_eq!(session.marked(), [] as [&str; 0]);

    // `v` again leaves extend mode and keeps the range, like `A-v`
    session.keys("vjj").await;
    assert_eq!(session.marked(), &COMMITS[2..4]);
    session.keys("v").await;
    assert_eq!(status(&session.app), range(COMMITS[4], COMMITS[2], 3));
    assert_eq!(session.marked(), &COMMITS[2..4]);

    // the keys that extend work in filter mode too, `v` is a letter there
    session.keys("<esc>").await;
    session.keys("s<A-v>v").await;
    assert_eq!(prompt(&session).split_whitespace().next(), Some("v"));
    assert!(status(&session.app).starts_with("[extend] "));
}

#[tokio::test(flavor = "multi_thread")]
async fn w_and_e_extend_to_the_oldest_commit_of_the_section() {
    let Some(_) = Repo::enter("w_and_e_extend_to_the_oldest_commit_of_the_section") else {
        return;
    };
    let mut session = Session::open_log().await;

    session.keys("w").await;
    assert_eq!(session.cursor(), COMMITS[2]);
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[0], 3));
    assert_eq!(session.marked(), &COMMITS[..2]);
    // it stops at the divider, again and with `e`
    session.keys("w").await;
    assert_eq!(session.cursor(), COMMITS[2]);
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[0], 3));
    session.keys("e").await;
    assert_eq!(session.cursor(), COMMITS[2]);
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[0], 3));

    // `w` doesn't leave extend mode on: a plain movement starts a new selection
    session.keys("j").await;
    assert_eq!(session.cursor(), COMMITS[3]);
    assert_eq!(status(&session.app), "");
    assert_eq!(session.marked(), [] as [&str; 0]);

    // from a commit in the middle of a section, without a range: the range starts there
    session.keys("e").await;
    assert_eq!(session.cursor(), COMMITS[4]);
    assert_eq!(status(&session.app), range(COMMITS[4], COMMITS[3], 2));
    session.keys("jj").await;
    assert_eq!(session.cursor(), COMMITS[6]);
    session.keys("w").await;
    assert_eq!(session.cursor(), COMMITS[7]);
    assert_eq!(status(&session.app), range(COMMITS[7], COMMITS[6], 2));
    assert_eq!(session.marked(), &COMMITS[6..7]);

    // in extend mode the anchor stays
    session.keys("<esc>gg").await;
    session.keys("jjv").await;
    assert_eq!(session.cursor(), COMMITS[1]);
    session.keys("w").await;
    assert_eq!(session.cursor(), COMMITS[2]);
    assert_eq!(
        status(&session.app),
        format!("[extend] {}", range(COMMITS[2], COMMITS[1], 2))
    );
    // (and it's still on: moving extends)
    session.keys("j").await;
    assert_eq!(
        status(&session.app),
        format!("[extend] {}", range(COMMITS[3], COMMITS[1], 3))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn b_extends_to_the_newest_commit_of_the_section() {
    let Some(_) = Repo::enter("b_extends_to_the_newest_commit_of_the_section") else {
        return;
    };
    let mut session = Session::open_log().await;

    // the first commit of the section, nothing to extend to
    session.keys("b").await;
    assert_eq!(session.cursor(), COMMITS[0]);
    assert_eq!(status(&session.app), HINT);

    session.keys("G").await;
    session.keys("b").await;
    assert_eq!(session.cursor(), COMMITS[5]);
    assert_eq!(status(&session.app), range(COMMITS[7], COMMITS[5], 3));
    assert_eq!(session.marked(), &COMMITS[6..]);
    // it stops at the divider
    session.keys("b").await;
    assert_eq!(session.cursor(), COMMITS[5]);
    assert_eq!(status(&session.app), range(COMMITS[7], COMMITS[5], 3));

    // with a range, the anchor stays: back to the end of the section the cursor is in
    session.keys("w").await;
    assert_eq!(session.cursor(), COMMITS[7]);
    assert_eq!(status(&session.app), "");
    assert_eq!(session.marked(), [] as [&str; 0]);

    // from the middle, without a range
    session.keys("kb").await;
    assert_eq!(session.cursor(), COMMITS[5]);
    assert_eq!(status(&session.app), range(COMMITS[6], COMMITS[5], 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_on_a_range_made_with_w_lists_the_combined_change() {
    let Some(_) = Repo::enter("enter_on_a_range_made_with_w_lists_the_combined_change") else {
        return;
    };
    let mut session = Session::open_log().await;
    session.keys("w<ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "f3.txt"))
        .await;
    let expected = new_files(&["f3.txt", "f4.txt", "m3.txt"]);
    assert_eq!(session.list(), expected);
    assert_eq!(
        status(&session.app),
        format!(
            "{}: 3 changes in 3 files, +3",
            range(COMMITS[2], COMMITS[0], 3)
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn w_and_b_go_to_the_matching_commits_of_the_section() {
    let Some(_) = Repo::enter("w_and_b_go_to_the_matching_commits_of_the_section") else {
        return;
    };
    let mut session = Session::open_log().await;
    // the commits with a colon: two in the first section, one in the second
    session.keys("s:").await;
    session
        .wait_for_commits(&[COMMITS[0], COMMITS[2], COMMITS[3]])
        .await;
    session.keys("<esc>").await;
    // (the order of the matches is not the order of the log)
    for _ in 0..3 {
        if session.cursor() == COMMITS[0] {
            break;
        }
        session.keys("j").await;
    }
    assert_eq!(session.cursor(), COMMITS[0]);

    // to the oldest commit of the section that is shown, over the one that isn't: the range has
    // them all
    session.keys("w").await;
    assert_eq!(session.cursor(), COMMITS[2]);
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[0], 3));
    session.keys("w").await;
    assert_eq!(session.cursor(), COMMITS[2]);
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[0], 3));
    session.keys("b").await;
    assert_eq!(session.cursor(), COMMITS[0]);
    assert_eq!(status(&session.app), "");

    // the only match of the second section is at both ends of it
    for _ in 0..3 {
        if session.cursor() == COMMITS[3] {
            break;
        }
        session.keys("j").await;
    }
    assert_eq!(session.cursor(), COMMITS[3]);
    session.keys("wb").await;
    assert_eq!(session.cursor(), COMMITS[3]);
    assert_eq!(status(&session.app), "");

    // Esc in normal mode: the range first, then the query, then the picker; in filter mode it
    // only goes to normal mode
    for _ in 0..3 {
        if session.cursor() == COMMITS[0] {
            break;
        }
        session.keys("j").await;
    }
    session.keys("w").await;
    // (the commit in between is not on the screen)
    assert_eq!(session.marked(), &COMMITS[..1]);
    session.keys("s<esc>").await;
    assert_eq!(session.marked(), &COMMITS[..1]);
    session.keys("<esc>").await;
    assert_eq!(session.marked(), [] as [&str; 0]);
    assert_eq!(prompt(&session).split_whitespace().next(), Some(":"));
    session.keys("<esc>").await;
    session
        .wait_for_screen("the whole log", |rows| on_screen(rows, "Add the parser"))
        .await;
    assert_eq!(session.log().len(), 12);
    session.keys("<esc>").await;
    assert!(!on_screen(&session.rows(), "origin/main"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_alt_keys_work_in_filter_mode() {
    let Some(_) = Repo::enter("the_alt_keys_work_in_filter_mode") else {
        return;
    };
    let mut session = Session::open_log().await;
    session.keys("s<A-s>").await;
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[0], 3));
    session.keys("<S-down>").await;
    assert_eq!(status(&session.app), range(COMMITS[2], COMMITS[1], 2));
    assert_eq!(prompt(&session).split_whitespace().next(), Some("9/9"));
}

// Commit hashes in the log: the abbreviation git makes of them, and the search by hash.

/// What `git rev-parse --short <id>` prints in the repository of the test.
fn git_short(id: &str) -> String {
    git(
        &vortex_stdx::env::current_working_dir(),
        "",
        &["rev-parse", "--short", id],
    )
    .trim()
    .to_string()
}

/// The row of the log with `summary`, from the start of the commit column.
fn commit_row(session: &Session, summary: &str) -> String {
    let log = session.log();
    let row = log
        .iter()
        .find(|row| row.contains(summary))
        .unwrap_or_else(|| panic!("no row with {summary:?}: {log:#?}"));
    row.trim_start_matches(" > ").trim_start().to_string()
}

fn theme_with(toml: &str) -> vortex_view::Theme {
    vortex_view::Theme::from(toml::from_str::<toml::Value>(toml).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commit_column_has_the_abbreviation_git_makes() {
    let Some(_) = Repo::enter("the_commit_column_has_the_abbreviation_git_makes") else {
        return;
    };
    let session = Session::open_log_on_uncommitted().await;

    for summary in COMMITS {
        let full = full_id(summary);
        let short = git_short(&full);
        assert!(short.len() >= 7 && full.starts_with(&short));
        let row = commit_row(&session, summary);
        // the abbreviation of git, then the date: the column is no wider than that
        assert!(row.starts_with(&format!("{short} ")), "{row:?}");
        // not longer than git abbreviates it: the next characters are not the ones of the id
        assert!(
            !row.contains(&full[..short.len() + 1]),
            "{row:?} has more than {short}"
        );
        assert_eq!(id(summary), short);
    }

    // the dividers and the uncommitted changes are what they were
    let log = session.log();
    let dividers: Vec<_> = log.iter().filter(|row| row.contains('─')).collect();
    assert_eq!(dividers.len(), 3, "{log:#?}");
    for (divider, title) in dividers
        .iter()
        .zip(["feature (unpushed)", "feature", "origin/main"])
    {
        assert!(divider.starts_with(&format!("── {title} ─")), "{divider:?}");
        assert!(divider.ends_with('─'));
        // (no hash in them)
        assert!(!COMMITS
            .iter()
            .any(|summary| divider.contains(&git_short(&full_id(summary)))));
    }
    assert!(log[0].contains(UNCOMMITTED) && !log[0].contains('─'));
    assert_eq!(log.len(), 12);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hash_is_drawn_subtly() {
    let Some(_) = Repo::enter("the_hash_is_drawn_subtly") else {
        return;
    };
    // the fg of the first cell of the hash of "Feature: tests", and of the cell of its summary
    async fn hash_fg(theme: &str) -> (vortex_view::graphics::Color, vortex_view::graphics::Color) {
        let mut session = Session::new();
        session.app.editor.theme = theme_with(theme);
        let session = Session::open_log_with(session).await;
        let row = session.row_of("Feature: tests");
        let text = &session.rows()[row];
        let short = git_short(&full_id("Feature: tests"));
        let x = text[..text.find(&short).unwrap()].chars().count() as u16;
        let screen = session.app.screen();
        let cell = |x: u16| screen.get(x, row as u16).unwrap().fg;
        // all of the hash is the same
        for i in 0..short.len() as u16 {
            assert_eq!(cell(x + i), cell(x));
        }
        let summary = text[..text.find("Feature: tests").unwrap()].chars().count() as u16;
        (cell(x), cell(summary))
    }
    use vortex_view::graphics::Color::Rgb;
    // the scope for text that is not the focus, when the theme has it
    let (hash, summary) =
        hash_fg("\"ui.text.inactive\" = { fg = \"#112233\" }\n\"comment\" = { fg = \"#445566\" }\n\"constant\" = { fg = \"#778899\" }\n\"ui.selection\" = { bg = \"#333333\" }")
            .await;
    assert_eq!(hash, Rgb(0x11, 0x22, 0x33));
    assert_ne!(summary, hash);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hash_falls_back_to_the_comment_style() {
    let Some(_) = Repo::enter("the_hash_falls_back_to_the_comment_style") else {
        return;
    };
    let mut session = Session::new();
    session.app.editor.theme = theme_with(
        "\"comment\" = { fg = \"#445566\" }\n\"constant\" = { fg = \"#778899\" }\n\"ui.selection\" = { bg = \"#333333\" }",
    );
    let session = Session::open_log_with(session).await;
    let row = session.row_of("Feature: tests");
    let text = &session.rows()[row];
    let short = git_short(&full_id("Feature: tests"));
    let x = text[..text.find(&short).unwrap()].chars().count() as u16;
    let fg = session.app.screen().get(x, row as u16).unwrap().fg;
    // (not the color of constants, which the hash had)
    assert_eq!(fg, vortex_view::graphics::Color::Rgb(0x44, 0x55, 0x66));
}

/// A repository whose log has hash-looking words in a summary, with no letter of a hash in the
/// other summaries (for a hex query not to match them by chance): a commit whose summary has
/// the first 4 digits of the hash of an older one.
///
/// ```text
/// * add <4 digits> cafe     <- fuzzy matches the 4 digits
/// * Sling stump
/// * Print only              <- the 4 digits are the start of its hash
/// * Mount sump
/// ```
fn make_hash_repo(parent: &Path) -> PathBuf {
    let work = parent.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, "", &["init", "-q"]);
    let mut repo = Repo { work, minute: 0 };
    repo.commit("a.txt", "Mount sump");
    repo.commit("b.txt", "Print only");
    repo.commit("c.txt", "Sling stump");
    let work = repo.work.clone();
    let id = git(&work, "", &["rev-parse", "HEAD~1"]);
    repo.commit("d.txt", &format!("add {} cafe", &id.trim()[..4]));
    work
}

/// The summaries of the commits on the screen, in order.
fn listed(session: &Session) -> Vec<String> {
    session
        .log()
        .iter()
        .map(|row| match row.split_once("Jane Doe") {
            Some((_, summary)) => summary.trim().to_string(),
            None => row.clone(),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_query_that_is_a_hash_prefix_lists_that_commit_first() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "a_query_that_is_a_hash_prefix_lists_that_commit_first",
        make_hash_repo,
    ) else {
        return;
    };
    let print = full_id("Print only");
    let p4 = print[..4].to_string();
    let with_p4 = format!("add {p4} cafe");
    // (the others don't have it, or the test would not tell anything)
    for other in ["Mount sump", "Sling stump", &with_p4] {
        assert!(!full_id(other).starts_with(&p4), "{other} starts with {p4}");
    }
    assert!(!git(&work, "", &["log", "--format=%H %s"]).is_empty());

    // (the log of this repository has no "Initial commit")
    let mut session = Session::new();
    session.keys("<space>l").await;
    session
        .wait_for_screen("the log", |rows| on_screen(rows, "Mount sump"))
        .await;
    session.keys("j").await;

    // the 4 digits: the commit whose hash starts with them, then the one whose summary has them
    session.keys(&format!("s{p4}")).await;
    session
        .wait_for_screen("the matches", |rows| !on_screen(rows, "Mount sump"))
        .await;
    assert_eq!(listed(&session), ["Print only", &with_p4]);
    // the cursor is on the first, which is the one the hash is of
    assert_eq!(session.cursor(), "Print only");
    assert!(
        session.frame()[0].ends_with("2/5"),
        "{:?}",
        session.frame()[0]
    );
    // no divider
    assert!(session.log().iter().all(|row| !row.contains('─')));

    // more of the hash (the abbreviation git makes, and more): only the commit has it
    let mut typed = p4.len();
    for len in [7, 12, 40] {
        session.keys(&"<backspace>".repeat(typed)).await;
        session.keys(&print[..len]).await;
        typed = len;
        session
            .wait_for_screen("the commit", |rows| {
                on_screen(rows, "Print only") && !on_screen(rows, &with_p4)
            })
            .await;
        assert_eq!(listed(&session), ["Print only"], "{len}");
        assert_eq!(session.cursor(), "Print only");
    }

    // 3 digits are not a hash: the commit is not looked for, only summaries are
    session.keys(&"<backspace>".repeat(typed)).await;
    session.keys(&p4[..3]).await;
    session
        .wait_for_screen("the match", |rows| on_screen(rows, &with_p4))
        .await;
    assert_eq!(listed(&session), [with_p4.as_str()]);
    assert_eq!(session.cursor(), with_p4);

    // 4 characters that are not the start of a hash: the summaries, as before
    assert!(!full_id("Mount sump").starts_with("cafe"));
    assert!(!print.starts_with("cafe") && !full_id(&with_p4).starts_with("cafe"));
    session.keys("<backspace><backspace><backspace>cafe").await;
    session
        .wait_for_screen("cafe", |rows| !on_screen(rows, "Print only"))
        .await;
    assert_eq!(listed(&session), [with_p4.as_str()]);

    // clearing the query is the whole log again
    session
        .keys("<backspace><backspace><backspace><backspace>")
        .await;
    session
        .wait_for_screen("the whole log", |rows| on_screen(rows, "Mount sump"))
        .await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    assert_eq!(listed(&session).len(), 6, "{:#?}", listed(&session));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commit_of_the_hash_is_listed_first_and_opens() {
    let Some(_) = Repo::enter("the_commit_of_the_hash_is_listed_first_and_opens") else {
        return;
    };
    let mut session = Session::open_log().await;
    // the oldest commit is the last row of the log, and the log of the others has dividers
    let oldest = COMMITS[7];
    let prefix = full_id(oldest)[..5].to_string();

    session.keys(&format!("s{prefix}")).await;
    session
        .wait_for_screen("the matches", |rows| on_screen(rows, oldest))
        .await;
    // (the others are listed if their summaries match the digits, which they might)
    let log = listed(&session);
    assert_eq!(log[0], oldest, "{log:#?}");
    assert_eq!(session.cursor(), oldest);
    // once each, and no divider
    assert_eq!(log.iter().filter(|row| row.as_str() == oldest).count(), 1);
    assert!(session.log().iter().all(|row| !row.contains('─')));
    let count = log.len();
    assert!(
        session.frame()[0].ends_with(&format!("{count}/9")),
        "{:?}",
        session.frame()[0]
    );

    // Enter opens it, as it does for a commit that was found by its summary
    session.keys("<ret>").await;
    session
        .wait_for_screen("the commit", |rows| on_screen(rows, "m1.txt"))
        .await;
    assert_eq!(
        status(&session.app),
        format!("{}: 1 change in 1 file, +1 · {oldest}", id(oldest))
    );
}
