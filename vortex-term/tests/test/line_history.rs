//! Tests of the line history (`Space L`, `:line-history`): the commits that changed the cursor's
//! line or the selected lines, in the commit log's picker.
//!
//! The repositories of the tests that look at the log are made in the test, and the test runs
//! again in a process of its own with the repository as its working directory, as the tests of
//! the commit log do: see [`Repo::enter_with`]. The tests on the big generated repositories open
//! their file by its absolute path and don't care about the working directory.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use termina::event::{Event, KeyEvent};
use vortex_core::Position;
use vortex_term::commands::line_history::walks_running;
use vortex_view::input::parse_macro;

use super::git_log::{git, id, on_screen, status, Repo, Session, UNCOMMITTED};
use super::helpers::AppBuilder;
use super::repo_gen::{generate, RepoSpec};

// -- repositories ---------------------------------------------------------------------------

/// A repository on the branch `main` that is made commit by commit.
struct Builder {
    work: PathBuf,
    minute: u32,
}

impl Builder {
    fn new(parent: &Path) -> Builder {
        let work = parent.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, "", &["init", "-q"]);
        Builder { work, minute: 0 }
    }

    /// Writes `files` and commits them (and everything else that changed).
    fn commit(&mut self, summary: &str, files: &[(&str, &str)]) {
        for (name, contents) in files {
            let path = self.work.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        self.minute += 1;
        let date = format!("2024-03-01T12:{:02}:00+00:00", self.minute);
        git(&self.work, &date, &["add", "-A"]);
        git(&self.work, &date, &["commit", "-q", "-m", summary]);
    }
}

/// The lines of `notes.txt` at some versions: `3 alpha v2`.
fn notes(versions: [u32; 6]) -> String {
    let names = ["title", "intro", "alpha", "beta", "gamma", "delta"];
    let mut text = String::new();
    for (i, (name, version)) in names.iter().zip(versions).enumerate() {
        text += &format!("{} {name} v{version}\n", i + 1);
    }
    text
}

/// Five commits of `notes.txt`; three of them changed line 3 (alpha).
fn make_notes(parent: &Path) -> PathBuf {
    let mut repo = Builder::new(parent);
    repo.commit("Add the notes", &[("notes.txt", &notes([1; 6]))]);
    repo.commit(
        "Rename the title",
        &[("notes.txt", &notes([2, 1, 1, 1, 1, 1]))],
    );
    repo.commit("Reword alpha", &[("notes.txt", &notes([2, 1, 2, 1, 1, 1]))]);
    repo.commit("Fix beta", &[("notes.txt", &notes([2, 1, 2, 2, 1, 1]))]);
    repo.commit(
        "Reword alpha again",
        &[("notes.txt", &notes([2, 1, 3, 2, 1, 1]))],
    );
    repo.work
}

/// `names` as the lines of a file, each with a version.
fn list(versions: &[(&str, u32)]) -> String {
    versions
        .iter()
        .map(|(name, version)| format!("{name} v{version}\n"))
        .collect()
}

const NAMES: [&str; 8] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight",
];

fn numbers(versions: [u32; 8]) -> String {
    let lines: Vec<_> = NAMES.iter().copied().zip(versions).collect();
    list(&lines)
}

/// Changes of line 2 (two) and line 7 (seven), and of line 4 in between.
fn make_ranges(parent: &Path) -> PathBuf {
    let mut repo = Builder::new(parent);
    repo.commit("Add the list", &[("list.txt", &numbers([1; 8]))]);
    repo.commit(
        "Edit two",
        &[("list.txt", &numbers([1, 2, 1, 1, 1, 1, 1, 1]))],
    );
    repo.commit(
        "Edit four",
        &[("list.txt", &numbers([1, 2, 1, 2, 1, 1, 1, 1]))],
    );
    repo.commit(
        "Edit seven",
        &[("list.txt", &numbers([1, 2, 1, 2, 1, 1, 2, 1]))],
    );
    repo.commit(
        "Edit two again",
        &[("list.txt", &numbers([1, 3, 1, 2, 1, 1, 2, 1]))],
    );
    repo.work
}

/// Commits of `notes.txt` between commits of `other.txt`.
fn make_mixed(parent: &Path) -> PathBuf {
    let mut repo = Builder::new(parent);
    let other = |n: u32| format!("other v{n}\n");
    repo.commit(
        "Add the notes",
        &[("notes.txt", &notes([1; 6])), ("other.txt", &other(1))],
    );
    repo.commit("Update other", &[("other.txt", &other(2))]);
    repo.commit("Reword alpha", &[("notes.txt", &notes([1, 1, 2, 1, 1, 1]))]);
    repo.commit("Update other again", &[("other.txt", &other(3))]);
    repo.commit("Fix beta", &[("notes.txt", &notes([1, 1, 2, 2, 1, 1]))]);
    repo.commit("Reword title", &[("notes.txt", &notes([2, 1, 2, 2, 1, 1]))]);
    repo.commit("Update other once more", &[("other.txt", &other(4))]);
    repo.work
}

/// 24 lines, and a commit that changes three of them far apart (and another file).
fn long(version: &str) -> String {
    (1..=24)
        .map(|n| match (n, version) {
            (3 | 12 | 21, "changed") => format!("line {n:02} changed\n"),
            _ => format!("line {n:02}\n"),
        })
        .collect()
}

fn make_hunks(parent: &Path) -> PathBuf {
    let mut repo = Builder::new(parent);
    repo.commit(
        "Add the long file",
        &[("notes.txt", &long("first")), ("other.txt", "other v1\n")],
    );
    repo.commit(
        "Change three places",
        &[("notes.txt", &long("changed")), ("other.txt", "other v2\n")],
    );
    repo.commit("Change only other", &[("other.txt", "other v3\n")]);
    repo.work
}

fn make_untracked(parent: &Path) -> PathBuf {
    let mut repo = Builder::new(parent);
    repo.commit("Add the notes", &[("notes.txt", &notes([1; 6]))]);
    std::fs::write(repo.work.join("untracked.txt"), "nobody tracks me\n").unwrap();
    repo.work
}

// -- driving the application ----------------------------------------------------------------

/// A session that has `path` open with the cursor on `line` (from 1).
fn open_file(path: &Path, line: usize) -> Session {
    let app = AppBuilder::new()
        .with_file(
            path.to_path_buf(),
            Some(Position {
                row: line - 1,
                col: 0,
            }),
        )
        .build()
        .unwrap();
    Session::with_app(app)
}

/// The rows of the picker on top: `Uncommitted changes` for the first row, and the summary
/// (with ` (added)` if it has it) of each commit. Dividers are left out.
fn listed(session: &Session) -> Vec<String> {
    session
        .log()
        .iter()
        .filter_map(|row| {
            // (the preview is on the right of the list)
            let row = row.split("││").next().unwrap();
            if row.contains(UNCOMMITTED) {
                Some(UNCOMMITTED.to_string())
            } else {
                row.split_once("Jane Doe")
                    .map(|(_, summary)| summary.trim().to_string())
            }
        })
        .collect()
}

/// The summaries of the commits listed, without ` (added)`.
fn summaries(session: &Session) -> Vec<String> {
    listed(session)
        .into_iter()
        .map(|row| row.trim_end_matches(" (added)").to_string())
        .collect()
}

/// Runs the application until `condition` holds, and how long it took.
async fn wait_until(
    session: &mut Session,
    what: &str,
    limit: Duration,
    condition: impl Fn(&Session) -> bool,
) -> Duration {
    let start = Instant::now();
    while !condition(session) {
        assert!(
            start.elapsed() < limit,
            "timed out waiting for {what}\nstatus: {:?}\n{:#?}",
            status(&session.app),
            session
                .rows()
                .iter()
                .map(|row| row.trim_end())
                .filter(|row| !row.is_empty())
                .collect::<Vec<_>>()
        );
        session.pump(Duration::from_millis(5)).await;
    }
    start.elapsed()
}

/// What the line history says on the prompt row of its picker (the prompt, then its info and
/// the count).
fn info(session: &Session) -> String {
    session.info()
}

/// Whether the walk of the line history has ended: its info has the count and no `searching…`.
fn walk_ended(session: &Session) -> bool {
    let info = info(session);
    // (cut with a `…`: the end of it, which says whether it still searches, is not shown)
    (info.contains(" commit") || info.contains("committed yet"))
        && !info.contains("searching…")
        && !info.ends_with('…')
}

impl Session {
    /// Types `keys` without waiting for the application to be idle: the walks and the other
    /// background work go on while the test looks (see [`Session::pump`]).
    fn send(&mut self, keys: &str) {
        for key in parse_macro(keys).unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
    }

    /// Runs the application for at most `time`, no longer than it has something to do.
    async fn pump(&mut self, time: Duration) {
        self.app.editor.reset_idle_timer();
        let _ = tokio::time::timeout(time, self.app.event_loop(&mut self.input)).await;
    }

    /// Opens the line history with `keys` (`<space>L` or `:line-history<ret>`) and waits for the
    /// walk to end.
    async fn open_line_history(&mut self, keys: &str) {
        // wide: its info is cut when the list has the half of the terminal
        self.widen().await;
        self.keys(keys).await;
        wait_until(
            self,
            "the line history",
            Duration::from_secs(20),
            walk_ended,
        )
        .await;
    }

    /// Waits for the diff on the screen to have `text` in its footer.
    async fn wait_for_hunk_footer(&mut self, text: &str) {
        let text = text.to_string();
        self.wait_for_screen(&text.clone(), move |rows| {
            rows.iter().any(|row| row.trim().starts_with(text.as_str()))
        })
        .await;
    }

    /// Waits for the buffer's diff against HEAD (the one of the gutter) to have `hunks` hunks.
    async fn wait_for_hunks(&mut self, hunks: usize) {
        wait_until(
            self,
            "the diff of the buffer",
            Duration::from_secs(20),
            |s| {
                let editor = &s.app.editor;
                let view = editor.tree.get(editor.tree.focus);
                let doc = &editor.documents[&view.doc];
                doc.diff_handle()
                    .is_some_and(|d| d.load().len() as usize == hunks)
            },
        )
        .await;
    }
}

fn picker_is_open(session: &Session) -> bool {
    session
        .rows()
        .iter()
        .any(|row| row.trim_start().starts_with('┌'))
}

// -- the tests ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_cursors_line_lists_the_commits_that_changed_it() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "the_cursors_line_lists_the_commits_that_changed_it",
        make_notes,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 3);
    session.open_line_history("<space>L").await;

    // the three that changed line 3, newest first; not "Rename the title" or "Fix beta"
    assert_eq!(
        listed(&session),
        [
            "Reword alpha again",
            "Reword alpha",
            "Add the notes (added)"
        ],
        "{:#?}",
        session.log()
    );
    let rows = session.rows();
    assert!(!on_screen(&rows, "Rename the title"));
    assert!(!on_screen(&rows, "Fix beta"));
    let info = info(&session);
    assert!(info.contains("notes.txt:3 · 3 commits"), "{info:?}");
    // the keys are not in the status line, nor on the prompt row: `?` lists them
    assert!(!info.contains("enter diff"), "{info:?}");
    assert_eq!(status(&session.app), "");
}

#[tokio::test(flavor = "multi_thread")]
async fn selections_of_several_ranges_list_the_commits_of_all() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "selections_of_several_ranges_list_the_commits_of_all",
        make_ranges,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("list.txt"), 1);
    // two selections: "two" (line 2) and "seven" (line 7)
    session.keys("%stwo|seven<ret>").await;
    session.open_line_history("<space>L").await;

    // the commits of both lines, not "Edit four" of the line in between
    assert_eq!(
        listed(&session),
        [
            "Edit two again",
            "Edit seven",
            "Edit two",
            "Add the list (added)"
        ],
        "{:#?}",
        session.log()
    );
    let info = info(&session);
    assert!(info.contains("list.txt:2,7 · 4 commits"), "{info:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_whole_file_lists_every_commit_that_touched_it() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "the_whole_file_lists_every_commit_that_touched_it",
        make_mixed,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 1);
    session.open_line_history("%<space>L").await;

    let expected: Vec<String> = git(&work, "", &["log", "--format=%s", "--", "notes.txt"])
        .lines()
        .map(str::to_string)
        .collect();
    // (the test is no test when git lists all the commits)
    assert_eq!(expected.len(), 4, "{expected:?}");
    assert_eq!(summaries(&session), expected, "{:#?}", session.log());
    let info = info(&session);
    assert!(info.contains("notes.txt:1-6 · 4 commits"), "{info:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_uncommitted_row_is_there_when_the_line_has_unsaved_changes() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "the_uncommitted_row_is_there_when_the_line_has_unsaved_changes",
        make_notes,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 3);
    // an unsaved edit of line 3
    session.keys("A!<esc>").await;
    session.wait_for_hunks(1).await;
    session.open_line_history("<space>L").await;
    assert_eq!(
        listed(&session),
        [
            UNCOMMITTED,
            "Reword alpha again",
            "Reword alpha",
            "Add the notes (added)"
        ],
        "{:#?}",
        session.log()
    );
    assert!(info(&session).contains("· 3 commits"));
    session.keys("<esc>").await;
    assert!(!picker_is_open(&session));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_of_another_line_does_not_make_an_uncommitted_row() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "an_edit_of_another_line_does_not_make_an_uncommitted_row",
        make_notes,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 3);
    // an unsaved edit of line 5, then back to line 3
    session.keys("jjA!<esc>kk").await;
    session.wait_for_hunks(1).await;
    session.open_line_history("<space>L").await;
    assert_eq!(
        listed(&session),
        [
            "Reword alpha again",
            "Reword alpha",
            "Add the notes (added)"
        ],
        "{:#?}",
        session.log()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lines_that_only_exist_in_the_buffer_have_the_uncommitted_row_alone() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "lines_that_only_exist_in_the_buffer_have_the_uncommitted_row_alone",
        make_notes,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 3);
    // a new line below line 3, with the cursor on it
    session.keys("onothing like it<esc>").await;
    session.wait_for_hunks(1).await;
    session.open_line_history("<space>L").await;
    let info = info(&session);
    assert!(
        info.contains("notes.txt:4 · these lines aren't committed yet"),
        "{info:?}"
    );
    assert_eq!(listed(&session), [UNCOMMITTED], "{:#?}", session.log());
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_commit_that_added_the_lines_is_marked_added() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "only_the_commit_that_added_the_lines_is_marked_added",
        make_notes,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 3);
    session.open_line_history("<space>L").await;
    let rows = listed(&session);
    assert_eq!(rows.len(), 3, "{rows:?}");
    // the oldest, which is the last row
    assert!(rows[2].ends_with("(added)"), "{rows:?}");
    assert!(rows[2].starts_with("Add the notes"), "{rows:?}");
    for row in &rows[..2] {
        assert!(!row.contains("(added)"), "{rows:?}");
    }
    // on the screen too
    let screen = session.rows();
    assert_eq!(
        screen.iter().filter(|row| row.contains("(added)")).count(),
        1,
        "{screen:#?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_opens_the_diff_at_the_hunk_that_touches_the_line() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "enter_opens_the_diff_at_the_hunk_that_touches_the_line",
        make_hunks,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 12);
    session.open_line_history("<space>L").await;
    assert_eq!(
        listed(&session),
        ["Change three places", "Add the long file (added)"]
    );

    session.keys("<ret>").await;
    // the second of the three hunks of the commit, not the first
    session.wait_for_hunk_footer("hunk 2/3").await;
    let (left, right) = (
        format!("{}^: notes.txt", id("Change three places")),
        format!("{}: notes.txt", id("Change three places")),
    );
    let rows = session.rows();
    assert!(on_screen(&rows, &left), "{rows:#?}");
    assert!(on_screen(&rows, &right), "{rows:#?}");
    // the right side has the line
    assert!(on_screen(&rows, "line 12 changed"), "{rows:#?}");

    // q goes back to the picker
    session.keys("q").await;
    assert_eq!(
        listed(&session),
        ["Change three places", "Add the long file (added)"],
        "{:#?}",
        session.rows()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn o_opens_everything_the_commit_changed() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "o_opens_everything_the_commit_changed",
        make_hunks,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 12);
    session.open_line_history("<space>L").await;
    assert!(session.log().len() >= 2);

    session.keys("o").await;
    session
        .wait_for_screen("the list of changes", |rows| on_screen(rows, "other.txt"))
        .await;
    let short = id("Change three places");
    // the totals are on the prompt row of the list
    let info = info(&session);
    assert!(
        info.contains(&format!("{short}: ")) && info.contains("changes in 2 files"),
        "{info:?}"
    );
    assert_eq!(status(&session.app), "");
    let rows = session.rows();
    // the tracked file and the other one the commit changed
    assert!(on_screen(&rows, "notes.txt"), "{rows:#?}");
    assert!(on_screen(&rows, "other.txt"), "{rows:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_of_commits_opens_the_diff_over_the_range() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "a_range_of_commits_opens_the_diff_over_the_range",
        make_notes,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 3);
    session.open_line_history("<space>L").await;
    assert!(session.log().len() >= 3);

    // the newest two: "Reword alpha again" and "Reword alpha"
    session.keys("vj<ret>").await;
    let left = format!("{}^: notes.txt", id("Reword alpha"));
    let right = format!("{}: notes.txt", id("Reword alpha again"));
    session
        .wait_for_screen("the diff over the range", |rows| {
            on_screen(rows, &left) && on_screen(rows, &right)
        })
        .await;
    // from the first parent of the older one to the newer one: alpha went from v1 to v3
    let rows = session.rows();
    assert!(on_screen(&rows, "3 alpha v3"), "{rows:#?}");
    assert!(on_screen(&rows, "3 alpha v1"), "{rows:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_buffer_without_a_file_gives_an_error_and_no_picker() {
    let mut session = Session::new();
    session.keys(":new<ret>").await;
    session.keys("<space>L").await;
    let status = status(&session.app);
    assert!(status.contains("file"), "{status:?}");
    assert!(!picker_is_open(&session), "{:#?}", session.rows());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_outside_a_repository_gives_an_error_and_no_picker() {
    let dir = tempfile::tempdir().unwrap();
    let file = std::fs::canonicalize(dir.path()).unwrap().join("loose.txt");
    std::fs::write(&file, "no repository here\n").unwrap();
    let mut session = open_file(&file, 1);
    session.keys("<space>L").await;
    let status = status(&session.app);
    assert!(status.contains("isn't in a git repository"), "{status:?}");
    assert!(status.contains("loose.txt"), "{status:?}");
    assert!(!picker_is_open(&session), "{:#?}", session.rows());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_untracked_file_gives_an_error_and_no_picker() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "an_untracked_file_gives_an_error_and_no_picker",
        make_untracked,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("untracked.txt"), 1);
    session.keys("<space>L").await;
    let status = status(&session.app);
    assert!(status.contains("isn't tracked by git"), "{status:?}");
    assert!(status.contains("untracked.txt"), "{status:?}");
    assert!(!picker_is_open(&session), "{:#?}", session.rows());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_typable_command_does_what_the_key_does() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "the_typable_command_does_what_the_key_does",
        make_notes,
    ) else {
        return;
    };
    // (one application per test: the commands typed in a second one do nothing)
    let mut session = open_file(&work.join("notes.txt"), 3);
    session.open_line_history(":line-history<ret>").await;

    // what `<space>L` lists for the same line
    assert_eq!(
        listed(&session),
        [
            "Reword alpha again",
            "Reword alpha",
            "Add the notes (added)"
        ],
        "{:#?}",
        session.log()
    );
    let info = info(&session);
    assert!(info.contains("notes.txt:3 · 3 commits"), "{info:?}");
    // the keys are not in the status line, nor on the prompt row: `?` lists them
    assert!(!info.contains("enter diff"), "{info:?}");
    assert_eq!(status(&session.app), "");
}

/// How many places the focused view's history has.
fn places(session: &Session) -> usize {
    let editor = &session.app.editor;
    editor.tree.get(editor.tree.focus).history.places().count()
}

#[tokio::test(flavor = "multi_thread")]
async fn picking_a_commit_records_one_history_jump() {
    let Some(work) = Repo::enter_with(
        module_path!(),
        "picking_a_commit_records_one_history_jump",
        make_hunks,
    ) else {
        return;
    };
    let mut session = open_file(&work.join("notes.txt"), 12);
    session.open_line_history("<space>L").await;
    let before = places(&session);
    // (opening the picker is no jump)

    session.keys("<ret>").await;
    session.wait_for_hunk_footer("hunk 2/3").await;
    assert_eq!(places(&session), before + 1);

    // and back in the picker: nothing more is recorded
    session.keys("q").await;
    assert_eq!(places(&session), before + 1);
}

// -- big repositories -----------------------------------------------------------------------

/// A repository of `commits` commits that touch the tracked file every `touch_every`th commit.
fn big(commits: usize, touch_every: usize) -> super::repo_gen::GeneratedRepo {
    let start = Instant::now();
    let repo = generate(&RepoSpec {
        commits,
        files: 40,
        tracked_path: "data/tracked.txt",
        tracked_lines: 200,
        touch_every,
        tracked_from: 0,
    });
    eprintln!("generated {commits} commits in {:?}", start.elapsed());
    repo
}

/// The summary of the newest commit that touches the tracked file of [`big`].
fn newest_touch(commits: usize, touch_every: usize) -> String {
    let index = (commits - 1) / touch_every * touch_every;
    format!("{index}: data/tracked.txt")
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_picker_stops_the_walk() {
    let repo = big(20_000, 50);
    let mut session = open_file(&repo.tracked, 1);
    session.keys("%").await;
    session.send("<space>L");

    // the walk goes on in the background...
    let started = wait_until(
        &mut session,
        "the walk to run",
        Duration::from_secs(20),
        |_| walks_running(&repo.tracked) == 1,
    )
    .await;
    eprintln!("walk seen running after {started:?}");
    assert!(
        info(&session).contains("searching…") || !walk_ended(&session),
        "the walk ended before it could be stopped: {:?}",
        info(&session)
    );

    // ...until the picker closes
    let closed = Instant::now();
    session.send("q");
    wait_until(
        &mut session,
        "the walk to stop",
        Duration::from_secs(1),
        |_| walks_running(&repo.tracked) == 0,
    )
    .await;
    eprintln!("walk stopped {:?} after q", closed.elapsed());
    assert!(!picker_is_open(&session), "{:#?}", session.rows());
}

/// Opens the line history of the whole tracked file of [`big`] and measures how long the first
/// commit takes to be on the screen, and the whole walk.
async fn measure(commits: usize, touch_every: usize, limit: Duration) -> (Duration, Duration) {
    let repo = big(commits, touch_every);
    let mut session = open_file(&repo.tracked, 1);
    session.keys("%").await;
    // wide: the info of the prompt row is cut in a list with a preview
    session.widen().await;
    let first = newest_touch(commits, touch_every);

    let start = Instant::now();
    session.send("<space>L");
    wait_until(&mut session, "the first commit", limit, |s| {
        on_screen(&s.rows(), &first)
    })
    .await;
    let first_row = start.elapsed();
    // the prompt row says how it goes: still searching, or the count of the end
    let progress = info(&session);
    assert!(
        progress.contains("searching…") || progress.contains(" commit"),
        "{progress:?}"
    );

    wait_until(&mut session, "the walk to end", limit, walk_ended).await;
    let walk = start.elapsed();
    let end = info(&session);
    eprintln!(
        "{commits} commits, touched every {touch_every}: first row after {first_row:?} \
         (status then: {progress:?}), walk ended after {walk:?}: {end:?}"
    );
    // every touch is listed, the one that created the file too
    // (the count of the picker: the info says it too, but is cut by the long path). The matcher
    // may take the last rows a frame after the walk ended, under load.
    let count = format!("{0}/{0}", repo.touches);
    let listed = |session: &Session| {
        let row = session.frame()[0].clone();
        row.split("││").next().unwrap().trim_end().ends_with(&count)
    };
    wait_until(&mut session, "every touch to be listed", limit, listed).await;
    let row = session.frame()[0].clone();
    let row = row.split("││").next().unwrap().trim_end();
    assert!(
        row.ends_with(&format!("{0}/{0}", repo.touches)),
        "{row:?}, expected {} commits",
        repo.touches
    );
    (first_row, walk)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_commit_shows_fast_in_a_big_repository() {
    let (first_row, _) = measure(20_000, 25, Duration::from_secs(120)).await;
    assert!(
        first_row < Duration::from_secs(2),
        "the first commit took {first_row:?}"
    );
}

/// 50,000 commits, once: `cargo integration-test -- --ignored very_big --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "50,000 commits (about 7 s in a debug build): run it on purpose"]
async fn very_big_repository_line_history() {
    measure(50_000, 25, Duration::from_secs(900)).await;
}
