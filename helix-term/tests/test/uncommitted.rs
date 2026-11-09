//! Tests of the uncommitted changes in the git commit log: the first row of the log, the picker of
//! files it opens, the diff of a file, and the lockfiles. They follow the working tree while they
//! are open.
//!
//! They use the repository of [`super::git_log`], which each test gets for itself in a process of
//! its own. The tests change it with git and `std::fs`, like another program would.

use std::{path::PathBuf, time::Duration};

use super::git_log::{git, on_screen, status, Repo, Session, COMMITS, UNCOMMITTED};

fn enter(test: &str) -> Option<PathBuf> {
    Repo::enter_in(module_path!(), test)
}

fn cwd() -> PathBuf {
    helix_stdx::env::current_working_dir()
}

fn write(path: &str, contents: &str) {
    std::fs::write(cwd().join(path), contents).unwrap();
}

/// `count` lines `{prefix}1`, `{prefix}2`...
fn lines(prefix: &str, count: usize) -> String {
    (1..=count).map(|i| format!("{prefix}{i}\n")).collect()
}

fn commit_all(message: &str) {
    git(&cwd(), "", &["add", "-A"]);
    git(&cwd(), "", &["commit", "-q", "-m", message]);
}

impl Session {
    /// The path column of the row under the cursor of the picker of files, with the `(+N -M)` of
    /// a lockfile.
    fn selected_file(&self) -> String {
        let frame = self.frame();
        let row = frame[3..]
            .iter()
            .find(|row| row.starts_with(" > "))
            .unwrap_or_else(|| panic!("no cursor: {frame:#?}"));
        row.trim_start_matches(" > ")
            .split_whitespace()
            .skip(2)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Waits for the first row of the log to say `label`.
    async fn wait_for_uncommitted(&mut self, label: &str) {
        let label = format!("{UNCOMMITTED} {label}");
        self.wait_for_screen(&label, |rows| on_screen(rows, &label))
            .await;
    }

    /// From the commit log: opens the picker of the uncommitted files.
    async fn open_uncommitted_files(&mut self) {
        self.keys("<ret>").await;
        self.wait_for_screen("the files", |rows| on_screen(rows, "path"))
            .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_uncommitted_files_are_sorted_with_the_lockfile_last() {
    let Some(_) = enter("the_uncommitted_files_are_sorted_with_the_lockfile_last") else {
        return;
    };
    write("Cargo.lock", &lines("a", 50));
    commit_all("Add the lockfile");

    write("m1.txt", "changed\n");
    write("new.txt", "new\n");
    git(&cwd(), "", &["mv", "f1.txt", "renamed.txt"]);
    write("Cargo.lock", &lines("b", 60));

    let mut session = Session::open_log_on_uncommitted().await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    session.wait_for_uncommitted("(4 files)").await;
    session.open_uncommitted_files().await;
    assert_eq!(
        session.files(),
        [
            "~ modified m1.txt",
            "+ untracked new.txt",
            "> renamed f1.txt -> renamed.txt",
            "~ modified Cargo.lock (+60 -50)",
        ]
    );
    assert_eq!(status(&session.app), "Uncommitted changes: 4 files");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_uncommitted_files_follow_the_working_tree_keeping_the_cursor() {
    let Some(_) = enter("the_uncommitted_files_follow_the_working_tree_keeping_the_cursor") else {
        return;
    };
    write("m2.txt", "changed\n");
    write("m3.txt", "changed\n");

    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(2 files)").await;
    session.open_uncommitted_files().await;
    assert_eq!(session.files(), ["~ modified m2.txt", "~ modified m3.txt"]);
    session.keys("<down>").await;
    assert_eq!(session.selected_file(), "m3.txt");

    // a file that sorts before the selected one, and a change of another
    write("a_new.txt", "new\n");
    write("m1.txt", "changed\n");
    session
        .wait_for_screen("the new files", |rows| on_screen(rows, "a_new.txt"))
        .await;
    session
        .wait_for_screen("all the files", |rows| on_screen(rows, "m1.txt"))
        .await;
    assert_eq!(
        session.files(),
        [
            "+ untracked a_new.txt",
            "~ modified m1.txt",
            "~ modified m2.txt",
            "~ modified m3.txt",
        ]
    );
    assert_eq!(session.selected_file(), "m3.txt");

    // and a file that goes away: the cursor stays on the one it was on
    git(&cwd(), "", &["checkout", "--", "m2.txt"]);
    session
        .wait_for_screen("the file gone", |rows| !on_screen(rows, "m2.txt"))
        .await;
    assert_eq!(session.selected_file(), "m3.txt");
    assert_eq!(session.files().len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_count_of_the_log_follows_the_working_tree_and_commits() {
    let Some(_) = enter("the_count_of_the_log_follows_the_working_tree_and_commits") else {
        return;
    };
    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(none)").await;

    write("one.txt", "1\n");
    session.wait_for_uncommitted("(1 file)").await;
    write("two.txt", "2\n");
    session.wait_for_uncommitted("(2 files)").await;
    assert_eq!(session.cursor(), UNCOMMITTED);

    // a commit: the log has it, at the top of its section, and nothing is uncommitted
    commit_all("Brand new commit");
    session
        .wait_for_screen("the new commit", |rows| on_screen(rows, "Brand new commit"))
        .await;
    session.wait_for_uncommitted("(none)").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
    let log = session.log();
    assert!(log[0].contains(UNCOMMITTED), "{log:#?}");
    assert!(log[1].contains("── feature (unpushed)"), "{log:#?}");
    assert!(log[2].contains("Brand new commit"), "{log:#?}");
    assert!(log[3].contains(COMMITS[0]), "{log:#?}");
    assert_eq!(log.len(), 13, "{log:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_log_keeps_the_cursor_on_the_same_commit_when_a_commit_arrives() {
    let Some(_) = enter("the_log_keeps_the_cursor_on_the_same_commit_when_a_commit_arrives") else {
        return;
    };
    let mut session = Session::open_log().await;
    session.keys("jj").await;
    assert_eq!(session.cursor(), COMMITS[2]);

    write("one.txt", "1\n");
    commit_all("Brand new commit");
    session
        .wait_for_screen("the new commit", |rows| on_screen(rows, "Brand new commit"))
        .await;
    assert_eq!(session.cursor(), COMMITS[2]);
    // (and the range is the commit alone again)
    assert_eq!(status(&session.app), super::git_log::HINT);
}

/// The number of the line of the right side of the diff at the top of the screen.
fn top_line(rows: &[String]) -> usize {
    // (the first row is the title of the sides)
    let (_, right) = rows[1]
        .split_once('│')
        .unwrap_or_else(|| panic!("{rows:#?}"));
    right
        .split_whitespace()
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("no line number in {right:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_diff_of_an_uncommitted_file_follows_the_working_tree() {
    let Some(_) = enter("the_diff_of_an_uncommitted_file_follows_the_working_tree") else {
        return;
    };
    let original = lines("line ", 400);
    write("big.txt", &original);
    commit_all("Add big.txt");
    write("big.txt", &original.replacen("line 5\n", "line five\n", 1));

    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(1 file)").await;
    session.open_uncommitted_files().await;
    session.keys("<ret>").await;
    session
        .wait_for_screen("the diff", |rows| on_screen(rows, "hunk 1/1"))
        .await;
    session.keys(&"j".repeat(100)).await;
    let top = top_line(&session.rows());
    assert!(top > 50, "{top}");

    // a line on the screen changes, and lines are added at the end: the diff has them, and the
    // line at the top is the same
    let edited = original.replacen("line 5\n", "line five\n", 1).replacen(
        &format!("line {}\n", top + 3),
        &format!("EXTERNAL EDIT {}\n", top + 3),
        1,
    );
    write("big.txt", &format!("{edited}{}", lines("appended ", 3)));
    session
        .wait_for_screen("the new line", |rows| on_screen(rows, "EXTERNAL EDIT"))
        .await;
    let rows = session.rows();
    assert_eq!(top_line(&rows), top, "{rows:#?}");
    assert!(on_screen(&rows, "hunk "), "{rows:#?}");
    assert!(!on_screen(&rows, "no changes"), "{rows:#?}");

    // back to what is in HEAD: nothing to show, but the diff stays open
    git(&cwd(), "", &["checkout", "--", "big.txt"]);
    session
        .wait_for_screen("no changes", |rows| on_screen(rows, "no changes"))
        .await;
    let rows = session.rows();
    assert!(on_screen(&rows, "q close"), "{rows:#?}");
    assert!(!on_screen(&rows, "EXTERNAL EDIT"), "{rows:#?}");

    // and it comes back with the next change, still at the same place
    write(
        "big.txt",
        &original.replacen(
            &format!("line {}\n", top + 5),
            &format!("SECOND EDIT {}\n", top + 5),
            1,
        ),
    );
    session
        .wait_for_screen("the change", |rows| on_screen(rows, "SECOND EDIT"))
        .await;
    let rows = session.rows();
    assert_eq!(top_line(&rows), top, "{rows:#?}");
    assert!(!on_screen(&rows, "no changes"), "{rows:#?}");
    // q closes the diff, and the files below are gone (opening the file closes them): the log
    // is not
    session.keys("q").await;
    assert!(!on_screen(&session.rows(), "hunk "));
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_lockfiles_are_collapsed_instead_of_the_default_ones() {
    let Some(_) = enter("configured_lockfiles_are_collapsed_instead_of_the_default_ones") else {
        return;
    };
    write("Cargo.lock", &lines("a", 20));
    write("deps.lock", &lines("a", 20));
    commit_all("Add the lockfiles");
    write("Cargo.lock", &lines("b", 25));
    write("deps.lock", &format!("{}{}", lines("a", 18), lines("c", 4)));
    write("zeta.txt", "zeta\n");

    let mut config = super::helpers::test_config();
    config.editor.changes.lockfiles = vec!["deps.lock".to_string()];
    let mut session = Session::open_log_with(Session::with_config(config)).await;
    session.wait_for_uncommitted("(3 files)").await;
    session.open_uncommitted_files().await;
    // (deps.lock: the last 2 of 20 lines go, 4 come)
    assert_eq!(
        session.files(),
        [
            "~ modified Cargo.lock",
            "+ untracked zeta.txt",
            "~ modified deps.lock (+4 -2)",
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_files_of_a_commit_have_the_lockfile_last_with_its_counts() {
    let Some(_) = enter("the_files_of_a_commit_have_the_lockfile_last_with_its_counts") else {
        return;
    };
    write("Cargo.lock", &lines("a", 50));
    commit_all("Add the lockfile");
    write("Cargo.lock", &lines("b", 60));
    write("a.txt", "a\n");
    write("z.txt", "z\n");
    commit_all("Update the dependencies");

    let mut session = Session::open_log().await;
    assert_eq!(session.cursor(), "Update the dependencies");
    session.keys("<ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "z.txt"))
        .await;
    assert_eq!(
        session.files(),
        [
            "+ added a.txt",
            "+ added z.txt",
            "~ modified Cargo.lock (+60 -50)",
        ]
    );
    // the commit that added it: a new file, all lines are added
    session.keys("<esc>j").await;
    assert_eq!(session.cursor(), "Add the lockfile");
    session.keys("<ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "Cargo.lock"))
        .await;
    assert_eq!(session.files(), ["+ added Cargo.lock (+50 -0)"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn closed_views_stop_following_the_working_tree() {
    let Some(_) = enter("closed_views_stop_following_the_working_tree") else {
        return;
    };
    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(none)").await;
    session.open_uncommitted_files().await;
    // all of them
    session.keys("<esc><esc>").await;
    assert!(!on_screen(&session.rows(), "path"));
    assert!(!on_screen(&session.rows(), "Initial commit"));

    // (a watch that was still on would have its views, and the editor, handle these)
    write("one.txt", "1\n");
    session.run_for(Duration::from_millis(700)).await;
    write("two.txt", "2\n");
    git(&cwd(), "", &["add", "-A"]);
    session.run_for(Duration::from_millis(700)).await;
    write("three.txt", "3\n");
    commit_all("Another commit");
    write("four.txt", "4\n");
    session.run_for(Duration::from_millis(700)).await;

    // the log shows what is now, from the start
    session.keys("<space>l").await;
    session
        .wait_for_screen("the log", |rows| on_screen(rows, "Another commit"))
        .await;
    session.wait_for_uncommitted("(1 file)").await;
    assert_eq!(session.cursor(), UNCOMMITTED);
}

/// The text of the open buffer of `path`.
fn buffer(session: &Session, path: &str) -> String {
    let path = cwd().join(path);
    session
        .app
        .editor
        .documents()
        .find(|doc| doc.path().is_some_and(|p| p == path))
        .unwrap_or_else(|| panic!("no buffer for {}", path.display()))
        .text()
        .to_string()
}

impl Session {
    /// Runs until `condition` is true of the application.
    async fn wait_for(&mut self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !condition(self) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            self.run_for(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn open_buffers_follow_the_files_while_the_log_is_open() {
    let Some(_) = enter("open_buffers_follow_the_files_while_the_log_is_open") else {
        return;
    };
    // m2.txt has unsaved changes, m1.txt doesn't
    let mut session = Session::new();
    session
        .keys(":o m2.txt<ret>ihello <esc>:o m1.txt<ret>")
        .await;
    let mut session = Session::open_log_with(session).await;

    // another program writes both, without the editor gaining focus or switching buffers
    let output = std::process::Command::new("sh")
        .args([
            "-c",
            "printf 'from another program\\n' > m1.txt; printf 'also changed\\n' > m2.txt",
        ])
        .current_dir(cwd())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    session
        .wait_for("the reload", |session| {
            buffer(session, "m1.txt") == "from another program\n"
        })
        .await;
    session
        .wait_for("the warning", |session| {
            status(&session.app).contains("m2.txt changed on disk")
        })
        .await;
    assert_eq!(buffer(&session, "m2.txt"), "hello m2.txt\n");
    assert_eq!(
        status(&session.app),
        "m2.txt changed on disk: :reload to load it, or :write! to overwrite it"
    );
}
