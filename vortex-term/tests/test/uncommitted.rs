//! Tests of the uncommitted changes in the git commit log: the first row of the log, the picker of
//! hunks it opens (marked staged, unstaged and unsaved), the diff of a file, and the lockfiles.
//! They follow the working tree and the modified buffers while they are open.
//!
//! They use the repository of [`super::git_log`], which each test gets for itself in a process of
//! its own. The tests change it with git and `std::fs`, like another program would.

use std::{path::PathBuf, time::Duration};

use super::git_log::{git, on_screen, status, Repo, Session, COMMITS, UNCOMMITTED};

fn enter(test: &str) -> Option<PathBuf> {
    Repo::enter_in(module_path!(), test)
}

fn cwd() -> PathBuf {
    vortex_stdx::env::current_working_dir()
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

/// The columns of a row of the picker of hunks, as `marks change`: the marks are 3 characters,
/// like ` M•`, `S  ` or `SM•`.
fn hunk_row(row: &str) -> String {
    // (the picker has the preview in a box of its own, on the right)
    let list = row.split("││").next().unwrap();
    let mut chars = list.chars().skip(3);
    let marks: String = chars.by_ref().take(3).collect();
    let change: String = chars.collect();
    format!("{marks} {}", change.trim())
}

impl Session {
    /// The rows of the picker of hunks (`marks change`, see [`hunk_row`]).
    pub(super) fn hunks(&self) -> Vec<String> {
        let frame = self.frame();
        assert!(
            frame[2].contains("marks") && frame[2].contains("change"),
            "{frame:#?}"
        );
        frame[3..]
            .iter()
            .map(|row| hunk_row(row))
            .filter(|row| !row.trim().is_empty())
            .collect()
    }

    /// The row under the cursor of the picker of hunks, as in [`Session::hunks`].
    pub(super) fn selected_hunk(&self) -> String {
        let frame = self.frame();
        let row = frame[3..]
            .iter()
            .find(|row| row.starts_with(" > "))
            .unwrap_or_else(|| panic!("no cursor: {frame:#?}"));
        hunk_row(row)
    }

    /// The preview of the row under the cursor: its lines, without the empty ones at the end.
    fn preview(&self) -> Vec<String> {
        let frame = self.frame();
        let mut lines: Vec<String> = frame
            .iter()
            .filter_map(|row| row.split_once("││"))
            .map(|(_, preview)| preview.trim_end().to_string())
            .collect();
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        lines
    }

    /// Waits for the first row of the log to say `label`.
    async fn wait_for_uncommitted(&mut self, label: &str) {
        let label = format!("{UNCOMMITTED} {label}");
        self.wait_for_screen(&label, |rows| on_screen(rows, &label))
            .await;
    }

    /// From the commit log: opens the picker of the uncommitted hunks.
    pub(super) async fn open_uncommitted_files(&mut self) {
        self.keys("<ret>").await;
        self.wait_for_screen("the hunks", |rows| on_screen(rows, "marks"))
            .await;
    }

    /// Waits for the picker of hunks to have exactly `rows`.
    async fn wait_for_hunks(&mut self, rows: &[&str]) {
        let expected: Vec<String> = rows.iter().map(|row| row.to_string()).collect();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while self.hunks() != expected {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for the hunks {expected:#?}, got {:#?}",
                self.hunks()
            );
            self.run_for(Duration::from_millis(50)).await;
        }
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
        session.hunks(),
        [
            " M  m1.txt:1",
            " M  new.txt:1 (untracked)",
            "S   renamed.txt (renamed from f1.txt)",
            " M  Cargo.lock (+60 -50)",
        ]
    );
    assert_eq!(
        status(&session.app),
        "Uncommitted changes: 4 changes in 4 files"
    );
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
    assert_eq!(session.hunks(), [" M  m2.txt:1", " M  m3.txt:1"]);
    session.keys("j").await;
    assert_eq!(session.selected_hunk(), " M  m3.txt:1");

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
        session.hunks(),
        [
            " M  a_new.txt:1 (untracked)",
            " M  m1.txt:1",
            " M  m2.txt:1",
            " M  m3.txt:1",
        ]
    );
    assert_eq!(session.selected_hunk(), " M  m3.txt:1");

    // and a file that goes away: the cursor stays on the one it was on
    git(&cwd(), "", &["checkout", "--", "m2.txt"]);
    session
        .wait_for_screen("the file gone", |rows| !on_screen(rows, "m2.txt"))
        .await;
    assert_eq!(session.selected_hunk(), " M  m3.txt:1");
    assert_eq!(session.hunks().len(), 3);
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
    // q closes the diff, and the list below is gone (opening the file closes them): the log
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
        session.hunks(),
        [
            " M  Cargo.lock:1-25",
            " M  zeta.txt:1 (untracked)",
            " M  deps.lock (+4 -2)",
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
    assert!(!on_screen(&session.rows(), "marks"));
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

/// `lines("line ", n)` with its line `n` replaced by `new`.
fn with_line(n: usize, new: &str, count: usize) -> String {
    lines("line ", count).replacen(&format!("line {n}\n"), &format!("{new}\n"), 1)
}

/// Types `text` at the start of the line `line` (from 1) of the current buffer: unsaved edits.
fn type_at(line: usize, text: &str) -> String {
    format!("{line}ggi{text}<esc>")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hunks_have_the_marks_of_where_their_changes_are() {
    let Some(_) = enter("the_hunks_have_the_marks_of_where_their_changes_are") else {
        return;
    };
    for path in ["a.txt", "b.txt", "d.txt", "e.txt", "clean.txt"] {
        write(path, &lines("line ", 20));
    }
    write("Cargo.lock", &lines("a", 50));
    commit_all("Add the files");

    // a.txt: a staged hunk and, apart from it, an unstaged one
    write("a.txt", &with_line(3, "three", 20));
    git(&cwd(), "", &["add", "a.txt"]);
    write(
        "a.txt",
        &with_line(3, "three", 20).replacen("line 15\n", "fifteen\n", 1),
    );
    // b.txt: staged, then changed again in the same place
    write("b.txt", &with_line(5, "five", 20));
    git(&cwd(), "", &["add", "b.txt"]);
    write("b.txt", &with_line(5, "FIVE", 20));
    // d.txt: changed on disk, and in its buffer inside of that hunk and elsewhere
    write("d.txt", &with_line(4, "four", 20));
    // e.txt: like b.txt, and edited in its buffer inside of the hunk
    write("e.txt", &with_line(8, "eight", 20));
    git(&cwd(), "", &["add", "e.txt"]);
    write("e.txt", &with_line(8, "EIGHT", 20));
    // untracked, and the lockfile
    write("new.txt", &lines("new ", 3));
    write("Cargo.lock", &lines("b", 60));

    // the buffers: clean.txt is clean on disk, ghost.txt isn't on disk
    let mut session = Session::new();
    session
        .keys(&format!(
            ":o d.txt<ret>{}{}:o e.txt<ret>{}:o clean.txt<ret>{}:o ghost.txt<ret>ihi<esc>",
            type_at(4, "ZZ"),
            type_at(12, "YY"),
            type_at(8, "XX"),
            type_at(2, "WW"),
        ))
        .await;
    let mut session = Session::open_log_with(session).await;
    session.wait_for_uncommitted("(6 files, 4 unsaved)").await;
    session.open_uncommitted_files().await;
    assert_eq!(
        session.hunks(),
        [
            "S   a.txt:3",
            " M  a.txt:15",
            "SM  b.txt:5",
            "  • clean.txt:2",
            " M• d.txt:4",
            "  • d.txt:12",
            "SM• e.txt:8",
            "  • ghost.txt:1 (untracked)",
            " M  new.txt:1-3 (untracked)",
            " M  Cargo.lock (+60 -50)",
        ]
    );
    assert_eq!(
        status(&session.app),
        "Uncommitted changes: 10 changes in 8 files, 5 unsaved"
    );
    // the preview says where the change is, and has the hunk
    session.keys("jj").await;
    assert_eq!(session.selected_hunk(), "SM  b.txt:5");
    let preview = session.preview();
    assert_eq!(preview[0].trim(), "b.txt:5 · staged, unstaged");
    assert!(
        preview.iter().any(|line| line.trim() == "- line 5"),
        "{preview:#?}"
    );
    assert!(
        preview.iter().any(|line| line.trim() == "+ FIVE"),
        "{preview:#?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_label_of_the_log_counts_the_unsaved_buffers() {
    let Some(_) = enter("the_label_of_the_log_counts_the_unsaved_buffers") else {
        return;
    };
    // nothing but an unsaved edit
    let mut session = Session::new();
    session
        .keys(&format!(":o m2.txt<ret>{}", type_at(1, "hello ")))
        .await;
    let mut session = Session::open_log_with(session).await;
    session.wait_for_uncommitted("(1 unsaved)").await;
    session.open_uncommitted_files().await;
    assert_eq!(session.hunks(), ["  • m2.txt:1"]);
    assert_eq!(
        status(&session.app),
        "Uncommitted changes: 1 change in 1 file, 1 unsaved"
    );
    session.keys("<esc>").await;

    // and a change on disk, with a second buffer that is modified
    write("m1.txt", "changed\n");
    session.wait_for_uncommitted("(1 file, 1 unsaved)").await;
    let mut session = {
        session.keys("<esc>").await;
        session
            .keys(&format!(":o m3.txt<ret>{}", type_at(1, "x")))
            .await;
        Session::open_log_with(session).await
    };
    session.wait_for_uncommitted("(1 file, 2 unsaved)").await;
}

impl Session {
    /// Inserts `text` at the start of the line `line` (from 0) of the open buffer of `path`,
    /// through the editor, the way something else than the keys of the user would.
    fn edit_buffer(&mut self, path: &str, line: usize, text: &str) {
        let path = cwd().join(path);
        let view = self.app.editor.tree.focus;
        let doc = self
            .app
            .editor
            .documents_mut()
            .find(|doc| doc.path().is_some_and(|p| p == path))
            .unwrap_or_else(|| panic!("no buffer for {}", path.display()));
        let at = doc.text().line_to_char(line);
        let transaction =
            vortex_core::Transaction::change(doc.text(), [(at, at, Some(text.into()))].into_iter());
        doc.apply(&transaction, view);
        // (what typing does too: the screen is drawn again)
        vortex_event::request_redraw();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hunks_follow_the_files_and_the_index_keeping_the_cursor() {
    let Some(_) = enter("the_hunks_follow_the_files_and_the_index_keeping_the_cursor") else {
        return;
    };
    write("a.txt", &lines("line ", 20));
    write("b.txt", &lines("line ", 20));
    commit_all("Add the files");
    write("a.txt", &with_line(3, "three", 20));
    let two_hunks = with_line(2, "two", 20).replacen("line 15\n", "fifteen\n", 1);
    write("b.txt", &two_hunks);

    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(2 files)").await;
    session.open_uncommitted_files().await;
    assert_eq!(
        session.hunks(),
        [" M  a.txt:3", " M  b.txt:2", " M  b.txt:15"]
    );
    session.keys("jj").await;
    assert_eq!(session.selected_hunk(), " M  b.txt:15");

    // another program: a new file that sorts first, and b.txt staged
    write("0_new.txt", "new\n");
    git(&cwd(), "", &["add", "b.txt"]);
    session
        .wait_for_hunks(&[
            " M  0_new.txt:1 (untracked)",
            " M  a.txt:3",
            "S   b.txt:2",
            "S   b.txt:15",
        ])
        .await;
    assert_eq!(session.selected_hunk(), "S   b.txt:15");

    // the first hunk of b.txt changed again, and a hunk before the cursor in a.txt
    write("b.txt", &two_hunks.replacen("two\n", "TWO\n", 1));
    write(
        "a.txt",
        &with_line(3, "three", 20).replacen("line 12\n", "twelve\n", 1),
    );
    session
        .wait_for_hunks(&[
            " M  0_new.txt:1 (untracked)",
            " M  a.txt:3",
            " M  a.txt:12",
            "SM  b.txt:2",
            "S   b.txt:15",
        ])
        .await;
    assert_eq!(session.selected_hunk(), "S   b.txt:15");

    // a file that goes away: the cursor stays where it was
    git(&cwd(), "", &["checkout", "--", "a.txt"]);
    session
        .wait_for_hunks(&[" M  0_new.txt:1 (untracked)", "SM  b.txt:2", "S   b.txt:15"])
        .await;
    assert_eq!(session.selected_hunk(), "S   b.txt:15");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hunks_follow_the_edits_of_the_buffers_keeping_the_cursor() {
    let Some(_) = enter("the_hunks_follow_the_edits_of_the_buffers_keeping_the_cursor") else {
        return;
    };
    write("a.txt", &lines("line ", 20));
    write("b.txt", &lines("line ", 20));
    commit_all("Add the files");
    write("b.txt", &with_line(10, "ten", 20));

    // both files have a buffer, which isn't modified
    let mut session = Session::new();
    session.keys(":o a.txt<ret>:o b.txt<ret>").await;
    let mut session = Session::open_log_with(session).await;
    session.wait_for_uncommitted("(1 file)").await;
    session.open_uncommitted_files().await;
    assert_eq!(session.hunks(), [" M  b.txt:10"]);

    // edits of the buffer (not typed: the list is open and has the keys), in the hunk and
    // apart from it
    session.edit_buffer("b.txt", 9, "ZZ");
    session.edit_buffer("b.txt", 1, "YY");
    session
        .wait_for_hunks(&["  • b.txt:2", " M• b.txt:10"])
        .await;
    assert_eq!(session.selected_hunk(), " M• b.txt:10");
    assert_eq!(
        status(&session.app),
        "Uncommitted changes: 1 change in 1 file"
    );

    // the buffer of another file: its rows come before the cursor
    session.edit_buffer("a.txt", 3, "XX");
    session
        .wait_for_hunks(&["  • a.txt:4", "  • b.txt:2", " M• b.txt:10"])
        .await;
    assert_eq!(session.selected_hunk(), " M• b.txt:10");
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_opens_the_diff_at_the_hunk_of_the_row() {
    let Some(_) = enter("enter_opens_the_diff_at_the_hunk_of_the_row") else {
        return;
    };
    write("a.txt", &lines("line ", 30));
    commit_all("Add a.txt");
    write(
        "a.txt",
        &with_line(3, "three", 30)
            .replacen("line 15\n", "fifteen\n", 1)
            .replacen("line 27\n", "twenty-seven\n", 1),
    );

    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(1 file)").await;
    session.open_uncommitted_files().await;
    assert_eq!(
        session.hunks(),
        [" M  a.txt:3", " M  a.txt:15", " M  a.txt:27"]
    );
    session.keys("j").await;
    session.keys("<ret>").await;
    session
        .wait_for_screen("the diff", |rows| on_screen(rows, "hunk 2/3"))
        .await;
    assert!(
        on_screen(&session.rows(), "fifteen"),
        "{:#?}",
        session.rows()
    );

    // back to the list, with the cursor on the row of the file's first hunk; the last row
    session.keys("q").await;
    session.keys("G<ret>").await;
    session
        .wait_for_screen("the diff", |rows| on_screen(rows, "hunk 3/3"))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_on_an_unsaved_hunk_opens_the_buffer_at_it() {
    let Some(_) = enter("enter_on_an_unsaved_hunk_opens_the_buffer_at_it") else {
        return;
    };
    write("a.txt", &lines("line ", 40));
    commit_all("Add a.txt");

    // an unsaved edit at line 27 of a.txt, and another buffer in the view
    let mut session = Session::new();
    session
        .keys(&format!(
            ":o a.txt<ret>{}:o m1.txt<ret>",
            type_at(27, "hello ")
        ))
        .await;
    let mut session = Session::open_log_with(session).await;
    session.wait_for_uncommitted("(1 unsaved)").await;
    session.open_uncommitted_files().await;
    assert_eq!(session.hunks(), ["  • a.txt:27"]);

    session.keys("<ret>").await;
    // the list and the log are gone, and the buffer is where the hunk is
    session
        .wait_for_screen("the buffer", |rows| !on_screen(rows, "marks"))
        .await;
    let rows = session.rows();
    assert!(!on_screen(&rows, "Initial commit"), "{rows:#?}");
    assert!(!rows.iter().any(|row| row.contains('┌')), "{rows:#?}");
    assert!(on_screen(&rows, "hello line 27"), "{rows:#?}");
    let (view, doc) = vortex_view::current_ref!(session.app.editor);
    assert_eq!(
        doc.path().map(|path| path.to_path_buf()),
        Some(cwd().join("a.txt"))
    );
    let text = doc.text();
    let cursor = doc.selection(view.id).primary().cursor(text.slice(..));
    assert_eq!(text.char_to_line(cursor), 26);
}
