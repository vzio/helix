//! Tests of the lists of changes of the commit log that update while the log is open: the totals
//! of the uncommitted changes in the status line, and the commits and ranges opened in the
//! background.
//!
//! Like the tests of [`super::uncommitted`], each one runs in a process of its own with the
//! working directory in a throwaway repository, and changes it with git and `std::fs`.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::git_log::{git, id, on_screen, range, status, Repo, Session};
use super::uncommitted::{commit_all, cwd, lines, write};

fn enter(test: &str) -> Option<PathBuf> {
    Repo::enter_in(module_path!(), test)
}

/// `text` (lines `line 1`...) with the line `line {n}` replaced.
fn change(text: &str, n: usize, new: &str) -> String {
    text.replacen(&format!("line {n}\n"), &format!("{new}\n"), 1)
}

impl Session {
    /// Runs until the prompt row of the list on top says `expected`.
    async fn wait_for_info(&mut self, expected: &str) {
        self.widen().await;
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.info_is(expected) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the info {expected:?}, it is {:?}",
                self.info()
            );
            self.run_for(Duration::from_millis(50)).await;
        }
    }

    /// Whether the picker on top is the commit log.
    fn log_is_on_top(&self) -> bool {
        // the commits have authors, the lists of changes don't
        self.rows().iter().any(|row| row.contains("Jane Doe"))
    }

    fn picker_is_open(&self) -> bool {
        self.rows()
            .iter()
            .any(|row| row.trim_start().starts_with('┌'))
    }
}

/// a.txt with 20 lines, committed, and changed at line 3 on disk (one hunk).
fn one_hunk_in_a() -> String {
    write("a.txt", &lines("line ", 20));
    commit_all("Add a.txt");
    let one = change(&lines("line ", 20), 3, "three");
    write("a.txt", &one);
    one
}

#[tokio::test(flavor = "multi_thread")]
async fn the_totals_follow_the_working_tree() {
    let Some(_) = enter("the_totals_follow_the_working_tree") else {
        return;
    };
    let one = one_hunk_in_a();

    let mut session = Session::open_log_on_uncommitted().await;
    session.wait_for_uncommitted("(1 file)").await;
    session.open_uncommitted_files().await;
    session
        .assert_info("Uncommitted changes: 1 change in 1 file, +1 -1")
        .await;

    // another program: a second hunk
    write("a.txt", &change(&one, 15, "fifteen"));
    session
        .wait_for_info("Uncommitted changes: 2 changes in 1 file, +2 -2")
        .await;
    // an untracked file (a change of its own, with all its lines added)
    write("new.txt", &lines("new ", 3));
    session
        .wait_for_info("Uncommitted changes: 3 changes in 2 files, +5 -2")
        .await;
    // a file that goes away
    git(&cwd(), "", &["checkout", "--", "a.txt"]);
    session
        .wait_for_info("Uncommitted changes: 1 change in 1 file, +3")
        .await;
    // a change that is staged: the same totals, the list has its marks
    git(&cwd(), "", &["add", "new.txt"]);
    session
        .wait_for_list(&[
            "── new.txt (new)  1 hunk  +3  1 staged",
            "S   ++ new.txt:1-3",
        ])
        .await;
    session
        .assert_info("Uncommitted changes: 1 change in 1 file, +3")
        .await;
    // and nothing left
    git(&cwd(), "", &["rm", "-q", "-f", "new.txt"]);
    session.wait_for_info("No uncommitted changes").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_totals_count_the_unsaved_edits_of_the_buffers() {
    let Some(_) = enter("the_totals_count_the_unsaved_edits_of_the_buffers") else {
        return;
    };
    one_hunk_in_a();

    let mut session = Session::new();
    session.keys(":o a.txt<ret>").await;
    let mut session = Session::open_log_with(session).await;
    session.wait_for_uncommitted("(1 file)").await;
    session.open_uncommitted_files().await;
    session
        .assert_info("Uncommitted changes: 1 change in 1 file, +1 -1")
        .await;

    // an edit far from the hunk: a change of its own
    session.edit_buffer("a.txt", 10, "ZZ");
    session
        .wait_for_info("Uncommitted changes: 2 changes in 1 file, +2 -2, 1 unsaved")
        .await;
    // an edit in the hunk: the same changes, the hunk is unsaved too
    session.edit_buffer("a.txt", 2, "YY");
    session
        .wait_for_info("Uncommitted changes: 2 changes in 1 file, +2 -2, 2 unsaved")
        .await;
    // the files change too, while the buffer has unsaved edits
    write("new.txt", &lines("new ", 2));
    session
        .wait_for_info("Uncommitted changes: 3 changes in 2 files, +4 -2, 2 unsaved")
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hunk_action_message_stays_until_the_diff_is_closed() {
    let Some(_) = enter("a_hunk_action_message_stays_until_the_diff_is_closed") else {
        return;
    };
    one_hunk_in_a();
    write(
        "a.txt",
        &change(&change(&lines("line ", 20), 3, "three"), 15, "fifteen"),
    );

    let mut session = Session::open_uncommitted(1).await;
    session.open_diff(0).await;
    // (opening the diff leaves the totals of the list)
    session.keys("s").await;
    session.wait_for_footer("hunk 1/2 staged · file 1/1").await;
    assert_eq!(status(&session.app), "Staged the hunk");

    // the list sees the index change and a new file, and the message is still there
    write("new.txt", &lines("new ", 2));
    session.run_for(Duration::from_millis(1000)).await;
    assert_eq!(status(&session.app), "Staged the hunk");

    // closing the diff: the list says its totals, new file included
    session.keys("q").await;
    session
        .wait_for_info("Uncommitted changes: 3 changes in 2 files, +4 -2")
        .await;
    session
        .wait_for_list(&[
            "── a.txt  2 hunks  +2 -2  1 staged",
            "S   -+ a.txt:3",
            " M  -+ a.txt:15",
            "── new.txt (untracked)  1 hunk  +2",
            " M  ++ new.txt:1-2",
        ])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revert_message_stays_while_the_list_changes_and_the_totals_come_back_on_close() {
    let Some(_) =
        enter("a_revert_message_stays_while_the_list_changes_and_the_totals_come_back_on_close")
    else {
        return;
    };
    one_hunk_in_a();
    write(
        "a.txt",
        &change(&change(&lines("line ", 20), 3, "three"), 15, "fifteen"),
    );

    let mut session = Session::open_uncommitted(1).await;
    session.open_diff(0).await;
    session.keys("r").await;
    session.wait_for_footer("hunk 1/1 · file 1/1").await;
    let message = "Reverted the hunk and wrote a.txt";
    assert_eq!(status(&session.app), message);

    // the file changed (by the revert, and by another program): the list refreshes, the
    // message stays
    write("new.txt", &lines("new ", 2));
    session.run_for(Duration::from_millis(1000)).await;
    assert_eq!(status(&session.app), message);

    session.keys("q").await;
    session
        .wait_for_info("Uncommitted changes: 2 changes in 2 files, +3 -1")
        .await;
    session
        .wait_for_list(&[
            "── a.txt  1 hunk  +1 -1",
            " M  -+ a.txt:15",
            "── new.txt (untracked)  1 hunk  +2",
            " M  ++ new.txt:1-2",
        ])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_deleted_message_stays_after_the_list_refreshed() {
    let Some(_) = enter("the_deleted_message_stays_after_the_list_refreshed") else {
        return;
    };
    write("new.txt", &lines("new ", 3));
    write("other.txt", &lines("other ", 3));

    let mut session = Session::open_uncommitted(2).await;
    session.open_diff(0).await;
    session.keys("r").await;
    session.keys("y").await;
    assert_eq!(status(&session.app), "Deleted new.txt");
    // the list sees the file gone
    session.run_for(Duration::from_millis(1000)).await;
    assert_eq!(status(&session.app), "Deleted new.txt");
    assert!(!session.rows().iter().any(|row| row.contains("hunk 1/")));
    session
        .wait_for_list(&[
            "── other.txt (untracked)  1 hunk  +3",
            " M  ++ other.txt:1-3",
        ])
        .await;
    assert_eq!(status(&session.app), "Deleted new.txt");
}

// ---- opening commits and ranges in the background ----

const DIRS: usize = 40;
const PER_DIR: usize = 50;
const FILES: usize = DIRS * PER_DIR;
const SMALL_COMMITS: usize = 30;

fn big_file(n: usize, changed: bool) -> String {
    format!(
        "one {n}\n{}\nthree {n}\nfour {n}\n",
        if changed { "TWO" } else { "two" }
    )
}

fn write_big_files(work: &Path, changed: bool) {
    for dir in 0..DIRS {
        let path = work.join(format!("dir{dir:02}"));
        std::fs::create_dir_all(&path).unwrap();
        for file in 0..PER_DIR {
            let n = dir * PER_DIR + file;
            std::fs::write(path.join(format!("f{file:02}.txt")), big_file(n, changed)).unwrap();
        }
    }
}

/// `main`: "Add the files" (2,000 files), "Change all the files" (a line in each), and
/// `SMALL_COMMITS` commits "Small 1".. each adding a file.
fn make_big(parent: &Path) -> PathBuf {
    let work = parent.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, "", &["init", "-q"]);
    let commit = |message: &str| {
        git(&work, "", &["add", "-A"]);
        git(&work, "", &["commit", "-q", "-m", message]);
    };
    write_big_files(&work, false);
    commit("Add the files");
    write_big_files(&work, true);
    commit("Change all the files");
    for i in 1..=SMALL_COMMITS {
        std::fs::write(work.join(format!("s{i}.txt")), format!("small {i}\n")).unwrap();
        commit(&format!("Small {i}"));
    }
    work
}

const BIG: &str = "Change all the files";

fn enter_big(test: &str) -> Option<PathBuf> {
    Repo::enter_with(module_path!(), test, make_big)
}

impl Session {
    /// Opens the log, waits for all of it, and puts the cursor on `summary`.
    async fn open_big_log_at(summary: &str) -> Self {
        let mut session = Session::new();
        session.keys("<space>l").await;
        let count = format!("{}/{}", SMALL_COMMITS + 3, SMALL_COMMITS + 3);
        session
            .wait_for_screen("the log", |rows| on_screen(rows, &count))
            .await;
        // (the uncommitted changes are first)
        for _ in 0..40 {
            session.keys("j").await;
            if session.cursor() == summary {
                return session;
            }
        }
        panic!("no commit {summary}");
    }
}

fn big_totals() -> String {
    format!(
        "{}: {FILES} changes in {FILES} files, +{FILES} -{FILES} · {BIG}",
        id(BIG)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_big_commit_opens_in_the_background_and_lists_all_its_changes() {
    let Some(_) = enter_big("a_big_commit_opens_in_the_background_and_lists_all_its_changes")
    else {
        return;
    };
    let mut session = Session::open_big_log_at(BIG).await;
    // `j` right after Enter goes to the log, which has the keys while the changes are read (the
    // list can't have been pushed between the two keys)
    session.keys("<ret>j").await;
    session
        .wait_for_screen("the list", |rows| on_screen(rows, "dir00/f00.txt"))
        .await;
    session.assert_info(&big_totals()).await;
    assert_eq!(status(&session.app), "");
    let frame = session.frame();
    assert!(
        frame[0].contains(&format!("{FILES}/{FILES} ")),
        "{:?}",
        frame[0]
    );
    assert!(
        session.list()[0].starts_with("── dir00/f00.txt"),
        "{:?}",
        session.list()
    );
    // one list, and then the log
    session.keys("<esc>").await;
    assert!(session.log_is_on_top());
    assert_eq!(session.cursor(), "Add the files");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_range_opens_with_its_totals() {
    let Some(_) = enter_big("a_long_range_opens_with_its_totals") else {
        return;
    };
    let mut session = Session::open_big_log_at(&format!("Small {SMALL_COMMITS}")).await;
    session.keys("<A-v>").await;
    session.keys(&"<down>".repeat(SMALL_COMMITS)).await;
    assert_eq!(session.cursor(), BIG);
    let label = range(BIG, &format!("Small {SMALL_COMMITS}"), SMALL_COMMITS + 1);
    assert!(session.info().contains(&label), "{}", session.info());
    session.keys("<ret>").await;
    session
        .wait_for_screen("the list", |rows| on_screen(rows, "dir00/f00.txt"))
        .await;
    let total = FILES + SMALL_COMMITS;
    session
        .assert_info(&format!(
            "{label}: {total} changes in {total} files, +{total} -{FILES}"
        ))
        .await;
    // (the files of the small commits come after the directories)
    session.keys("<end>").await;
    assert!(
        on_screen(&session.rows(), &format!("s{SMALL_COMMITS}.txt")),
        "{:#?}",
        session.rows()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commit_asked_for_last_is_the_one_that_opens() {
    let Some(_) = enter_big("the_commit_asked_for_last_is_the_one_that_opens") else {
        return;
    };
    let mut session = Session::open_big_log_at(BIG).await;
    // the big commit, then the commit above it (a small one), in one go
    session.keys("<ret>k<ret>").await;
    session
        .wait_for_screen("the small commit", |rows| on_screen(rows, "s1.txt"))
        .await;
    let small = format!("{}: 1 change in 1 file, +1 · Small 1", id("Small 1"));
    session.assert_info(&small).await;
    // time for the big one to finish, if it were to
    session.run_for(Duration::from_millis(3000)).await;
    session.assert_info(&small).await;
    assert_eq!(
        session.list(),
        ["── s1.txt (new)  1 hunk  +1", "++ s1.txt:1"]
    );
    // one list: Esc goes to the log
    session.keys("<esc>").await;
    assert!(session.log_is_on_top(), "{:#?}", session.rows());
    assert_eq!(session.cursor(), "Small 1");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_uncommitted_row_cancels_a_commit_still_loading() {
    let Some(_) = enter_big("the_uncommitted_row_cancels_a_commit_still_loading") else {
        return;
    };
    write("new.txt", "new\n");
    let mut session = Session::open_big_log_at(BIG).await;
    session.keys("<ret><home><ret>").await;
    session.wait_for_uncommitted_list().await;
    session.run_for(Duration::from_millis(3000)).await;
    session
        .assert_info("Uncommitted changes: 1 change in 1 file, +1")
        .await;
    session.keys("<esc>").await;
    assert!(session.log_is_on_top(), "{:#?}", session.rows());
}

impl Session {
    async fn wait_for_uncommitted_list(&mut self) {
        self.wait_for_screen("the hunks", super::uncommitted::uncommitted_list_is_open)
            .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_log_drops_the_commit_still_loading() {
    let Some(_) = enter_big("closing_the_log_drops_the_commit_still_loading") else {
        return;
    };
    let mut session = Session::open_big_log_at(BIG).await;
    session.keys("<ret><esc>").await;
    assert!(!session.picker_is_open(), "{:#?}", session.rows());
    // long enough for the reading to be done, and for the message to be due
    session.run_for(Duration::from_millis(4000)).await;
    assert!(!session.picker_is_open(), "{:#?}", session.rows());
    // nothing says "loading", or the totals of the list that was dropped
    assert_eq!(status(&session.app), "");
}
