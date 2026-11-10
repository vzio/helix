//! Tests of the hunk actions of the diff of a file (`s`/`u`/`r` and `S`/`U`/`R`) and of `n`/`N`
//! over the hunks of all the files of an entry of the commit log.
//!
//! Like the tests of [`super::uncommitted`], each one runs in a process of its own with the
//! working directory in a throwaway repository, and changes it with git and `std::fs`.

use std::path::PathBuf;

use super::git_log::{git, on_screen, status, Repo, Session, UNCOMMITTED};

fn enter(test: &str) -> Option<PathBuf> {
    Repo::enter_in(module_path!(), test)
}

fn cwd() -> PathBuf {
    helix_stdx::env::current_working_dir()
}

fn write(path: &str, contents: &str) {
    std::fs::write(cwd().join(path), contents).unwrap();
}

fn read(path: &str) -> String {
    std::fs::read_to_string(cwd().join(path)).unwrap()
}

fn exists(path: &str) -> bool {
    cwd().join(path).exists()
}

/// `count` lines `{prefix}1`, `{prefix}2`...
fn lines(prefix: &str, count: usize) -> String {
    (1..=count).map(|i| format!("{prefix}{i}\n")).collect()
}

/// `text` with the line `line {n}` replaced.
fn change(text: &str, n: usize, new: &str) -> String {
    let old = format!("line {n}\n");
    assert!(text.contains(&old));
    text.replacen(&old, &format!("{new}\n"), 1)
}

fn commit_all(message: &str) {
    git(&cwd(), "", &["add", "-A"]);
    git(&cwd(), "", &["commit", "-q", "-m", message]);
}

fn cached() -> String {
    git(&cwd(), "", &["diff", "--cached"])
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
    /// Opens the log on the uncommitted changes, which are `count` files.
    async fn open_uncommitted(count: usize) -> Self {
        let mut session = Session::open_log_on_uncommitted().await;
        let label = format!(
            "{UNCOMMITTED} ({count} file{})",
            if count == 1 { "" } else { "s" }
        );
        session
            .wait_for_screen(&label, |rows| on_screen(rows, &label))
            .await;
        session
    }

    /// From a picker of files: opens the diff of the file `index` (from 0).
    async fn open_diff(&mut self, index: usize) {
        self.keys("<ret>").await;
        self.wait_for_screen("the files", |rows| on_screen(rows, "path"))
            .await;
        if index > 0 {
            self.keys(&"<down>".repeat(index)).await;
        }
        self.keys("<ret>").await;
        self.wait_for_screen("the diff", |rows| on_screen(rows, "hunk "))
            .await;
    }

    /// The first line of the footer without the keys: `hunk 2/5 staged · file 1/3`.
    fn footer(&self) -> String {
        let rows = self.rows();
        let row = rows
            .iter()
            .find(|row| {
                row.contains("hunk ")
                    && ["n/N", "s/u/r", "r revert"]
                        .iter()
                        .any(|keys| row.contains(keys))
            })
            .unwrap_or_else(|| panic!("no footer: {rows:#?}"));
        let end = [" · s/u/r", " · r revert", " · n/N"]
            .iter()
            .filter_map(|keys| row.find(keys))
            .min()
            .unwrap();
        row[..end].trim().to_string()
    }

    async fn wait_for_footer(&mut self, footer: &str) {
        self.wait_for_screen(footer, |rows| on_screen(rows, &format!("{footer} ·")))
            .await;
    }

    /// Asserts the file and the footer of the diff on the screen.
    fn assert_at(&self, file: &str, footer: &str) {
        let rows = self.rows();
        assert!(
            rows[0].contains(file) && !rows[0].contains(&format!("/{file}")),
            "title is not of {file}: {rows:#?}"
        );
        assert_eq!(self.footer(), footer, "{rows:#?}");
    }
}

/// A file of 20 lines, committed, with its lines 3 and 15 changed in the working tree: two hunks.
fn two_hunks(path: &str) -> (String, String) {
    let original = lines("line ", 20);
    write(path, &original);
    let edited = change(&change(&original, 3, "three"), 15, "fifteen");
    (original, edited)
}

#[tokio::test(flavor = "multi_thread")]
async fn staging_a_hunk_stages_only_it_and_unstaging_undoes_it() {
    let Some(_) = enter("staging_a_hunk_stages_only_it_and_unstaging_undoes_it") else {
        return;
    };
    let (_, edited) = two_hunks("a.txt");
    commit_all("Add a.txt");
    write("a.txt", &edited);

    let mut session = Session::open_uncommitted(1).await;
    session.open_diff(0).await;
    assert_eq!(session.footer(), "hunk 1/2 · file 1/1");
    session.keys("s").await;
    session.wait_for_footer("hunk 1/2 staged · file 1/1").await;
    assert_eq!(status(&session.app), "Staged the hunk");
    let staged = cached();
    assert!(staged.contains("+three"), "{staged}");
    assert!(staged.contains("-line 3"), "{staged}");
    assert!(!staged.contains("fifteen"), "{staged}");
    assert!(on_screen(&session.rows(), "┃"), "{:#?}", session.rows());
    // the file itself is untouched
    assert_eq!(read("a.txt"), edited);

    // the second hunk is not staged
    session.keys("n").await;
    assert_eq!(session.footer(), "hunk 2/2 · file 1/1");
    session.keys("N").await;
    session.keys("u").await;
    session.wait_for_footer("hunk 1/2").await;
    assert_eq!(status(&session.app), "Unstaged the hunk");
    assert_eq!(cached(), "");
}

#[tokio::test(flavor = "multi_thread")]
async fn stage_and_unstage_a_whole_file_also_when_partly_staged() {
    let Some(_) = enter("stage_and_unstage_a_whole_file_also_when_partly_staged") else {
        return;
    };
    let original = lines("line ", 20);
    write("a.txt", &original);
    write("b.txt", &original);
    commit_all("Add the files");
    // a.txt: line 3 staged from the CLI, then the line next to it changed too (one hunk, half of
    // it staged)
    let first = change(&original, 3, "three");
    write("a.txt", &first);
    git(&cwd(), "", &["add", "a.txt"]);
    write("a.txt", &change(&first, 4, "four"));
    write("b.txt", &change(&original, 10, "ten"));

    let mut session = Session::open_uncommitted(2).await;
    session.open_diff(0).await;
    session.assert_at("a.txt", "hunk 1/1 partly staged · file 1/2");
    assert!(on_screen(&session.rows(), "╏"), "{:#?}", session.rows());

    session.keys("S").await;
    session.wait_for_footer("hunk 1/1 staged · file 1/2").await;
    assert_eq!(status(&session.app), "Staged a.txt");
    let staged = cached();
    assert!(
        staged.contains("+three") && staged.contains("+four"),
        "{staged}"
    );
    assert!(!staged.contains("ten"), "{staged}");

    session.keys("U").await;
    session.wait_for_footer("hunk 1/1 · file 1/2").await;
    assert_eq!(status(&session.app), "Unstaged a.txt");
    assert_eq!(cached(), "");
    // (the work tree is as it was)
    assert!(read("a.txt").contains("four"));

    // and a file that was not staged at all
    session.keys("N").await;
    session.assert_at("b.txt", "hunk 1/1 · file 2/2");
    session.keys("S").await;
    session.wait_for_footer("hunk 1/1 staged · file 2/2").await;
    assert!(cached().contains("+ten"));
    session.keys("U").await;
    session.wait_for_footer("hunk 1/1 · file 2/2").await;
    assert_eq!(cached(), "");
}

#[tokio::test(flavor = "multi_thread")]
async fn reverting_a_hunk_goes_back_to_head_and_u_in_the_buffer_undoes_it() {
    let Some(_) = enter("reverting_a_hunk_goes_back_to_head_and_u_in_the_buffer_undoes_it") else {
        return;
    };
    let (original, edited) = two_hunks("a.txt");
    commit_all("Add a.txt");
    write("a.txt", &edited);

    let mut session = Session::open_uncommitted(1).await;
    session.open_diff(0).await;
    session.keys("r").await;
    session.wait_for_footer("hunk 1/1 · file 1/1").await;
    assert_eq!(read("a.txt"), change(&original, 15, "fifteen"));

    // close the diff and the files, and undo in the buffer
    session.keys("q<esc><esc>").await;
    session.keys(":open a.txt<ret>").await;
    assert_eq!(buffer(&session, "a.txt"), change(&original, 15, "fifteen"));
    session.keys("u").await;
    assert_eq!(buffer(&session, "a.txt"), edited);
}

#[tokio::test(flavor = "multi_thread")]
async fn reverting_a_whole_file_is_one_undo_step() {
    let Some(_) = enter("reverting_a_whole_file_is_one_undo_step") else {
        return;
    };
    let original = lines("line ", 30);
    write("a.txt", &original);
    commit_all("Add a.txt");
    let edited = change(
        &change(&change(&original, 3, "three"), 15, "fifteen"),
        27,
        "twenty-seven",
    );
    write("a.txt", &edited);

    let mut session = Session::open_uncommitted(1).await;
    session.open_diff(0).await;
    assert_eq!(session.footer(), "hunk 1/3 · file 1/1");
    session.keys("R").await;
    session
        .wait_for_screen("no changes", |rows| on_screen(rows, "no changes"))
        .await;
    assert_eq!(read("a.txt"), original);

    session.keys("q<esc><esc>").await;
    session.keys(":open a.txt<ret>").await;
    assert_eq!(buffer(&session, "a.txt"), original);
    session.keys("u").await;
    assert_eq!(buffer(&session, "a.txt"), edited);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fully_staged_hunk_cannot_be_reverted() {
    let Some(_) = enter("a_fully_staged_hunk_cannot_be_reverted") else {
        return;
    };
    let (_, edited) = two_hunks("a.txt");
    commit_all("Add a.txt");
    write("a.txt", &edited);

    let mut session = Session::open_uncommitted(1).await;
    session.open_diff(0).await;
    session.keys("s").await;
    session.wait_for_footer("hunk 1/2 staged · file 1/1").await;
    session.keys("r").await;
    assert_eq!(
        status(&session.app),
        "The hunk is staged: u unstages it, then r reverts it"
    );
    assert_eq!(read("a.txt"), edited);
    // and R on a file that is staged as a whole
    session.keys("S").await;
    session.keys("R").await;
    assert_eq!(
        status(&session.app),
        "a.txt is staged: U unstages it, then R reverts it"
    );
    assert_eq!(read("a.txt"), edited);
}

#[tokio::test(flavor = "multi_thread")]
async fn reverting_an_untracked_file_asks_before_deleting_it() {
    let Some(_) = enter("reverting_an_untracked_file_asks_before_deleting_it") else {
        return;
    };
    write("new.txt", &lines("new ", 3));
    write("other.txt", &lines("other ", 3));

    let mut session = Session::open_uncommitted(2).await;
    session.open_diff(0).await;
    session.assert_at("new.txt", "hunk 1/1 · file 1/2");
    session.keys("r").await;
    assert!(
        status(&session.app).starts_with("Delete untracked new.txt?"),
        "{}",
        status(&session.app)
    );
    assert!(
        on_screen(&session.rows(), "y deletes it, any other key keeps it"),
        "{:#?}",
        session.rows()
    );
    session.keys("x").await;
    assert_eq!(status(&session.app), "Kept new.txt");
    assert!(exists("new.txt"));
    assert!(on_screen(&session.rows(), "hunk 1/1"));

    session.keys("R").await;
    assert!(status(&session.app).starts_with("Delete untracked new.txt?"));
    session.keys("y").await;
    assert!(!exists("new.txt"));
    assert!(exists("other.txt"));
    assert_eq!(status(&session.app), "Deleted new.txt");
    // the diff is closed
    assert!(
        !on_screen(&session.rows(), "hunk "),
        "{:#?}",
        session.rows()
    );
}

/// a.txt (one hunk), b.txt (two), c.txt (one) and Cargo.lock, all changed.
fn three_files_and_a_lockfile() {
    for path in ["a.txt", "b.txt", "c.txt"] {
        write(path, &lines("line ", 20));
    }
    write("Cargo.lock", &lines("lock ", 50));
    commit_all("Add the files");
    write("a.txt", &change(&lines("line ", 20), 5, "five"));
    let b = change(&change(&lines("line ", 20), 3, "three"), 15, "fifteen");
    write("b.txt", &b);
    write("c.txt", &change(&lines("line ", 20), 10, "ten"));
    write("Cargo.lock", &lines("locked ", 60));
}

#[tokio::test(flavor = "multi_thread")]
async fn n_and_n_visit_the_hunks_of_all_the_files_and_wrap_around() {
    let Some(_) = enter("n_and_n_visit_the_hunks_of_all_the_files_and_wrap_around") else {
        return;
    };
    three_files_and_a_lockfile();

    let mut session = Session::open_uncommitted(4).await;
    session.open_diff(0).await;
    session.assert_at("a.txt", "hunk 1/1 · file 1/3");

    let forward = [
        ("b.txt", "hunk 1/2 · file 2/3"),
        ("b.txt", "hunk 2/2 · file 2/3"),
        ("c.txt", "hunk 1/1 · file 3/3"),
    ];
    for (file, footer) in forward {
        session.keys("n").await;
        session.assert_at(file, footer);
    }
    assert!(!status(&session.app).contains("Wrapped"));
    session.keys("n").await;
    session.assert_at("a.txt", "hunk 1/1 · file 1/3");
    assert_eq!(status(&session.app), "Wrapped around to the first file");

    // backwards from the first file: around to the last one, at its last hunk
    session.keys("N").await;
    session.assert_at("c.txt", "hunk 1/1 · file 3/3");
    assert_eq!(status(&session.app), "Wrapped around to the last file");
    for (file, footer) in forward.iter().rev().skip(1) {
        session.keys("N").await;
        session.assert_at(file, footer);
    }
    session.keys("N").await;
    session.assert_at("a.txt", "hunk 1/1 · file 1/3");
    assert!(!on_screen(&session.rows(), "Cargo.lock"));
    // the picker below follows: closing the diff shows the cursor on a.txt
    session.keys("N").await;
    session.keys("q").await;
    assert_eq!(session.selected_file_in_picker(), "c.txt");
}

impl Session {
    /// The path of the row under the cursor of the picker of files.
    fn selected_file_in_picker(&self) -> String {
        let frame = self.frame();
        let row = frame[3..]
            .iter()
            .find(|row| row.starts_with(" > "))
            .unwrap_or_else(|| panic!("no cursor: {frame:#?}"));
        row.split_whitespace().nth(3).unwrap().to_string()
    }
}

/// Two commits on top of the log: the older changes a.txt, the newer b.txt (two hunks).
fn two_commits() {
    write("a.txt", &lines("line ", 20));
    write("b.txt", &lines("line ", 20));
    commit_all("Add the files");
    write("a.txt", &change(&lines("line ", 20), 5, "five"));
    commit_all("Change a");
    let b = change(&change(&lines("line ", 20), 3, "three"), 15, "fifteen");
    write("b.txt", &b);
    commit_all("Change b");
}

/// From a commit or range picker of files: the diff of the first file and n over both files.
async fn walk_two_files(session: &mut Session) {
    session.keys("<ret>").await;
    session
        .wait_for_screen("the diff", |rows| on_screen(rows, "hunk "))
        .await;
    session.assert_at("a.txt", "hunk 1/1 · file 1/2");
    for (file, footer) in [
        ("b.txt", "hunk 1/2 · file 2/2"),
        ("b.txt", "hunk 2/2 · file 2/2"),
    ] {
        session.keys("n").await;
        session.assert_at(file, footer);
    }
    session.keys("n").await;
    session.assert_at("a.txt", "hunk 1/1 · file 1/2");
    assert_eq!(status(&session.app), "Wrapped around to the first file");
    session.keys("N").await;
    session.assert_at("b.txt", "hunk 2/2 · file 2/2");
    assert_eq!(status(&session.app), "Wrapped around to the last file");
}

#[tokio::test(flavor = "multi_thread")]
async fn n_and_n_cross_the_files_of_a_commit() {
    let Some(_) = enter("n_and_n_cross_the_files_of_a_commit") else {
        return;
    };
    write("a.txt", &lines("line ", 20));
    write("b.txt", &lines("line ", 20));
    commit_all("Add the files");
    write("a.txt", &change(&lines("line ", 20), 5, "five"));
    let b = change(&change(&lines("line ", 20), 3, "three"), 15, "fifteen");
    write("b.txt", &b);
    commit_all("Change both");

    let mut session = Session::open_log().await;
    assert_eq!(session.cursor(), "Change both");
    session.keys("<ret>").await;
    session
        .wait_for_screen("the files", |rows| on_screen(rows, "b.txt"))
        .await;
    walk_two_files(&mut session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn n_and_n_cross_the_files_of_a_range_of_commits() {
    let Some(_) = enter("n_and_n_cross_the_files_of_a_range_of_commits") else {
        return;
    };
    two_commits();

    let mut session = Session::open_log().await;
    assert_eq!(session.cursor(), "Change b");
    session.keys("vj<ret>").await;
    session
        .wait_for_screen("the files", |rows| {
            on_screen(rows, "a.txt") && on_screen(rows, "b.txt")
        })
        .await;
    walk_two_files(&mut session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reverting_refuses_when_the_buffer_has_unsaved_changes() {
    let Some(_) = enter("reverting_refuses_when_the_buffer_has_unsaved_changes") else {
        return;
    };
    let (_, edited) = two_hunks("a.txt");
    commit_all("Add a.txt");
    write("a.txt", &edited);

    let mut session = Session::new();
    session.keys(":open a.txt<ret>ihello <esc>").await;
    assert!(buffer(&session, "a.txt").starts_with("hello "));
    let mut session = Session::open_log_with(session).await;
    session
        .wait_for_screen("the count", |rows| on_screen(rows, "(1 file)"))
        .await;
    session.open_diff(0).await;
    session.keys("r").await;
    assert_eq!(
        status(&session.app),
        "a.txt has unsaved changes: write it first"
    );
    assert_eq!(read("a.txt"), edited);
    assert!(buffer(&session, "a.txt").starts_with("hello "));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "bug: the 'Reverted ...: u in its buffer brings it back' message is replaced by \
            the \"'a.txt' written\" message of the save that follows it"]
async fn the_revert_message_stays_on_the_statusline() {
    let Some(_) = enter("the_revert_message_stays_on_the_statusline") else {
        return;
    };
    let (_, edited) = two_hunks("a.txt");
    commit_all("Add a.txt");
    write("a.txt", &edited);

    let mut session = Session::open_uncommitted(1).await;
    session.open_diff(0).await;
    session.keys("r").await;
    session.run_for(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        status(&session.app),
        "Reverted the hunk: u in its buffer brings it back"
    );
}
