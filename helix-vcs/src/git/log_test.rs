//! Tests of the sections of the commit log and of the changes of ranges of commits, on
//! throwaway repositories made with the `git` command line.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::{git, FileChange, LogEntry};

/// A repository with a bare "remote" and commits with increasing dates, so that the order of the
/// log is the order they were made in.
struct Repo {
    _remote: TempDir,
    dir: TempDir,
    minute: Cell<u32>,
}

fn run(dir: &Path, args: &[&str], minute: u32) -> String {
    let date = format!("2000-01-01T00:{minute:02}:00+00:00");
    let output = Command::new("git")
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
        .expect("git can be run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

impl Repo {
    /// A repository with the bare repository `origin` as its remote; nothing is committed.
    fn new() -> Self {
        let remote = tempfile::tempdir().unwrap();
        run(remote.path(), &["init", "-q", "--bare"], 0);
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"], 0);
        Self {
            _remote: remote,
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
        let remote = self._remote.path().to_str().unwrap().to_string();
        self.git(&["remote", "add", "origin", &remote]);
    }

    fn write(&self, file: &str, contents: &str) {
        let path = self.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Commits whatever is in the working tree, one minute after the last commit.
    fn commit(&self, message: &str) -> String {
        self.minute.set(self.minute.get() + 1);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.id("HEAD")
    }

    /// Commits a change to a file named after the message.
    fn commit_file(&self, message: &str) -> String {
        self.write(&format!("{message}.txt"), &format!("{message}\n"));
        self.commit(message)
    }

    fn id(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev]).trim().to_string()
    }

    fn push(&self, branch: &str) {
        self.git(&["push", "-q", "-u", "origin", branch]);
    }

    /// The log, as the label of each section and the summaries of its commits. The log is the same
    /// whether the repository is trusted or not.
    fn log(&self) -> Vec<(String, Vec<String>)> {
        let log = self.log_with_trust(true);
        assert_eq!(log, self.log_with_trust(false), "log of an untrusted repo");
        log
    }

    fn log_with_trust(&self, trust_full: bool) -> Vec<(String, Vec<String>)> {
        let sections = std::sync::Mutex::new(Vec::<(String, Vec<String>)>::new());
        git::for_each_log_entry(self.path(), trust_full, |entry| {
            let mut sections = sections.lock().unwrap();
            match entry.unwrap() {
                LogEntry::Divider(label) => sections.push((label, Vec::new())),
                LogEntry::Commit(commit) => sections
                    .last_mut()
                    .expect("a divider comes before the commits")
                    .1
                    .push(commit.summary),
            }
            true
        })
        .unwrap();
        sections.into_inner().unwrap()
    }
}

fn section(label: &str, commits: &[&str]) -> (String, Vec<String>) {
    (
        label.to_string(),
        commits.iter().map(|commit| commit.to_string()).collect(),
    )
}

/// `main` with two commits, `feature` with `f1` and `f2` pushed, `f3` unpushed, `m3` on main (pushed)
/// and merged into feature (unpushed `merge`), and `f4` on top.
fn feature_repo() -> Repo {
    let repo = Repo::new();
    repo.commit_file("m1");
    repo.connect();
    repo.commit_file("m2");
    repo.push("main");
    repo.git(&["remote", "set-head", "origin", "main"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.commit_file("f1");
    repo.commit_file("f2");
    repo.push("feature");
    repo.commit_file("f3");
    repo.git(&["checkout", "-q", "main"]);
    repo.commit_file("m3");
    repo.push("main");
    repo.git(&["checkout", "-q", "feature"]);
    repo.minute.set(repo.minute.get() + 1);
    repo.git(&["merge", "-q", "--no-edit", "-m", "merge", "main"]);
    repo.commit_file("f4");
    repo
}

#[test]
fn feature_branch_in_sections() {
    let repo = feature_repo();
    assert_eq!(
        repo.log(),
        [
            section("feature (unpushed)", &["f4", "merge", "f3"]),
            section("feature", &["f2", "f1"]),
            section("origin/main", &["m3", "m2", "m1"]),
        ]
    );
}

#[test]
fn sections_are_what_git_rev_list_says() {
    let repo = feature_repo();
    let rev_list = |args: &[&str]| -> Vec<String> {
        let mut command = vec!["log", "--format=%s"];
        command.extend(args);
        repo.git(&command).lines().map(str::to_string).collect()
    };
    let log = repo.log();
    assert_eq!(
        log[0].1,
        rev_list(&["HEAD", "^origin/feature", "^origin/main"])
    );
    assert_eq!(log[1].1, rev_list(&["origin/feature", "^origin/main"]));
    assert_eq!(log[2].1, rev_list(&["origin/main"]));
    // every commit once
    let all = rev_list(&["HEAD"]);
    let mut listed: Vec<_> = log
        .iter()
        .flat_map(|(_, commits)| commits.clone())
        .collect();
    listed.sort();
    let mut all_sorted = all;
    all_sorted.sort();
    assert_eq!(listed, all_sorted);
}

#[test]
fn unmerged_main_commits_belong_to_the_feature_section() {
    let repo = Repo::new();
    repo.commit_file("m1");
    repo.connect();
    repo.push("main");
    repo.git(&["remote", "set-head", "origin", "main"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.commit_file("f1");
    // main moves on locally only, and is merged into the feature
    repo.git(&["checkout", "-q", "main"]);
    repo.commit_file("m2");
    repo.git(&["checkout", "-q", "feature"]);
    repo.minute.set(repo.minute.get() + 1);
    repo.git(&["merge", "-q", "--no-edit", "-m", "merge", "main"]);
    // there's no upstream: the label has no "unpushed"
    assert_eq!(
        repo.log(),
        [
            section("feature", &["merge", "m2", "f1"]),
            section("origin/main", &["m1"]),
        ]
    );
}

#[test]
fn on_main_with_unpushed_commits() {
    let repo = feature_repo();
    repo.git(&["checkout", "-q", "main"]);
    repo.commit_file("m4");
    repo.commit_file("m5");
    assert_eq!(
        repo.log(),
        [
            section("main (unpushed)", &["m5", "m4"]),
            section("origin/main", &["m3", "m2", "m1"]),
        ]
    );
}

#[test]
fn on_main_that_is_pushed() {
    let repo = feature_repo();
    repo.git(&["checkout", "-q", "main"]);
    assert_eq!(repo.log(), [section("origin/main", &["m3", "m2", "m1"])]);
}

#[test]
fn detached_head() {
    let repo = feature_repo();
    repo.git(&["checkout", "-q", "--detach", "feature"]);
    assert_eq!(
        repo.log(),
        [
            section("HEAD (detached)", &["f4", "merge", "f3", "f2", "f1"]),
            section("origin/main", &["m3", "m2", "m1"]),
        ]
    );
}

#[test]
fn no_remote_and_no_other_branch() {
    let repo = Repo::new();
    repo.commit_file("m1");
    repo.commit_file("m2");
    // main is the current branch: it's not its own base
    assert_eq!(repo.log(), [section("main", &["m2", "m1"])]);
}

#[test]
fn no_remote_and_a_local_base() {
    let repo = Repo::new();
    repo.commit_file("m1");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.commit_file("f1");
    assert_eq!(
        repo.log(),
        [section("feature", &["f1"]), section("main", &["m1"])]
    );

    // master is a base too
    repo.git(&["branch", "-m", "main", "master"]);
    assert_eq!(
        repo.log(),
        [section("feature", &["f1"]), section("master", &["m1"])]
    );
}

#[test]
fn remote_without_upstream() {
    let repo = feature_repo();
    repo.git(&["checkout", "-q", "-b", "local-only", "feature"]);
    repo.commit_file("l1");
    assert_eq!(
        repo.log(),
        [
            section("local-only", &["l1", "f4", "merge", "f3", "f2", "f1"]),
            section("origin/main", &["m3", "m2", "m1"]),
        ]
    );
}

#[test]
fn upstream_that_does_not_exist_is_ignored() {
    let repo = feature_repo();
    repo.git(&["checkout", "-q", "-b", "gone", "main"]);
    repo.git(&["config", "branch.gone.remote", "origin"]);
    repo.git(&["config", "branch.gone.merge", "refs/heads/gone"]);
    repo.commit_file("g1");
    assert_eq!(
        repo.log(),
        [
            section("gone", &["g1"]),
            section("origin/main", &["m3", "m2", "m1"]),
        ]
    );
}

#[test]
fn upstream_without_a_base() {
    // the remote has no main nor master
    let repo = Repo::new();
    repo.git(&["checkout", "-q", "-b", "dev"]);
    repo.commit_file("d1");
    repo.connect();
    repo.commit_file("d2");
    repo.push("dev");
    repo.commit_file("d3");
    assert_eq!(
        repo.log(),
        [
            section("dev (unpushed)", &["d3"]),
            section("dev", &["d2", "d1"]),
        ]
    );
}

#[test]
fn default_branch_of_the_remote_is_the_base() {
    let repo = feature_repo();
    // origin/HEAD points to something else than main
    repo.git(&["checkout", "-q", "-b", "develop", "main"]);
    repo.commit_file("v1");
    repo.push("develop");
    repo.git(&["remote", "set-head", "origin", "develop"]);
    repo.git(&["checkout", "-q", "feature"]);
    let log = repo.log();
    assert_eq!(log.last().unwrap().0, "origin/develop");
    // develop has v1 on top of main, feature doesn't: it's the same commits as with main
    assert_eq!(log.last().unwrap().1, ["m3", "m2", "m1"]);
}

#[test]
fn branch_without_commits_of_its_own() {
    let repo = feature_repo();
    repo.git(&["checkout", "-q", "-b", "same", "main"]);
    assert_eq!(repo.log(), [section("origin/main", &["m3", "m2", "m1"])]);
}

/// Sections are sets of commits like `git log` says even when the history is a tangle of merges
/// and the dates of the commits are all over the place.
#[test]
fn sections_of_a_tangled_history_are_what_git_log_says() {
    use std::collections::BTreeSet;

    let repo = Repo::new();
    repo.commit_file("c0");
    repo.connect();
    repo.push("main");
    repo.git(&["remote", "set-head", "origin", "main"]);
    // a deterministic pseudo random walk over a few branches
    let branches = ["main", "a", "b", "c"];
    for branch in &branches[1..] {
        repo.git(&["branch", branch, "main"]);
    }
    let mut state = 12345u32;
    let mut next = |bound: u32| {
        state = state.wrapping_mul(1103515245).wrapping_add(12345);
        (state >> 16) % bound
    };
    for i in 1..60 {
        let branch = branches[next(4) as usize];
        repo.git(&["checkout", "-q", branch]);
        // dates that jump around
        repo.minute.set(next(59));
        let other = branches[next(4) as usize];
        if next(4) == 0 && other != branch {
            repo.git(&["merge", "-q", "--no-ff", "-m", &format!("merge{i}"), other]);
        } else {
            repo.commit_file(&format!("c{i}"));
        }
        if next(5) == 0 {
            repo.push(branch);
        }
    }

    let log = |args: &[&str]| -> BTreeSet<String> {
        let mut command = vec!["log", "--format=%s"];
        command.extend(args);
        repo.git(&command).lines().map(str::to_string).collect()
    };
    for branch in &branches[1..] {
        repo.git(&["checkout", "-q", branch]);
        let upstream = repo.git(&[
            "for-each-ref",
            "--format=%(upstream:short)",
            &format!("refs/heads/{branch}"),
        ]);
        let upstream = upstream.trim();
        let not_upstream = format!("^{upstream}");
        let (unpushed, pushed) = if upstream.is_empty() {
            (log(&["HEAD", "^origin/main"]), BTreeSet::new())
        } else {
            let unpushed = log(&["HEAD", &not_upstream, "^origin/main"]);
            let pushed = &log(&["HEAD", "^origin/main"]) - &unpushed;
            (unpushed, pushed)
        };
        let base = &log(&["HEAD"]) - &(&unpushed | &pushed);
        let expected: Vec<_> = [unpushed, pushed, base]
            .into_iter()
            .filter(|section| !section.is_empty())
            .collect();
        let found: Vec<_> = repo
            .log()
            .into_iter()
            .map(|(_, commits)| {
                let set: BTreeSet<_> = commits.iter().cloned().collect();
                assert_eq!(set.len(), commits.len(), "{branch}: a commit twice");
                set
            })
            .collect();
        assert_eq!(found, expected, "{branch} (upstream {upstream:?})");
    }
}

#[test]
fn stops_when_asked_to() {
    let repo = feature_repo();
    let count = std::sync::atomic::AtomicUsize::new(0);
    git::for_each_log_entry(repo.path(), true, |_| {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4
    })
    .unwrap();
    assert_eq!(count.into_inner(), 5);
}

#[test]
fn unborn_head_is_an_error() {
    let repo = Repo::new();
    assert!(git::for_each_log_entry(repo.path(), true, |_| true).is_err());
}

/// The changes of a range as `(kind, path)`, sorted like the changed files picker lists them.
fn range_changes(repo: &Repo, oldest: &str, newest: &str) -> Vec<(String, String)> {
    describe(
        repo,
        git::range_changes(repo.path(), true, oldest, newest).unwrap(),
    )
}

fn describe(repo: &Repo, changes: Vec<FileChange>) -> Vec<(String, String)> {
    let relative = |path: &PathBuf| {
        let canonical = repo.path().canonicalize().unwrap();
        path.strip_prefix(repo.path())
            .or_else(|_| path.strip_prefix(&canonical))
            .unwrap()
            .display()
            .to_string()
    };
    changes
        .into_iter()
        .map(|change| match change {
            FileChange::Added { path } => ("A".to_string(), relative(&path)),
            FileChange::Modified { path } => ("M".to_string(), relative(&path)),
            FileChange::Deleted { path } => ("D".to_string(), relative(&path)),
            FileChange::Renamed { from_path, to_path } => (
                "R".to_string(),
                format!("{}\t{}", relative(&from_path), relative(&to_path)),
            ),
            _ => unreachable!(),
        })
        .collect()
}

/// What `git diff --name-status` says about `base..tip`, sorted by the last path like ours.
fn git_diff(repo: &Repo, base: &str, tip: &str) -> Vec<(String, String)> {
    let mut changes: Vec<(String, String)> = repo
        .git(&["diff", "--name-status", "-M50%", base, tip])
        .lines()
        .map(|line| {
            let (kind, paths) = line.split_once('\t').unwrap();
            (kind[..1].to_string(), paths.to_string())
        })
        .collect();
    changes.sort_by(|a, b| a.1.rsplit('\t').next().cmp(&b.1.rsplit('\t').next()));
    changes
}

/// A history with every kind of change.
fn changes_repo() -> (Repo, Vec<String>) {
    let repo = Repo::new();
    let mut ids = Vec::new();
    repo.write("a.txt", "a\n");
    repo.write("b.txt", "b\nb\nb\nb\n");
    repo.write("dir/c.txt", "c\n");
    ids.push(repo.commit("root"));
    repo.write("a.txt", "a changed\n");
    repo.write("new.txt", "new\n");
    ids.push(repo.commit("modify and add"));
    repo.git(&["mv", "b.txt", "dir/b-renamed.txt"]);
    ids.push(repo.commit("rename"));
    repo.git(&["rm", "-q", "new.txt"]);
    repo.write("dir/c.txt", "c\nmore\n");
    ids.push(repo.commit("delete and modify"));
    repo.write("a.txt", "a changed again\n");
    ids.push(repo.commit("modify"));
    (repo, ids)
}

#[test]
fn range_of_one_commit_is_what_commit_changes_says() {
    let (repo, ids) = changes_repo();
    for id in &ids {
        let single = git::commit_changes(repo.path(), true, id).unwrap();
        assert_eq!(
            describe(&repo, single),
            range_changes(&repo, id, id),
            "{id}"
        );
    }
    // and git agrees (the root commit is compared with nothing)
    assert_eq!(
        range_changes(&repo, &ids[1], &ids[1]),
        git_diff(&repo, &format!("{}^", ids[1]), &ids[1])
    );
    assert_eq!(
        range_changes(&repo, &ids[2], &ids[2]),
        [("R".to_string(), "b.txt\tdir/b-renamed.txt".to_string())]
    );
}

#[test]
fn range_is_the_combined_change() {
    let (repo, ids) = changes_repo();
    for (first, last) in [(1, 3), (1, 4), (2, 4), (3, 4), (2, 3)] {
        assert_eq!(
            range_changes(&repo, &ids[first], &ids[last]),
            git_diff(&repo, &format!("{}^", ids[first]), &ids[last]),
            "{first}..{last}"
        );
    }
    // a file added and deleted in the range isn't in it
    let changes = range_changes(&repo, &ids[1], &ids[3]);
    assert!(changes.iter().all(|(_, path)| !path.contains("new.txt")));
}

#[test]
fn range_from_the_root_commit_is_compared_with_nothing() {
    let (repo, ids) = changes_repo();
    let combined = range_changes(&repo, &ids[0], &ids[4]);
    assert_eq!(
        combined,
        git_diff(
            &repo,
            // the empty tree
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            &ids[4]
        )
    );
    assert!(combined.iter().all(|(kind, _)| kind == "A"), "{combined:?}");
    assert_eq!(
        range_changes(&repo, &ids[0], &ids[0]),
        git_diff(&repo, "4b825dc642cb6eb9a060e54bf8d69288fbee4904", &ids[0])
    );
}

#[test]
fn merge_commits_are_compared_with_their_first_parent() {
    let repo = feature_repo();
    let merge = repo.id("HEAD~1");
    assert_eq!(
        range_changes(&repo, &merge, &merge),
        git_diff(&repo, &format!("{merge}^1"), &merge)
    );
    assert_eq!(
        range_changes(&repo, &merge, &merge),
        [("A".to_string(), "m3.txt".to_string())]
    );
}
