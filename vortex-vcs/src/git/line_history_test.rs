//! Tests of the line history walk on throwaway repositories made with the `git` command line.
#![allow(clippy::single_range_in_vec_init)]

use std::cell::Cell;
use std::ops::Range;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

use crate::line_history::LineEvent;

/// A repository with a bare "remote" and commits with increasing dates, so that the order of the
/// log is the order they were made in.
struct Repo {
    remote: TempDir,
    dir: TempDir,
    minute: Cell<u32>,
}

fn run_raw(dir: &Path, args: &[&str], minute: u32) -> Output {
    let date = format!("2000-01-01T00:{minute:02}:00+00:00");
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env("GIT_TERMINAL_PROMPT", "false")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_NAME", "author")
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_NAME", "committer")
        .env("GIT_CONFIG_COUNT", "3")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_1", "main")
        .env("GIT_CONFIG_KEY_2", "core.autocrlf")
        .env("GIT_CONFIG_VALUE_2", "false")
        .output()
        .expect("git can be run")
}

fn run(dir: &Path, args: &[&str], minute: u32) -> String {
    let output = run_raw(dir, args, minute);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

impl Repo {
    fn new() -> Self {
        let remote = tempfile::tempdir().unwrap();
        run(remote.path(), &["init", "-q", "--bare"], 0);
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"], 0);
        Self {
            remote,
            dir,
            minute: Cell::new(0),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) -> String {
        run(self.path(), args, self.minute.get())
    }

    fn connect(&self) {
        let remote = self.remote.path().to_str().unwrap().to_string();
        self.git(&["remote", "add", "origin", &remote]);
    }

    fn write(&self, file: &str, contents: &str) {
        let path = self.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn write_lines(&self, file: &str, lines: &[&str]) {
        let mut text = lines.join("\n");
        text.push('\n');
        self.write(file, &text);
    }

    /// Commits whatever is in the working tree, one minute after the last commit.
    fn commit(&self, message: &str) {
        self.minute.set(self.minute.get() + 1);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
    }

    /// Writes the lines of `file` and commits them.
    fn commit_lines(&self, message: &str, file: &str, lines: &[&str]) {
        self.write_lines(file, lines);
        self.commit(message);
    }

    /// Merges `branch` with a merge commit. When it conflicts, `resolved` is written to `file`.
    fn merge(&self, message: &str, branch: &str, resolved: Option<(&str, &[&str])>) {
        self.minute.set(self.minute.get() + 1);
        let output = run_raw(
            self.path(),
            &["merge", "-q", "--no-ff", "-m", message, branch],
            self.minute.get(),
        );
        match resolved {
            None => assert!(output.status.success(), "merge failed"),
            Some((file, lines)) => {
                assert!(!output.status.success(), "the merge should conflict");
                self.write_lines(file, lines);
                self.git(&["add", file]);
                self.git(&["commit", "-q", "--no-edit", "-m", message]);
            }
        }
    }

    fn abs(&self, file: &str) -> std::path::PathBuf {
        self.path().canonicalize().unwrap().join(file)
    }

    /// Walks the history of `ranges` of `file`.
    fn walk(&self, file: &str, ranges: Vec<Range<u32>>) -> Walk {
        let root = self.path().canonicalize().unwrap();
        let relative = |path: &Path| path.strip_prefix(&root).unwrap().display().to_string();
        let mut walk = Walk::default();
        super::for_each_line_commit(self.path(), true, &self.abs(file), ranges, |event| {
            match event {
                LineEvent::Searched(n) => walk.searched = n,
                LineEvent::Divider(label) => walk.events.push(Event::Divider(label.to_string())),
                LineEvent::Commit(commit) => walk.events.push(Event::Commit(Row {
                    summary: commit.info.summary.clone(),
                    added: commit.added,
                    lines: commit.lines.clone(),
                    path: relative(&commit.path),
                    old_path: commit.old_path.as_deref().map(relative),
                    hunks: commit
                        .hunks
                        .iter()
                        .map(|hunk| (hunk.before.clone(), hunk.after.clone()))
                        .collect(),
                })),
            }
            true
        })
        .unwrap();
        walk
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Row {
    summary: String,
    added: bool,
    lines: Vec<Range<u32>>,
    path: String,
    old_path: Option<String>,
    /// `(before, after)` of the hunks touching the lines
    hunks: Vec<(Range<u32>, Range<u32>)>,
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Divider(String),
    Commit(Row),
}

#[derive(Default)]
struct Walk {
    events: Vec<Event>,
    /// The last number of commits looked at
    searched: usize,
}

impl Walk {
    fn rows(&self) -> impl Iterator<Item = &Row> {
        self.events.iter().filter_map(|event| match event {
            Event::Commit(row) => Some(row),
            Event::Divider(_) => None,
        })
    }

    /// The summaries of the commits listed, in order.
    fn summaries(&self) -> Vec<&str> {
        self.rows().map(|row| row.summary.as_str()).collect()
    }

    fn row(&self, summary: &str) -> &Row {
        self.rows()
            .find(|row| row.summary == summary)
            .unwrap_or_else(|| panic!("{summary} isn't listed: {:?}", self.summaries()))
    }

    /// Dividers and commits in order, a divider as `"== label"`.
    fn outline(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| match event {
                Event::Divider(label) => format!("== {label}"),
                Event::Commit(row) => row.summary.clone(),
            })
            .collect()
    }
}

const FILE: &str = "src/tracked.rs";

#[test]
fn the_history_of_the_cursor_line() {
    let repo = Repo::new();
    let mut lines = ["alpha", "beta", "gamma", "delta", "epsilon"];
    repo.commit_lines("add file", FILE, &lines);
    lines[4] = "epsilon edited";
    repo.commit_lines("edit another line", FILE, &lines);
    lines[1] = "beta edited";
    repo.commit_lines("edit the line", FILE, &lines);

    let walk = repo.walk(FILE, vec![1..2]);
    assert_eq!(walk.summaries(), ["edit the line", "add file"]);
    let edit = walk.row("edit the line");
    assert!(!edit.added);
    assert_eq!(edit.lines, [1..2]);
    assert_eq!(edit.path, FILE);
    assert_eq!(edit.old_path, None);
    assert_eq!(edit.hunks, [(1..2, 1..2)]);
    let add = walk.row("add file");
    assert!(add.added);
    assert_eq!(add.lines, [1..2]);
    assert_eq!(add.hunks, [(0..0, 0..5)]);
}

#[test]
fn lines_moved_by_inserts_above_are_found_at_their_old_place() {
    let repo = Repo::new();
    let mut lines = vec!["one", "two", "three", "four", "five"];
    repo.commit_lines("add file", FILE, &lines);
    lines[2] = "three edited";
    repo.commit_lines("edit three", FILE, &lines);
    lines.splice(0..0, ["use a;", "use b;", "use c;"]);
    repo.commit_lines("insert above", FILE, &lines);

    // "three edited" is line 5 now
    let walk = repo.walk(FILE, vec![5..6]);
    assert_eq!(walk.summaries(), ["edit three", "add file"]);
    assert_eq!(walk.row("edit three").lines, [2..3]);
    assert_eq!(walk.row("add file").lines, [2..3]);
}

#[test]
fn a_split_line_is_followed_to_the_line_it_was() {
    let repo = Repo::new();
    repo.commit_lines("add file", FILE, &["a", "let x = f(1, 2);", "z"]);
    repo.commit_lines(
        "split the call",
        FILE,
        &["a", "let x = f(", "    1, 2", ");", "z"],
    );

    let walk = repo.walk(FILE, vec![1..4]);
    assert_eq!(walk.summaries(), ["split the call", "add file"]);
    assert!(!walk.row("split the call").added);
    assert_eq!(walk.row("split the call").lines, [1..4]);
    let add = walk.row("add file");
    assert!(add.added);
    assert_eq!(add.lines, [1..2]);

    // one of the three lines is enough
    let walk = repo.walk(FILE, vec![2..3]);
    assert_eq!(walk.summaries(), ["split the call", "add file"]);
}

#[test]
fn joined_lines_are_followed_to_both_lines_they_were() {
    let repo = Repo::new();
    repo.commit_lines("add first", FILE, &["a", "first", "z"]);
    repo.commit_lines("add second", FILE, &["a", "first", "second", "z"]);
    repo.commit_lines("join them", FILE, &["a", "first second", "z"]);

    let walk = repo.walk(FILE, vec![1..2]);
    assert_eq!(walk.summaries(), ["join them", "add second", "add first"]);
    assert_eq!(walk.row("join them").lines, [1..2]);
    assert_eq!(walk.row("add second").lines, [1..3]);
    assert!(walk.row("add first").added);
    // the lines at the commit that added the first one
    assert_eq!(walk.row("add first").lines, [1..2]);
}

/// Lines long enough for the rename to be detected as one when a line changes.
fn long_lines() -> Vec<String> {
    (0..8)
        .map(|i| format!("pub fn function_number_{i}() -> usize {{ {i} * 1000 + 17 }}"))
        .collect()
}

#[test]
fn history_goes_back_through_a_rename() {
    let repo = Repo::new();
    let mut lines = long_lines();
    let write = |message: &str, file: &str, lines: &[String]| {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        repo.commit_lines(message, file, &refs);
    };
    write("create old", "old.rs", &lines);
    lines[1] = "pub fn function_number_1() -> usize { 1 * 2000 + 17 }".to_string();
    write("edit in old", "old.rs", &lines);

    // renamed, with another line changed
    repo.git(&["mv", "old.rs", "new.rs"]);
    lines[6] = "pub fn function_number_6() -> usize { 6 * 3000 + 17 }".to_string();
    repo.write_lines(
        "new.rs",
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    repo.commit("rename");

    lines[1] = "pub fn function_number_1() -> usize { 1 * 4000 + 17 }".to_string();
    write("edit in new", "new.rs", &lines);

    let walk = repo.walk("new.rs", vec![1..2]);
    // the rename didn't change the line
    assert_eq!(
        walk.summaries(),
        ["edit in new", "edit in old", "create old"]
    );
    assert_eq!(walk.row("edit in new").path, "new.rs");
    assert_eq!(walk.row("edit in new").old_path, None);
    assert_eq!(walk.row("edit in old").path, "old.rs");
    assert_eq!(walk.row("edit in old").old_path, None);
    assert_eq!(walk.row("create old").path, "old.rs");
    assert!(walk.row("create old").added);
}

#[test]
fn a_rename_that_changed_the_lines_is_listed_with_the_old_path() {
    let repo = Repo::new();
    let mut lines = long_lines();
    let write = |message: &str, file: &str, lines: &[String]| {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        repo.commit_lines(message, file, &refs);
    };
    write("create old", "old.rs", &lines);
    repo.git(&["mv", "old.rs", "new.rs"]);
    lines[3] = "pub fn function_number_3() -> usize { 3 * 2000 + 17 }".to_string();
    repo.write_lines(
        "new.rs",
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    repo.commit("rename and edit");
    lines[6] = "pub fn function_number_6() -> usize { 6 * 3000 + 17 }".to_string();
    write("edit elsewhere", "new.rs", &lines);

    let walk = repo.walk("new.rs", vec![3..4]);
    assert_eq!(walk.summaries(), ["rename and edit", "create old"]);
    let rename = walk.row("rename and edit");
    assert_eq!(rename.path, "new.rs");
    assert_eq!(rename.old_path.as_deref(), Some("old.rs"));
    assert!(!rename.added);
    let create = walk.row("create old");
    assert_eq!(create.path, "old.rs");
    assert_eq!(create.old_path, None);
    assert!(create.added);
}

#[test]
fn the_walk_stops_at_the_commit_that_added_the_lines() {
    let repo = Repo::new();
    for i in 1..=4 {
        repo.write("other.txt", &format!("{i}\n"));
        repo.commit(&format!("other {i}"));
    }
    repo.commit_lines("add tracked", FILE, &["a", "b", "c"]);
    for i in 6..=10 {
        repo.write("other.txt", &format!("{i}\n"));
        repo.commit(&format!("other {i}"));
    }

    let walk = repo.walk(FILE, vec![0..3]);
    assert_eq!(walk.summaries(), ["add tracked"]);
    assert!(walk.row("add tracked").added);
    assert!(walk.searched <= 6, "searched {} commits", walk.searched);
}

#[test]
fn a_merge_that_took_the_line_from_one_side_is_not_listed() {
    let repo = Repo::new();
    repo.commit_lines("add file", FILE, &["a", "b", "c"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.commit_lines("branch edits b", FILE, &["a", "b edited", "c"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.commit_lines("main edits another file", "other.txt", &["x"]);
    repo.merge("merge feature", "feature", None);

    let walk = repo.walk(FILE, vec![1..2]);
    assert_eq!(walk.summaries(), ["branch edits b", "add file"]);
}

#[test]
fn a_merge_that_changed_the_line_from_both_sides_is_listed() {
    let repo = Repo::new();
    repo.commit_lines("add file", FILE, &["a", "b", "c"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.commit_lines("branch edits b", FILE, &["a", "b from branch", "c"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.commit_lines("main edits b", FILE, &["a", "b from main", "c"]);
    repo.merge(
        "merge feature",
        "feature",
        Some((FILE, &["a", "b resolved", "c"])),
    );

    let walk = repo.walk(FILE, vec![1..2]);
    assert_eq!(
        walk.summaries(),
        [
            "merge feature",
            "main edits b",
            "branch edits b",
            "add file"
        ]
    );
    assert!(!walk.row("merge feature").added);
    assert_eq!(walk.row("merge feature").lines, [1..2]);
    // the commit that added the lines is reached from both sides, and listed once
    assert!(walk.row("add file").added);
}

#[test]
fn several_ranges_are_followed_together() {
    let repo = Repo::new();
    let names: Vec<String> = (0..30).map(|i| format!("line {i:02}")).collect();
    let mut lines: Vec<String> = names.clone();
    let write = |message: &str, lines: &[String]| {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        repo.commit_lines(message, FILE, &refs);
    };
    write("add file", &lines);
    lines[2] = "line 02 edited".to_string();
    write("edit near the top", &lines);
    lines[24] = "line 24 edited".to_string();
    write("edit near the bottom", &lines);
    // above the bottom range, below the top one
    lines.insert(12, "an inserted line".to_string());
    write("insert in the middle", &lines);
    lines[2] = "line 02 edited twice".to_string();
    lines[25] = "line 24 edited twice".to_string();
    write("edit both", &lines);

    let walk = repo.walk(FILE, vec![25..26, 2..3]);
    assert_eq!(
        walk.summaries(),
        [
            "edit both",
            "edit near the bottom",
            "edit near the top",
            "add file"
        ]
    );
    assert_eq!(walk.row("edit both").lines, [2..3, 25..26]);
    // the insert in the middle moved the bottom range
    assert_eq!(walk.row("edit near the bottom").lines, [2..3, 24..25]);
    assert_eq!(walk.row("add file").lines, [2..3, 24..25]);
}

#[test]
fn the_walk_stops_when_asked_to() {
    let repo = Repo::new();
    let mut lines = ["a", "b", "c"];
    repo.commit_lines("add file", FILE, &lines);
    for (i, text) in ["one", "two", "three"].into_iter().enumerate() {
        lines[1] = text;
        repo.commit_lines(&format!("edit {i}"), FILE, &lines);
    }

    let mut events = Vec::new();
    super::for_each_line_commit(repo.path(), true, &repo.abs(FILE), vec![1..2], |event| {
        let (name, keep_going) = match event {
            LineEvent::Searched(n) => (format!("searched {n}"), n < 2),
            LineEvent::Divider(label) => (format!("divider {label}"), true),
            LineEvent::Commit(commit) => (commit.info.summary.clone(), true),
        };
        events.push(name);
        keep_going
    })
    .unwrap();
    assert_eq!(
        events,
        ["searched 1", "divider main", "edit 2", "searched 2"]
    );
}

#[test]
fn commits_are_listed_in_the_sections_of_the_log() {
    let repo = Repo::new();
    repo.commit_lines("add file", FILE, &["a", "b", "c"]);
    repo.connect();
    repo.commit_lines("main edit", FILE, &["a", "b main", "c"]);
    repo.git(&["push", "-q", "-u", "origin", "main"]);
    repo.git(&["remote", "set-head", "origin", "main"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.git(&["push", "-q", "-u", "origin", "feature"]);
    repo.commit_lines("unpushed one", FILE, &["a", "b one", "c"]);
    repo.commit_lines("unpushed two", FILE, &["a", "b two", "c"]);

    let walk = repo.walk(FILE, vec![1..2]);
    assert_eq!(
        walk.outline(),
        [
            "== feature (unpushed)",
            "unpushed two",
            "unpushed one",
            "== origin/main",
            "main edit",
            "add file",
        ]
    );
}

#[test]
fn a_deletion_inside_the_range_is_listed_and_one_above_it_is_not() {
    let repo = Repo::new();
    repo.commit_lines("add file", FILE, &["a", "b", "x", "c", "d"]);
    repo.commit_lines("delete x", FILE, &["a", "b", "c", "d"]);

    // `b` and `c` had `x` between them
    let walk = repo.walk(FILE, vec![1..3]);
    assert_eq!(walk.summaries(), ["delete x", "add file"]);
    assert_eq!(walk.row("delete x").lines, [1..3]);
    assert_eq!(walk.row("delete x").hunks, [(2..3, 2..2)]);
    assert_eq!(walk.row("add file").lines, [1..4]);

    // `c` alone: `x` was right above it
    let walk = repo.walk(FILE, vec![2..3]);
    assert_eq!(walk.summaries(), ["add file"]);
    assert_eq!(walk.row("add file").lines, [3..4]);
    // and `b` alone: right below it
    let walk = repo.walk(FILE, vec![1..2]);
    assert_eq!(walk.summaries(), ["add file"]);
}
