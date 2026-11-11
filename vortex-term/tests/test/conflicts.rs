//! Tests of git merge conflicts as diagnostics (severity `Conflict`, source `git`).
//!
//! The conflicts are made by git itself (`git merge`, `git rebase`) in a throwaway repository.
//! The working directory is global to a process, so, like the tests of [`super::git_log`], each
//! test runs itself again in a process of its own with the working directory in the repository
//! made for it: see [`enter`].

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use vortex_core::diagnostic::{DiagnosticProvider, Severity};
use vortex_term::application::Application;

use super::git_log::{git, on_screen, Session};

const IN_REPO: &str = "VORTEX_CONFLICTS_TEST_IN_REPO";

/// The text of `a.txt` and `b.txt` on the branches: `l1`...`l8`, with `l2` and `l7` changed.
fn text(change: &str) -> String {
    (1..=8)
        .map(|n| match n {
            2 | 7 if !change.is_empty() => format!("{change}{n}\n"),
            _ => format!("l{n}\n"),
        })
        .collect()
}

/// Text that looks like conflict markers, committed in `clean.txt` and never in conflict.
const MARKER_LIKE: &str = "docs:\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> feature-x\n";

/// Makes the repository in `parent`, on `main`, with the branch `feature-x` that changes lines 2
/// and 7 of `a.txt` and `b.txt` differently from `main`: merging it, or rebasing it, conflicts
/// twice in each of the two files. `clean.txt` has marker-like text and never changes.
/// Nothing is merged yet.
fn make(parent: &Path, style: Option<&str>) -> PathBuf {
    let work = parent.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, "", &["init", "-q"]);
    git(&work, "", &["config", "user.name", "Jane Doe"]);
    git(&work, "", &["config", "user.email", "jane@example.com"]);
    if let Some(style) = style {
        git(&work, "", &["config", "merge.conflictStyle", style]);
    }
    let write = |file: &str, contents: &str| std::fs::write(work.join(file), contents).unwrap();
    let commit_all = |message: &str| {
        git(&work, "", &["add", "-A"]);
        git(&work, "", &["commit", "-q", "-m", message]);
    };
    write("a.txt", &text(""));
    write("b.txt", &text(""));
    write("clean.txt", MARKER_LIKE);
    commit_all("Initial commit");
    git(&work, "", &["checkout", "-q", "-b", "feature-x"]);
    write("a.txt", &text("feature"));
    write("b.txt", &text("feature"));
    commit_all("Change lines on the feature");
    git(&work, "", &["checkout", "-q", "main"]);
    write("a.txt", &text("main"));
    write("b.txt", &text("main"));
    commit_all("Change lines on main");
    work
}

/// In the process that runs the test: the directory of the repository. In the process of `cargo
/// test`: makes the repository, runs the test again in it, checks that it passes and returns
/// `None`. `test` is the name of the calling function.
fn enter(test: &str, style: Option<&str>) -> Option<PathBuf> {
    if std::env::var_os(IN_REPO).is_some() {
        return Some(cwd());
    }
    let dir = tempfile::tempdir().unwrap();
    // (not through a symlinked temp dir: paths are compared)
    let work = make(&std::fs::canonicalize(dir.path()).unwrap(), style);
    let name = format!("{}::{test}", module_path!().split_once("::").unwrap().1);
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
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{name} didn't run:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    None
}

fn cwd() -> PathBuf {
    vortex_stdx::env::current_working_dir()
}

/// Runs git, which is expected to fail (a merge or rebase that stops on a conflict).
fn git_conflicts(args: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_EDITOR", "true")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("git can be run");
    assert!(
        !output.status.success(),
        "git {args:?} should stop on a conflict: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// `git merge feature-x`, which conflicts.
fn merge() {
    git_conflicts(&["merge", "feature-x"]);
}

fn read(file: &str) -> String {
    std::fs::read_to_string(cwd().join(file)).unwrap()
}

/// What the tests look at in a conflict diagnostic.
#[derive(Debug, PartialEq, Eq)]
struct Conflict {
    /// The first and last line of the block (0-based).
    lines: (usize, usize),
    message: String,
}

fn conflict(start: usize, end: usize, message: &str) -> Conflict {
    Conflict {
        lines: (start, end),
        message: message.to_string(),
    }
}

/// The conflict diagnostics of the document of `file`, in order. Checks the fields all of them
/// have, including that the range is from the start of the first line to the end of the last
/// without its line ending.
fn conflicts_of(app: &Application, file: &str) -> Vec<Conflict> {
    let path = cwd().join(file);
    let doc = app
        .editor
        .document_by_path(&path)
        .unwrap_or_else(|| panic!("{file} is not open"));
    let text = doc.text().slice(..);
    doc.diagnostics()
        .iter()
        .filter(|diag| diag.provider == DiagnosticProvider::Conflict)
        .map(|diag| {
            assert_eq!(diag.severity, Some(Severity::Conflict));
            assert_eq!(diag.source.as_deref(), Some("git"));
            let start = text.char_to_line(diag.range.start);
            let end = text.char_to_line(diag.range.end);
            assert_eq!(diag.line, start);
            assert_eq!(diag.range.start, text.line_to_char(start));
            assert_eq!(
                diag.range.end,
                vortex_core::line_ending::line_end_char_index(&text, end)
            );
            Conflict {
                lines: (start, end),
                message: diag.message.clone(),
            }
        })
        .collect()
}

/// The `(first, last)` lines of the blocks of `file` on disk, by its marker lines.
fn blocks_on_disk(file: &str) -> Vec<(usize, usize)> {
    let text = read(file);
    let mut blocks = Vec::new();
    let mut start = 0;
    for (n, line) in text.lines().enumerate() {
        if line.starts_with("<<<<<<<") {
            start = n;
        } else if line.starts_with(">>>>>>>") {
            blocks.push((start, n));
        }
    }
    blocks
}

fn labels(file: &str) -> (String, String) {
    let text = read(file);
    let label = |marker: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(marker))
            .unwrap()
            .trim()
            .to_string()
    };
    (label("<<<<<<<"), label(">>>>>>>"))
}

impl Session {
    /// Runs until `condition` is true of the application.
    async fn wait_until(&mut self, what: &str, condition: impl Fn(&Application) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition(&self.app) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}:\n{:#?}",
                self.rows()
            );
            self.run_for(Duration::from_millis(20)).await;
        }
    }

    /// Opens `file` and waits for the diagnostics of its `count` blocks.
    async fn open_conflicted(&mut self, file: &str, count: usize) {
        self.keys(&format!(":o {file}<ret>")).await;
        self.wait_until("the conflicts", |app| {
            conflicts_of(app, file).len() == count
        })
        .await;
    }

    /// The selected lines of the current document, (first, last) 0-based, and its file name.
    fn selected(&self) -> (String, (usize, usize)) {
        let (view, doc) = vortex_view::current_ref!(self.app.editor);
        let text = doc.text().slice(..);
        let range = doc.selection(view.id).primary();
        let name = doc.path().unwrap().file_name().unwrap();
        (
            name.to_string_lossy().to_string(),
            (
                text.char_to_line(range.from()),
                text.char_to_line(range.to().saturating_sub(1).max(range.from())),
            ),
        )
    }
}

fn git_at_least(major: u32, minor: u32) -> bool {
    let version = git(&cwd(), "", &["--version"]);
    let mut numbers = version
        .trim()
        .trim_start_matches("git version ")
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0)) >= (major, minor)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_merge_conflict_is_a_diagnostic_on_each_block() {
    let Some(_) = enter("a_merge_conflict_is_a_diagnostic_on_each_block", None) else {
        return;
    };
    merge();
    assert_eq!(blocks_on_disk("a.txt"), [(1, 5), (10, 14)]);
    let mut session = Session::new();
    session.open_conflicted("a.txt", 2).await;

    let message = "Conflict: HEAD vs feature-x";
    assert_eq!(
        conflicts_of(&session.app, "a.txt"),
        [conflict(1, 5, message), conflict(10, 14, message)]
    );
    // the file with marker-like text is committed and clean: nothing
    session.open_conflicted("clean.txt", 0).await;
    session.run_for(Duration::from_millis(200)).await;
    assert!(conflicts_of(&session.app, "clean.txt").is_empty());
    assert!(session
        .app
        .editor
        .document_by_path(cwd().join("clean.txt"))
        .unwrap()
        .diagnostics()
        .is_empty());
    // and a.txt is still flagged
    assert_eq!(conflicts_of(&session.app, "a.txt").len(), 2);
}

async fn diff3_style(style: &str, test: &str) {
    let Some(_) = enter(test, Some(style)) else {
        return;
    };
    merge();
    // the base section is in the blocks, which are two lines longer
    assert!(read("a.txt").contains("\n|||||||"));
    assert_eq!(blocks_on_disk("a.txt"), [(1, 7), (12, 18)]);
    let mut session = Session::new();
    session.open_conflicted("a.txt", 2).await;
    let message = "Conflict: HEAD vs feature-x";
    assert_eq!(
        conflicts_of(&session.app, "a.txt"),
        [conflict(1, 7, message), conflict(12, 18, message)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diff3_block_includes_the_base() {
    diff3_style("diff3", "a_diff3_block_includes_the_base").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zdiff3_block_includes_the_base() {
    // zdiff3 is from git 2.35 (the check is in the child, where the repository is)
    if std::env::var_os(IN_REPO).is_none() {
        let version = Command::new("git").arg("--version").output().unwrap();
        let version = String::from_utf8_lossy(&version.stdout).to_string();
        let mut numbers = version
            .trim()
            .trim_start_matches("git version ")
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0));
        if (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0)) < (2, 35) {
            eprintln!("git is too old for zdiff3: {version}");
            return;
        }
    } else {
        assert!(git_at_least(2, 35));
    }
    diff3_style("zdiff3", "a_zdiff3_block_includes_the_base").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebase_conflict_has_the_labels_git_writes() {
    let Some(_) = enter("a_rebase_conflict_has_the_labels_git_writes", None) else {
        return;
    };
    git(&cwd(), "", &["checkout", "-q", "feature-x"]);
    git_conflicts(&["rebase", "main"]);
    let (ours, theirs) = labels("a.txt");
    // the commit being replayed, as `<short hash> (<subject>)`
    assert_eq!(ours, "HEAD");
    assert!(
        theirs.ends_with("(Change lines on the feature)") && !theirs.starts_with("feature"),
        "{theirs:?}"
    );
    let mut session = Session::new();
    session.open_conflicted("a.txt", 2).await;
    let message = format!("Conflict: {ours} vs {theirs}");
    assert_eq!(
        conflicts_of(&session.app, "a.txt"),
        [conflict(1, 5, &message), conflict(10, 14, &message)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn next_and_previous_diagnostic_visit_the_blocks() {
    let Some(_) = enter("next_and_previous_diagnostic_visit_the_blocks", None) else {
        return;
    };
    merge();
    let mut session = Session::new();
    session.open_conflicted("a.txt", 2).await;
    session.keys("gg").await;

    session.keys("]d").await;
    assert_eq!(session.selected(), ("a.txt".into(), (1, 5)));
    session.keys("]d").await;
    assert_eq!(session.selected(), ("a.txt".into(), (10, 14)));
    // no more: stays
    session.keys("]d").await;
    assert_eq!(session.selected(), ("a.txt".into(), (10, 14)));
    // back: the cursor is in the last block, so the previous start is its own, then the first
    session.keys("[d").await;
    session.keys("[d").await;
    assert_eq!(session.selected(), ("a.txt".into(), (1, 5)));
    session.keys("[d").await;
    assert_eq!(session.selected(), ("a.txt".into(), (1, 5)));
}

/// The text of the rows of the picker on the screen, with the cursor mark cut off.
fn picker_rows(session: &Session) -> Vec<String> {
    session
        .frame()
        .iter()
        .skip(3)
        .map(|row| row.split("││").next().unwrap().trim_start_matches(" > "))
        .map(|row| row.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|row| !row.is_empty())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_workspace_picker_lists_the_conflicts_of_files_that_are_not_open() {
    let Some(_) = enter(
        "the_workspace_picker_lists_the_conflicts_of_files_that_are_not_open",
        None,
    ) else {
        return;
    };
    merge();
    let mut session = Session::new();
    // only the clean file is open
    session.keys(":o clean.txt<ret>").await;
    session.keys("<space>D").await;
    session
        .wait_for_screen("the picker", |rows| on_screen(rows, "CONFLICT"))
        .await;
    let rows = picker_rows(&session);
    let message = "Conflict: HEAD vs feature-x";
    let row = |file: &str| format!("CONFLICT git {file} {message}");
    assert_eq!(
        rows[..4],
        [row("a.txt"), row("a.txt"), row("b.txt"), row("b.txt")],
        "{rows:#?}"
    );
    assert!(
        session.frame()[0].contains("4/4"),
        "{:?}",
        session.frame()[0]
    );

    // Enter opens the file at the block of the row under the cursor
    session.keys("<ret>").await;
    assert_eq!(session.selected(), ("a.txt".into(), (1, 5)));

    // the second row is the second block
    session.keys("<space>D<down><ret>").await;
    assert_eq!(session.selected(), ("a.txt".into(), (10, 14)));
    // and the third the first block of the other file
    session.keys("<space>D<down><down><ret>").await;
    assert_eq!(session.selected(), ("b.txt".into(), (1, 5)));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_document_picker_lists_the_conflicts_of_the_document() {
    let Some(_) = enter(
        "the_document_picker_lists_the_conflicts_of_the_document",
        None,
    ) else {
        return;
    };
    merge();
    let mut session = Session::new();
    session.open_conflicted("a.txt", 2).await;
    session.keys("<space>d").await;
    let rows = picker_rows(&session);
    let message = "Conflict: HEAD vs feature-x";
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(rows
        .iter()
        .all(|row| row == &format!("CONFLICT git {message}")));
    session.keys("<down><ret>").await;
    assert_eq!(session.selected(), ("a.txt".into(), (10, 14)));
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_marker_clears_the_block_at_once_and_undo_brings_it_back() {
    let Some(_) = enter(
        "removing_a_marker_clears_the_block_at_once_and_undo_brings_it_back",
        None,
    ) else {
        return;
    };
    merge();
    let mut session = Session::new();
    session.open_conflicted("a.txt", 2).await;
    let message = "Conflict: HEAD vs feature-x";

    // no waiting: the diagnostic is gone when the key has been handled
    session.keys("gg").await;
    session.keys("jxd").await;
    assert_eq!(
        conflicts_of(&session.app, "a.txt"),
        [conflict(9, 13, message)]
    );
    session.keys("u").await;
    assert_eq!(
        conflicts_of(&session.app, "a.txt"),
        [conflict(1, 5, message), conflict(10, 14, message)]
    );
    // the closing marker of the second block too
    session.keys("15Gxd").await;
    assert_eq!(
        conflicts_of(&session.app, "a.txt"),
        [conflict(1, 5, message)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn staging_the_file_clears_the_conflicts_after_a_refresh() {
    let Some(_) = enter(
        "staging_the_file_clears_the_conflicts_after_a_refresh",
        None,
    ) else {
        return;
    };
    merge();
    let mut session = Session::new();
    session.open_conflicted("a.txt", 2).await;
    session.open_conflicted("b.txt", 2).await;

    // resolved in git's eyes, the markers are still in the text
    git(&cwd(), "", &["add", "a.txt"]);
    assert!(read("a.txt").contains("<<<<<<<"));
    // (leaving a buffer refreshes)
    session.keys(":buffer-previous<ret>").await;
    session
        .wait_until("a.txt to be resolved", |app| {
            conflicts_of(app, "a.txt").is_empty()
        })
        .await;
    // b.txt is still unmerged
    assert_eq!(conflicts_of(&session.app, "b.txt").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_becomes_conflicted_after_it_was_opened_is_flagged_after_a_refresh() {
    let Some(_) = enter(
        "a_file_that_becomes_conflicted_after_it_was_opened_is_flagged_after_a_refresh",
        None,
    ) else {
        return;
    };
    let mut session = Session::new();
    session.open_conflicted("a.txt", 0).await;
    session.open_conflicted("clean.txt", 0).await;
    session.run_for(Duration::from_millis(200)).await;
    assert!(conflicts_of(&session.app, "a.txt").is_empty());

    // the merge happens outside of the editor, which reloads the file when it is told to
    merge();
    session.keys(":reload-all<ret>").await;
    session.keys(":buffer-previous<ret>").await;
    session
        .wait_until("the conflicts", |app| conflicts_of(app, "a.txt").len() == 2)
        .await;
    assert_eq!(
        conflicts_of(&session.app, "a.txt"),
        [
            conflict(1, 5, "Conflict: HEAD vs feature-x"),
            conflict(10, 14, "Conflict: HEAD vs feature-x")
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_workspace_count_is_live_for_open_files_and_on_disk_for_the_others() {
    let Some(_) = enter(
        "the_workspace_count_is_live_for_open_files_and_on_disk_for_the_others",
        None,
    ) else {
        return;
    };
    merge();
    let mut session = Session::new();
    assert_eq!(session.app.editor.workspace_conflict_count(), 0);
    // the unmerged files are read when a document is opened: a.txt and b.txt, 2 blocks each
    session.keys(":o clean.txt<ret>").await;
    session
        .wait_until("the unmerged files", |app| {
            app.editor.workspace_conflict_count() == 4
        })
        .await;

    // opening one doesn't change the count, editing it does
    session.open_conflicted("a.txt", 2).await;
    assert_eq!(session.app.editor.workspace_conflict_count(), 4);
    session.keys("gg").await;
    session.keys("jxd").await;
    assert_eq!(session.app.editor.workspace_conflict_count(), 3);
    session.keys("u").await;
    assert_eq!(session.app.editor.workspace_conflict_count(), 4);
}
