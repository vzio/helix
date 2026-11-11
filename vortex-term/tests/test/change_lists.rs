//! Tests of the lists of changes of the commit log, which have the same shape for the uncommitted
//! changes, a commit and a range of commits: a divider row per file with its counts, a row per
//! hunk with what it does (`+`, `-`, `-+`, `++`, `--`), and dividers alone for the lockfiles,
//! the binary files and the files without changed lines.
//!
//! Like the tests of [`super::uncommitted`], each one runs in a process of its own with the
//! working directory in a throwaway repository, and changes it with git and `std::fs`.

use std::path::PathBuf;

use super::git_log::{git, id, on_screen, status, Repo, Session};
use super::uncommitted::{commit_all, cwd, lines, type_at, write};

fn enter(test: &str) -> Option<PathBuf> {
    Repo::enter_in(module_path!(), test)
}

/// `line 1`... `line {count}`.
fn text(count: usize) -> String {
    lines("line ", count)
}

fn mv(from: &str, to: &str) {
    git(&cwd(), "", &["mv", from, to]);
}

fn add(path: &str) {
    git(&cwd(), "", &["add", path]);
}

fn write_bytes(path: &str, bytes: &[u8]) {
    std::fs::write(cwd().join(path), bytes).unwrap();
}

/// The files that the tests change, committed: a.txt, b.txt, c.txt and old.txt (20 lines
/// each), gone.txt, logo.bin (binary) and Cargo.lock.
fn base() {
    for path in ["a.txt", "b.txt", "c.txt"] {
        write(path, &text(20));
    }
    write("old.txt", &lines("old ", 10));
    write("gone.txt", &lines("gone ", 3));
    write_bytes("logo.bin", &[0, 1, 2, 3]);
    write("Cargo.lock", &lines("lock ", 50));
    commit_all("Add the base files");
}

/// Changes of every kind, in the working tree:
/// - a.txt has three hunks: adds a line (line 6), removes one (line 10) and changes one
///   (line 15); the first is staged
/// - b.txt has two, at its lines 3 and 12
/// - gone.txt is deleted, logo.bin is changed, old.txt is renamed with a change (line 2)
/// - staged_new.txt is new and staged, untracked.txt isn't
/// - Cargo.lock has 60 lines instead of 50
fn change_everything() {
    let staged = text(20).replacen("line 5\n", "line 5\nadded\n", 1);
    write("a.txt", &staged);
    add("a.txt");
    write(
        "a.txt",
        &staged
            .replacen("line 10\n", "", 1)
            .replacen("line 15\n", "five\n", 1),
    );
    write(
        "b.txt",
        &text(20)
            .replacen("line 3\n", "three\n", 1)
            .replacen("line 12\n", "twelve\n", 1),
    );
    std::fs::remove_file(cwd().join("gone.txt")).unwrap();
    write_bytes("logo.bin", &[0, 9, 9, 9, 9]);
    mv("old.txt", "renamed.txt");
    write(
        "renamed.txt",
        &lines("old ", 10).replacen("old 2\n", "second\n", 1),
    );
    add("renamed.txt");
    write("staged_new.txt", "new 1\nnew 2\n");
    add("staged_new.txt");
    write("untracked.txt", "untracked\n");
    write("Cargo.lock", &lines("locked ", 60));
}

/// The rows of the list of the uncommitted changes after [`change_everything`].
fn uncommitted_rows() -> Vec<String> {
    [
        "── a.txt  3 hunks  +2 -2  1 staged",
        "S   + a.txt:6",
        " M  - a.txt:10",
        " M  -+ a.txt:15",
        "── b.txt  2 hunks  +2 -2",
        " M  -+ b.txt:3",
        " M  -+ b.txt:12",
        "── gone.txt (deleted)  1 hunk  -3",
        " M  -- gone.txt:1",
        "── logo.bin (binary)",
        "── old.txt → renamed.txt  1 hunk  +1 -1  1 staged",
        "S   -+ renamed.txt:2",
        "── staged_new.txt (new)  1 hunk  +2  1 staged",
        "S   ++ staged_new.txt:1-2",
        "── untracked.txt (untracked)  1 hunk  +1",
        " M  ++ untracked.txt:1",
        "── Cargo.lock  +60 -50",
    ]
    .iter()
    .map(|row| row.to_string())
    .collect()
}

/// The rows of the list of a commit that has the changes of [`change_everything`].
fn commit_rows() -> Vec<String> {
    [
        "── a.txt  3 hunks  +2 -2",
        "+ a.txt:6",
        "- a.txt:10",
        "-+ a.txt:15",
        "── b.txt  2 hunks  +2 -2",
        "-+ b.txt:3",
        "-+ b.txt:12",
        "── gone.txt (deleted)  1 hunk  -3",
        "-- gone.txt:1",
        "── logo.bin (binary)",
        "── old.txt → renamed.txt  1 hunk  +1 -1",
        "-+ renamed.txt:2",
        "── staged_new.txt (new)  1 hunk  +2",
        "++ staged_new.txt:1-2",
        "── untracked.txt (new)  1 hunk  +1",
        "++ untracked.txt:1",
        "── Cargo.lock  +60 -50",
    ]
    .iter()
    .map(|row| row.to_string())
    .collect()
}

/// Commits [`change_everything`], then a change of a.txt (line 18) and c.txt (line 1).
fn two_commits() {
    base();
    change_everything();
    commit_all("Change everything");
    write(
        "a.txt",
        &std::fs::read_to_string(cwd().join("a.txt"))
            .unwrap()
            .replacen("line 18\n", "late\n", 1),
    );
    write("c.txt", &text(20).replacen("line 1\n", "first\n", 1));
    commit_all("Touch a and c");
}

impl Session {
    /// From the log: opens the list of the uncommitted changes of `session` once the label of
    /// the log has `files`.
    async fn open_uncommitted_list(session: Session, files: &str) -> Session {
        let mut session = Session::open_log_with(session).await;
        session.wait_for_uncommitted(files).await;
        session.open_uncommitted_files().await;
        session
    }

    /// Waits for the footer of a diff, `hunk 2/3`, to be on the screen.
    async fn wait_for_diff_at(&mut self, hunk: &str) {
        let hunk = format!("{hunk} ");
        self.wait_for_screen(&hunk, |rows| {
            rows.iter().any(|row| {
                row.contains(&hunk)
                    && ["n/N", "s/u/r", "r revert"]
                        .iter()
                        .any(|keys| row.contains(keys))
            })
        })
        .await;
    }

    /// Asserts that the diff on the screen is of `path`, at the hunk `hunk` (`hunk 2/3`).
    fn assert_diff(&self, path: &str, hunk: &str) {
        let rows = self.rows();
        assert!(rows[0].contains(path), "not the diff of {path}: {rows:#?}");
        assert!(
            self.footer().starts_with(hunk),
            "{} is not at {hunk}: {rows:#?}",
            self.footer()
        );
    }

    /// From the log, with the cursor on a commit: opens the list of its changes.
    async fn open_commit_list(&mut self, expect: &str) {
        self.keys("<ret>").await;
        self.wait_for_screen("the list", |rows| on_screen(rows, expect))
            .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_uncommitted_list_has_dividers_with_counts_and_edit_marks() {
    let Some(_) = enter("the_uncommitted_list_has_dividers_with_counts_and_edit_marks") else {
        return;
    };
    base();
    change_everything();

    // b.txt is open with an unsaved edit at its line 18: one more hunk, and the unsaved count
    let mut session = Session::new();
    session
        .keys(&format!(":o b.txt<ret>{}", type_at(18, "ZZ")))
        .await;
    let session = Session::open_uncommitted_list(session, "(8 files, 1 unsaved)").await;

    let mut expected = uncommitted_rows();
    expected[4] = "── b.txt  3 hunks  +3 -3  1 unsaved".into();
    expected.insert(7, "  • -+ b.txt:18".into());
    assert_eq!(session.list(), expected);
    assert_eq!(
        status(&session.app),
        "Uncommitted changes: 12 changes in 8 files, +69 -59, 1 unsaved"
    );
    // the cursor starts on the first hunk, not on the divider above it
    assert_eq!(session.selected_row(), "S   + a.txt:6");
}

#[tokio::test(flavor = "multi_thread")]
async fn files_without_hunks_and_the_notes_of_their_dividers() {
    let Some(_) = enter("files_without_hunks_and_the_notes_of_their_dividers") else {
        return;
    };
    base();
    // a rename without a change, an empty new file, a binary lockfile, a staged binary file,
    // and a file that only gets lines (no removed ones: no zero count)
    mv("old.txt", "moved.txt");
    write("empty.txt", "");
    add("empty.txt");
    write_bytes("bun.lockb", &[0, 1]);
    write_bytes("logo.bin", &[0, 7]);
    add("logo.bin");
    write("a.txt", &format!("{}extra\n", text(20)));

    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(5 files)").await;
    session.open_uncommitted_files().await;
    assert_eq!(
        session.list(),
        [
            "── a.txt  1 hunk  +1",
            " M  + a.txt:21",
            "── empty.txt (new, empty)  staged",
            "── logo.bin (binary)  staged",
            "── old.txt → moved.txt (no lines changed)  staged",
            "── bun.lockb (untracked, binary)",
        ]
    );
    assert_eq!(
        status(&session.app),
        "Uncommitted changes: 5 changes in 5 files, +1"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn j_and_k_skip_the_dividers_and_stop_on_the_lockfile_and_binary_ones() {
    let Some(_) = enter("j_and_k_skip_the_dividers_and_stop_on_the_lockfile_and_binary_ones")
    else {
        return;
    };
    base();
    change_everything();
    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(8 files)").await;
    session.open_uncommitted_files().await;

    // down through every change: the dividers of files with hunks are skipped
    let down = [
        " M  - a.txt:10",
        " M  -+ a.txt:15",
        " M  -+ b.txt:3",
        " M  -+ b.txt:12",
        " M  -- gone.txt:1",
        "── logo.bin (binary)",
        "S   -+ renamed.txt:2",
        "S   ++ staged_new.txt:1-2",
        " M  ++ untracked.txt:1",
        "── Cargo.lock  +60 -50",
    ];
    for row in down {
        session.keys("j").await;
        assert_eq!(session.selected_row(), row);
    }
    // and up again
    for row in down.iter().rev().skip(1) {
        session.keys("k").await;
        assert_eq!(session.selected_row(), *row);
    }
    session.keys("k").await;
    assert_eq!(session.selected_row(), "S   + a.txt:6");
}

#[tokio::test(flavor = "multi_thread")]
async fn w_and_b_go_between_the_files() {
    let Some(_) = enter("w_and_b_go_between_the_files") else {
        return;
    };
    base();
    change_everything();
    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(8 files)").await;
    session.open_uncommitted_files().await;

    // w: the first change of the next file (the divider of a file without hunks)
    let firsts = [
        " M  -+ b.txt:3",
        " M  -- gone.txt:1",
        "── logo.bin (binary)",
        "S   -+ renamed.txt:2",
        "S   ++ staged_new.txt:1-2",
        " M  ++ untracked.txt:1",
        "── Cargo.lock  +60 -50",
    ];
    for row in firsts {
        session.keys("w").await;
        assert_eq!(session.selected_row(), row);
    }
    // (no wrap: the last file stays)
    session.keys("w").await;
    assert_eq!(session.selected_row(), "── Cargo.lock  +60 -50");

    // b: the previous file, whose first change it is
    session.keys("b").await;
    assert_eq!(session.selected_row(), " M  ++ untracked.txt:1");
    session.keys("bbbb").await;
    assert_eq!(session.selected_row(), " M  -- gone.txt:1");

    // inside of a file, b goes to its first hunk; only from there to the previous file
    session.keys("b").await;
    assert_eq!(session.selected_row(), " M  -+ b.txt:3");
    session.keys("jb").await;
    assert_eq!(session.selected_row(), " M  -+ b.txt:3");
    session.keys("b").await;
    assert_eq!(session.selected_row(), "S   + a.txt:6");
    // (and the first file stays)
    session.keys("b").await;
    assert_eq!(session.selected_row(), "S   + a.txt:6");
    session.keys("jjb").await;
    assert_eq!(session.selected_row(), "S   + a.txt:6");
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_opens_the_diff_at_the_hunk_or_of_the_lockfile() {
    let Some(_) = enter("enter_opens_the_diff_at_the_hunk_or_of_the_lockfile") else {
        return;
    };
    base();
    change_everything();
    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(8 files)").await;
    session.open_uncommitted_files().await;

    // the third hunk of a.txt
    session.keys("jj<ret>").await;
    session.wait_for_diff_at("hunk 3/3").await;
    session.assert_diff("a.txt", "hunk 3/3");
    // back to the list, with the cursor where it was: the second hunk of b.txt
    session.keys("q").await;
    assert_eq!(session.selected_row(), " M  -+ a.txt:15");
    session.keys("jj<ret>").await;
    session.wait_for_diff_at("hunk 2/2").await;
    session.assert_diff("b.txt", "hunk 2/2");
    session.keys("q").await;

    // the diff of the lockfile, from its divider (the cursor stops on it)
    session.keys("G").await;
    assert_eq!(session.selected_row(), "── Cargo.lock  +60 -50");
    session.keys("<ret>").await;
    session
        .wait_for_screen("the lockfile", |rows| on_screen(rows, "locked 1"))
        .await;
    assert!(
        session.rows()[0].contains("Cargo.lock"),
        "{:#?}",
        session.rows()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn filtering_keeps_the_dividers_of_the_files_with_matches() {
    let Some(_) = enter("filtering_keeps_the_dividers_of_the_files_with_matches") else {
        return;
    };
    base();
    change_everything();
    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(8 files)").await;
    session.open_uncommitted_files().await;

    // a query that matches one hunk: it, and the divider of its file
    session.keys("sb.txt:12").await;
    session
        .wait_for_screen("the match", |rows| !on_screen(rows, "a.txt"))
        .await;
    assert_eq!(
        session.list(),
        ["── b.txt  2 hunks  +2 -2", " M  -+ b.txt:12"]
    );
    assert_eq!(session.selected_row(), " M  -+ b.txt:12");

    // Esc leaves the filter mode and keeps the query; the next one clears it
    session.keys("<esc>").await;
    assert_eq!(
        session.list(),
        ["── b.txt  2 hunks  +2 -2", " M  -+ b.txt:12"]
    );
    session.keys("<esc>").await;
    session
        .wait_for_screen("every file", |rows| on_screen(rows, "Cargo.lock"))
        .await;
    assert_eq!(session.list(), uncommitted_rows());

    // a query that matches a path: all of its hunks, in the order of the list
    session.keys("sb.txt").await;
    session
        .wait_for_screen("the matches", |rows| !on_screen(rows, "a.txt"))
        .await;
    assert_eq!(
        session.list(),
        [
            "── b.txt  2 hunks  +2 -2",
            " M  -+ b.txt:3",
            " M  -+ b.txt:12",
        ]
    );
    assert_eq!(session.selected_row(), " M  -+ b.txt:3");

    // Esc, Esc: every row is back
    session.keys("<esc><esc>").await;
    session
        .wait_for_screen("every file", |rows| on_screen(rows, "Cargo.lock"))
        .await;
    assert_eq!(session.list(), uncommitted_rows());
}

#[tokio::test(flavor = "multi_thread")]
async fn filtering_finds_the_lockfile_and_binary_dividers() {
    let Some(_) = enter("filtering_finds_the_lockfile_and_binary_dividers") else {
        return;
    };
    base();
    change_everything();
    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(8 files)").await;
    session.open_uncommitted_files().await;

    session.keys("sCargo").await;
    session
        .wait_for_screen("the lockfile", |rows| !on_screen(rows, "a.txt"))
        .await;
    assert_eq!(session.list(), ["── Cargo.lock  +60 -50"]);
    assert_eq!(session.selected_row(), "── Cargo.lock  +60 -50");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_has_the_same_shape_as_the_uncommitted_changes() {
    let Some(_) = enter("a_commit_has_the_same_shape_as_the_uncommitted_changes") else {
        return;
    };
    two_commits();
    let mut session = Session::open_log().await;
    // the older commit changed everything
    session.keys("j").await;
    assert_eq!(session.cursor(), "Change everything");
    session.open_commit_list("Cargo.lock").await;
    assert_eq!(session.list(), commit_rows());
    assert_eq!(
        status(&session.app),
        format!(
            "{}: 11 changes in 8 files, +68 -58 · Change everything",
            id("Change everything")
        )
    );
    // the cursor starts on the first hunk, and walks like the uncommitted list
    assert_eq!(session.selected_row(), "+ a.txt:6");
    session.keys("jjj").await;
    assert_eq!(session.selected_row(), "-+ b.txt:3");
    session.keys("w").await;
    assert_eq!(session.selected_row(), "-- gone.txt:1");
    session.keys("jj").await;
    assert_eq!(session.selected_row(), "-+ renamed.txt:2");
    session.keys("b").await;
    assert_eq!(session.selected_row(), "── logo.bin (binary)");

    // the newer commit
    session.keys("<esc>").await;
    session.keys("k").await;
    assert_eq!(session.cursor(), "Touch a and c");
    session.open_commit_list("c.txt").await;
    assert_eq!(
        session.list(),
        [
            "── a.txt  1 hunk  +1 -1",
            "-+ a.txt:18",
            "── c.txt  1 hunk  +1 -1",
            "-+ c.txt:1",
        ]
    );
    assert_eq!(
        status(&session.app),
        format!(
            "{}: 2 changes in 2 files, +2 -2 · Touch a and c",
            id("Touch a and c")
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_has_the_combined_changes_in_the_same_shape() {
    let Some(_) = enter("a_range_has_the_combined_changes_in_the_same_shape") else {
        return;
    };
    two_commits();
    let mut session = Session::open_log().await;
    assert_eq!(session.cursor(), "Touch a and c");
    session.keys("vj<ret>").await;
    session
        .wait_for_screen("the list", |rows| on_screen(rows, "Cargo.lock"))
        .await;

    let mut expected = commit_rows();
    expected[0] = "── a.txt  4 hunks  +3 -3".into();
    expected.insert(4, "-+ a.txt:18".into());
    // c.txt sorts after b.txt
    expected.insert(8, "── c.txt  1 hunk  +1 -1".into());
    expected.insert(9, "-+ c.txt:1".into());
    assert_eq!(session.list(), expected);
    assert_eq!(
        status(&session.app),
        format!(
            "{}..{} (2 commits): 13 changes in 9 files, +70 -60",
            id("Change everything"),
            id("Touch a and c")
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_in_a_commit_opens_the_diff_at_the_hunk_or_of_the_lockfile() {
    let Some(_) = enter("enter_in_a_commit_opens_the_diff_at_the_hunk_or_of_the_lockfile") else {
        return;
    };
    two_commits();
    let mut session = Session::open_log().await;
    session.keys("j").await;
    assert_eq!(session.cursor(), "Change everything");
    session.open_commit_list("Cargo.lock").await;

    // the third hunk of a.txt
    session.keys("jj<ret>").await;
    session.wait_for_diff_at("hunk 3/3").await;
    session.assert_diff("a.txt", "hunk 3/3");
    session.keys("q").await;
    assert_eq!(session.selected_row(), "-+ a.txt:15");

    // the second hunk of b.txt
    session.keys("jj<ret>").await;
    session.wait_for_diff_at("hunk 2/2").await;
    session.assert_diff("b.txt", "hunk 2/2");
    session.keys("q").await;

    // the lockfile
    session.keys("G").await;
    assert_eq!(session.selected_row(), "── Cargo.lock  +60 -50");
    session.keys("<ret>").await;
    session
        .wait_for_screen("the lockfile", |rows| on_screen(rows, "locked 1"))
        .await;
    assert!(
        session.rows()[0].contains("Cargo.lock"),
        "{:#?}",
        session.rows()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_uncommitted_list_keeps_the_cursor_on_its_hunk_while_the_files_change() {
    let Some(_) = enter("the_uncommitted_list_keeps_the_cursor_on_its_hunk_while_the_files_change")
    else {
        return;
    };
    base();
    change_everything();
    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(8 files)").await;
    session.open_uncommitted_files().await;
    session.keys("jjjj").await;
    assert_eq!(session.selected_row(), " M  -+ b.txt:12");

    // another file changes: its rows come after the cursor
    write("c.txt", &text(20).replacen("line 1\n", "first\n", 1));
    session
        .wait_for("the rows of c.txt", |session| {
            session.list().contains(&" M  -+ c.txt:1".to_string())
        })
        .await;
    assert_eq!(session.selected_row(), " M  -+ b.txt:12");
    // and one that comes before
    write("0.txt", "zero\n");
    session
        .wait_for("the rows of 0.txt", |session| {
            session
                .list()
                .first()
                .is_some_and(|row| row.contains("0.txt"))
        })
        .await;
    assert_eq!(session.selected_row(), " M  -+ b.txt:12");

    // the hunk goes away: the cursor goes to the first change of its file
    write("b.txt", &text(20).replacen("line 3\n", "three\n", 1));
    session
        .wait_for("the hunk gone", |session| {
            !session.list().contains(&" M  -+ b.txt:12".to_string())
        })
        .await;
    assert_eq!(session.selected_row(), " M  -+ b.txt:3");
    assert!(
        session
            .list()
            .contains(&"── b.txt  1 hunk  +1 -1".to_string()),
        "{:#?}",
        session.list()
    );
}
