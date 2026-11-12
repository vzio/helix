use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use tempfile::TempDir;

use super::{
    summary::{
        sort_by_modified, Changes, Commit, Dated, Head, InProgress, Operation, Remote, Repo,
        Upstream, Worktree,
    },
    test::{create_commit_with_message, empty_git_repo, exec_git_args},
};

fn git(dir: &Path, args: &[&str]) {
    exec_git_args(args, dir)
}

fn write(dir: &Path, name: &str, contents: &str) {
    fs::write(dir.join(name), contents).unwrap();
}

fn open(dir: &Path) -> Repo {
    Repo::open(dir, true).unwrap().expect("a repository")
}

/// A repository with one commit, "first", of `a.txt`.
fn repo_with_commit() -> TempDir {
    let tmp = empty_git_repo();
    write(tmp.path(), "a.txt", "a\n");
    create_commit_with_message(tmp.path(), true, "first\n\nbody");
    tmp
}

/// Two branches changing the same two files differently, `main` checked out.
fn diverged() -> TempDir {
    let tmp = empty_git_repo();
    let dir = tmp.path();
    write(dir, "one.txt", "base\n");
    write(dir, "two.txt", "base\n");
    create_commit_with_message(dir, true, "base");
    git(dir, &["checkout", "-b", "side"]);
    write(dir, "one.txt", "side\n");
    write(dir, "two.txt", "side\n");
    create_commit_with_message(dir, true, "side");
    git(dir, &["checkout", "main"]);
    write(dir, "one.txt", "main\n");
    write(dir, "two.txt", "main\n");
    create_commit_with_message(dir, true, "main");
    tmp
}

fn git_fails(dir: &Path, args: &[&str]) {
    // exec_git_args panics on failure: a conflict is the expected outcome here
    let res = std::panic::catch_unwind(|| exec_git_args(args, dir));
    assert!(res.is_err());
}

#[test]
fn not_a_repository() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(Repo::open(tmp.path(), true).unwrap().is_none());
}

#[test]
fn unborn_repository() {
    let tmp = empty_git_repo();
    write(tmp.path(), "new.txt", "x\n");
    let repo = open(tmp.path());
    assert_eq!(
        repo.head().unwrap(),
        Head {
            branch: Some("main".into()),
            commit: None
        }
    );
    assert_eq!(
        repo.changes().unwrap(),
        Changes {
            staged: 0,
            unstaged: 0,
            untracked: 1
        }
    );
    assert_eq!(repo.upstream().unwrap(), None);
    assert_eq!(repo.in_progress().unwrap(), None);
}

#[test]
fn branch_and_commit() {
    let tmp = repo_with_commit();
    let repo = open(tmp.path());
    let head = repo.head().unwrap();
    let id = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["rev-parse", "--short=7", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(
        head,
        Head {
            branch: Some("main".into()),
            commit: Some(Commit {
                short_id: id.trim().into(),
                summary: "first".into()
            })
        }
    );
    assert_eq!(head.commit.unwrap().short_id.len(), 7);
    assert_eq!(
        repo.work_dir().canonicalize().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
}

#[test]
fn detached_head() {
    let tmp = repo_with_commit();
    git(tmp.path(), &["checkout", "--detach"]);
    let head = open(tmp.path()).head().unwrap();
    assert_eq!(head.branch, None);
    assert_eq!(head.commit.unwrap().summary, "first");
}

#[test]
fn remotes() {
    let tmp = repo_with_commit();
    let dir = tmp.path();
    let repo = open(dir);
    assert_eq!(repo.remote().unwrap(), None);

    git(
        dir,
        &["remote", "add", "zeta", "https://example.com/zeta.git"],
    );
    assert_eq!(
        open(dir).remote().unwrap(),
        Some(Remote {
            name: "zeta".into(),
            url: "https://example.com/zeta.git".into()
        })
    );
    git(
        dir,
        &["remote", "add", "alpha", "https://example.com/alpha.git"],
    );
    assert_eq!(open(dir).remote().unwrap().unwrap().name, "alpha");
    git(
        dir,
        &["remote", "add", "origin", "https://example.com/o.git"],
    );
    assert_eq!(open(dir).remote().unwrap().unwrap().name, "origin");

    // the remote of the upstream wins over origin
    git(dir, &["config", "branch.main.remote", "zeta"]);
    git(dir, &["config", "branch.main.merge", "refs/heads/x"]);
    assert_eq!(open(dir).remote().unwrap().unwrap().name, "zeta");
}

#[test]
fn upstream_and_ahead() {
    let tmp = repo_with_commit();
    let dir = tmp.path();
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare"]);
    git(
        dir,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    assert_eq!(open(dir).upstream().unwrap(), None);

    git(dir, &["push", "-u", "origin", "main"]);
    assert_eq!(
        open(dir).upstream().unwrap(),
        Some(Upstream {
            name: "origin/main".into(),
            ahead: 0
        })
    );

    for n in 0..3 {
        write(dir, "a.txt", &format!("{n}\n"));
        create_commit_with_message(dir, true, &format!("c{n}"));
    }
    assert_eq!(open(dir).upstream().unwrap().unwrap().ahead, 3);

    // a branch without upstream
    git(dir, &["checkout", "-b", "local"]);
    assert_eq!(open(dir).upstream().unwrap(), None);

    // an upstream whose ref doesn't exist locally
    git(dir, &["config", "branch.local.remote", "origin"]);
    git(dir, &["config", "branch.local.merge", "refs/heads/nothing"]);
    assert_eq!(open(dir).upstream().unwrap(), None);
}

#[test]
fn counts_of_changes() {
    let tmp = empty_git_repo();
    let dir = tmp.path();
    write(dir, ".gitignore", "*.log\n");
    for name in ["a", "b", "c", "d"] {
        write(dir, &format!("{name}.txt"), "x\n");
    }
    create_commit_with_message(dir, true, "init");
    assert!(open(dir).changes().unwrap().is_clean());

    // two staged: one edited, one new; one staged then edited again
    write(dir, "a.txt", "edit\n");
    write(dir, "new.txt", "new\n");
    write(dir, "b.txt", "edit\n");
    git(dir, &["add", "a.txt", "new.txt", "b.txt"]);
    write(dir, "b.txt", "edit again\n");
    // one unstaged only
    write(dir, "c.txt", "edit\n");
    // untracked, in a directory, and an ignored one
    fs::create_dir(dir.join("sub")).unwrap();
    write(dir, "sub/u.txt", "u\n");
    write(dir, "ignored.log", "i\n");

    let repo = open(dir);
    assert_eq!(
        repo.changes().unwrap(),
        Changes {
            staged: 3,
            unstaged: 2,
            untracked: 1
        }
    );
    assert!(!repo.changes().unwrap().is_clean());
}

#[test]
fn nothing_in_progress() {
    let tmp = repo_with_commit();
    assert_eq!(open(tmp.path()).in_progress().unwrap(), None);
}

#[test]
fn rebase_with_conflicts() {
    let tmp = diverged();
    let dir = tmp.path();
    git_fails(dir, &["rebase", "side"]);
    let repo = open(dir);
    assert_eq!(
        repo.in_progress().unwrap(),
        Some(InProgress {
            operation: Operation::Rebase,
            conflicted: 2
        })
    );
    // conflicts are in none of the counts
    assert_eq!(repo.changes().unwrap(), Changes::default());
}

#[test]
fn merge_with_conflicts() {
    let tmp = diverged();
    let dir = tmp.path();
    git_fails(dir, &["merge", "side"]);
    let progress = open(dir).in_progress().unwrap().unwrap();
    assert_eq!(progress.operation, Operation::Merge);
    assert_eq!(progress.conflicted, 2);
    assert_eq!(progress.operation.name(), "merge");
}

#[test]
fn conflicts_without_operation() {
    let tmp = diverged();
    let dir = tmp.path();
    git(dir, &["reset", "--hard", "HEAD~1"]);
    write(dir, "one.txt", "stashed\n");
    git(dir, &["stash"]);
    git(dir, &["merge", "--ff-only", "side"]);
    git_fails(dir, &["stash", "pop"]);
    assert_eq!(
        open(dir).in_progress().unwrap(),
        Some(InProgress {
            operation: Operation::Conflicts,
            conflicted: 1
        })
    );
}

#[test]
fn worktrees_of_both_sides() {
    let tmp = repo_with_commit();
    let main = tmp.path();
    assert_eq!(open(main).worktrees().unwrap(), vec![]);

    let other = tempfile::tempdir().unwrap();
    let linked = other.path().join("linked");
    git(
        main,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );

    let id = open(main).head().unwrap().commit.unwrap().short_id;
    let canonical = |path: &Path| path.canonicalize().unwrap();

    let from_main = open(main).worktrees().unwrap();
    assert_eq!(from_main.len(), 1);
    assert_eq!(canonical(&from_main[0].path), canonical(&linked));
    assert_eq!(from_main[0].branch.as_deref(), Some("feature"));
    assert_eq!(from_main[0].short_id.as_deref(), Some(id.as_str()));

    let from_linked = open(&linked).worktrees().unwrap();
    assert_eq!(from_linked.len(), 1);
    let Worktree {
        path,
        branch,
        short_id,
        ..
    } = &from_linked[0];
    assert_eq!(canonical(path), canonical(main));
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(short_id.as_deref(), Some(id.as_str()));

    // a detached one
    let detached = other.path().join("detached");
    git(
        main,
        &["worktree", "add", "--detach", detached.to_str().unwrap()],
    );
    let from_main = open(main).worktrees().unwrap();
    assert_eq!(from_main.len(), 2);
    let detached_entry = from_main
        .iter()
        .find(|w| canonical(&w.path) == canonical(&detached))
        .unwrap();
    assert_eq!(detached_entry.branch, None);
    assert_eq!(detached_entry.short_id.as_deref(), Some(id.as_str()));
}

#[test]
fn tracked_files_only() {
    let tmp = empty_git_repo();
    let dir = tmp.path();
    write(dir, "b.txt", "b\n");
    write(dir, "a.txt", "a\n");
    fs::create_dir(dir.join("sub")).unwrap();
    write(dir, "sub/c.txt", "c\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", dir.join("link")).unwrap();
    create_commit_with_message(dir, true, "files");
    write(dir, "untracked.txt", "u\n");

    let repo = open(dir);
    let root = repo.work_dir().to_path_buf();
    assert_eq!(
        repo.tracked_files().unwrap(),
        vec![
            root.join("a.txt"),
            root.join("b.txt"),
            root.join("sub/c.txt")
        ]
    );
}

#[test]
fn tracked_files_once_with_conflicts() {
    let tmp = diverged();
    let dir = tmp.path();
    git_fails(dir, &["merge", "side"]);
    let repo = open(dir);
    let root = repo.work_dir().to_path_buf();
    assert_eq!(
        repo.tracked_files().unwrap(),
        vec![root.join("one.txt"), root.join("two.txt")]
    );
}

/// The repository opened without full workspace trust: gix then leaves `.git/config` out of
/// its own config, so remotes and upstreams must come from reading that file directly.
fn open_reduced(dir: &Path) -> Repo {
    Repo::open(dir, false)
        .unwrap()
        .expect("a repository (reduced trust)")
}

// Whether gix leaves out the repository's own config at reduced trust depends on the
// environment (it does here when the files belong to root, not on CI runners), so the tests below
// only check that the remote and the upstream are reported either way.
#[test]
fn reduced_trust_still_reports_the_remote() {
    let tmp = repo_with_commit();
    let dir = tmp.path();
    assert_eq!(open_reduced(dir).remote().unwrap(), None);

    git(
        dir,
        &["remote", "add", "zeta", "https://example.com/zeta.git"],
    );
    assert_eq!(
        open_reduced(dir).remote().unwrap(),
        Some(Remote {
            name: "zeta".into(),
            url: "https://example.com/zeta.git".into()
        })
    );
    git(
        dir,
        &["remote", "add", "origin", "https://example.com/o.git"],
    );
    assert_eq!(
        open_reduced(dir).remote().unwrap(),
        Some(Remote {
            name: "origin".into(),
            url: "https://example.com/o.git".into()
        })
    );
    // the remote of the upstream wins over origin
    git(dir, &["config", "branch.main.remote", "zeta"]);
    git(dir, &["config", "branch.main.merge", "refs/heads/x"]);
    assert_eq!(
        open_reduced(dir).remote().unwrap(),
        Some(Remote {
            name: "zeta".into(),
            url: "https://example.com/zeta.git".into()
        })
    );
}

#[test]
fn reduced_trust_still_reports_the_upstream_and_ahead() {
    let tmp = repo_with_commit();
    let dir = tmp.path();
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare"]);
    git(
        dir,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    git(dir, &["push", "-u", "origin", "main"]);
    assert_eq!(
        open_reduced(dir).upstream().unwrap(),
        Some(Upstream {
            name: "origin/main".into(),
            ahead: 0
        })
    );
    for n in 0..3 {
        write(dir, "a.txt", &format!("{n}\n"));
        create_commit_with_message(dir, true, &format!("c{n}"));
    }
    let repo = open_reduced(dir);
    assert_eq!(
        repo.upstream().unwrap(),
        Some(Upstream {
            name: "origin/main".into(),
            ahead: 3
        })
    );
    assert_eq!(repo.remote().unwrap().unwrap().name, "origin");

    // no upstream configured, and one whose ref doesn't exist
    git(dir, &["checkout", "-b", "local"]);
    assert_eq!(open_reduced(dir).upstream().unwrap(), None);
    git(dir, &["config", "branch.local.remote", "origin"]);
    git(dir, &["config", "branch.local.merge", "refs/heads/nothing"]);
    assert_eq!(open_reduced(dir).upstream().unwrap(), None);
}

/// The dashboard reads local git only: the `gix` features must not include a transport.
#[test]
fn gix_is_built_without_network_features() {
    let manifest = include_str!("../../Cargo.toml");
    let line = manifest
        .lines()
        .find(|line| line.starts_with("gix = "))
        .expect("the gix dependency");
    assert!(
        line.contains("default-features = false"),
        "gix's default features must stay off: {line}"
    );
    let features = line
        .split_once("features = [")
        .and_then(|(_, rest)| rest.split_once(']'))
        .expect("gix lists its features")
        .0;
    let features: Vec<&str> = features
        .split(',')
        .map(|feature| feature.trim().trim_matches('"'))
        .filter(|feature| !feature.is_empty())
        .collect();
    assert!(features.contains(&"status"), "{features:?}");
    for feature in &features {
        for banned in [
            "network",
            "http",
            "curl",
            "reqwest",
            "blocking-network-client",
        ] {
            assert!(
                !feature.contains(banned),
                "gix feature {feature:?} brings in the network ({banned})"
            );
        }
    }
}

// ---- worktrees: last modified times, dated order, details ----

/// An hour, the unit of the times the fixtures set.
const HOUR: Duration = Duration::from_secs(3600);

/// Sets the modification time of a file (no-op when it doesn't exist).
fn touch(path: &Path, time: SystemTime) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        file.set_modified(time).unwrap();
    }
}

/// The files of a git dir that date a worktree: its index, `HEAD` and `logs/HEAD`.
fn dating_files(git_dir: &Path) -> [PathBuf; 3] {
    [
        git_dir.join("index"),
        git_dir.join("HEAD"),
        git_dir.join("logs").join("HEAD"),
    ]
}

/// Sets all the dating files of a git dir to `time`.
fn touch_git_files(git_dir: &Path, time: SystemTime) {
    for file in dating_files(git_dir) {
        touch(&file, time);
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

/// A main worktree with three linked ones and a deleted folder:
///
/// - `dirty`: a staged, an unstaged and an untracked file, modified 10 hours before `epoch`;
/// - `ahead`: two commits ahead of its upstream (the local branch `main`), 35 hours before;
/// - `detached`: nothing, 60 hours before;
/// - `gone`: created, then its folder deleted.
///
/// The main worktree is 80 hours before `epoch` and clean, with `origin/main` as its upstream.
struct Fleet {
    _main_tmp: TempDir,
    _others: TempDir,
    _remote: TempDir,
    epoch: SystemTime,
    main: PathBuf,
    dirty: PathBuf,
    ahead: PathBuf,
    detached: PathBuf,
    gone: PathBuf,
}

impl Fleet {
    fn new() -> Self {
        let main_tmp = repo_with_commit();
        let main = main_tmp.path().to_path_buf();
        write(&main, "tracked.txt", "tracked\n");
        create_commit_with_message(&main, true, "second");

        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare"]);
        git(
            &main,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(&main, &["push", "-u", "origin", "main"]);

        let others = tempfile::tempdir().unwrap();
        let add = |name: &str, args: &[&str]| {
            let path = others.path().join(name);
            let mut full = vec!["worktree", "add"];
            full.extend_from_slice(args);
            full.push(path.to_str().unwrap());
            git(&main, &full);
            path
        };
        let dirty = add("dirty", &["-b", "dirty"]);
        let ahead = add("ahead", &["-b", "ahead"]);
        let detached = add("detached", &["--detach"]);
        let gone = add("gone", &["-b", "gone"]);

        // dirty: staged, unstaged, untracked
        write(&dirty, "staged.txt", "staged\n");
        git(&dirty, &["add", "staged.txt"]);
        write(&dirty, "tracked.txt", "changed\n");
        write(&dirty, "untracked.txt", "untracked\n");

        // ahead: two commits on top of the local branch `main`
        write(&ahead, "one.txt", "1\n");
        create_commit_with_message(&ahead, true, "one");
        write(&ahead, "two.txt", "2\n");
        create_commit_with_message(&ahead, true, "two");
        git(&ahead, &["branch", "--set-upstream-to=main", "ahead"]);

        fs::remove_dir_all(&gone).unwrap();

        let epoch = SystemTime::now();
        let fleet = Self {
            _main_tmp: main_tmp,
            _others: others,
            _remote: remote,
            epoch,
            main,
            dirty,
            ahead,
            detached,
            gone,
        };
        fleet.set_times();
        fleet
    }

    fn ago(&self, hours: u64) -> SystemTime {
        self.epoch - HOUR * hours as u32
    }

    fn git_dir(&self, worktree: &Path) -> PathBuf {
        open(worktree).current_worktree().unwrap().git_dir
    }

    fn set_times(&self) {
        // git's own files first, all the same age per worktree
        touch_git_files(&self.git_dir(&self.main), self.ago(80));
        touch_git_files(&self.git_dir(&self.dirty), self.ago(50));
        touch_git_files(&self.git_dir(&self.ahead), self.ago(35));
        touch_git_files(&self.git_dir(&self.detached), self.ago(60));
        // every file on disk is old, except the dirty ones
        for dir in [&self.main, &self.dirty, &self.ahead, &self.detached] {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_file() {
                    touch(&path, self.ago(200));
                }
            }
        }
        touch(&self.dirty.join("staged.txt"), self.ago(30));
        touch(&self.dirty.join("untracked.txt"), self.ago(20));
        touch(&self.dirty.join("tracked.txt"), self.ago(10));
    }
}

#[test]
fn status_modified_is_the_newest_of_the_listed_files_and_the_git_files() {
    let tmp = repo_with_commit();
    let dir = tmp.path();
    write(dir, "a.txt", "changed\n");
    write(dir, "new.txt", "new\n");
    let repo = open(dir);
    let git_dir = repo.current_worktree().unwrap().git_dir;
    let [index, head, logs] = dating_files(&git_dir);
    let base = SystemTime::now() - HOUR * 100;
    let at = |hours: u64| base + HOUR * hours as u32;
    let modified = || repo.status().unwrap().modified;

    // each source on its own is the newest in turn
    for (newest, set) in [
        ("a changed file", dir.join("a.txt")),
        ("an untracked file", dir.join("new.txt")),
        ("the index", index.clone()),
        ("HEAD", head.clone()),
        ("logs/HEAD", logs.clone()),
    ] {
        for file in [
            dir.join("a.txt"),
            dir.join("new.txt"),
            index.clone(),
            head.clone(),
            logs.clone(),
        ] {
            touch(&file, at(1));
        }
        touch(&set, at(7));
        assert_eq!(modified(), Some(at(7)), "{newest}");
    }

    // a staged file counts too
    git(dir, &["add", "new.txt"]);
    touch(&dir.join("new.txt"), at(9));
    touch(&index, at(2));
    // a fresh handle: the first one may have kept the index it read before the add
    let repo = open(dir);
    assert_eq!(repo.status().unwrap().changes.staged, 1);
    let modified = || repo.status().unwrap().modified;
    assert_eq!(modified(), Some(at(9)));

    // a listed file that is gone is skipped (here: a tracked file deleted)
    fs::remove_file(dir.join("a.txt")).unwrap();
    touch(&dir.join("new.txt"), at(3));
    // (logs/HEAD, at 7, is now the newest of what is left)
    assert_eq!(repo.status().unwrap().changes.unstaged, 1);
    assert_eq!(modified(), Some(at(7)));
}

#[test]
fn status_modified_never_looks_at_unchanged_tracked_files() {
    let tmp = repo_with_commit();
    let dir = tmp.path();
    write(dir, "b.txt", "b\n");
    create_commit_with_message(dir, true, "second");
    let repo = open(dir);
    let git_dir = repo.current_worktree().unwrap().git_dir;
    let old = SystemTime::now() - HOUR * 100;
    touch_git_files(&git_dir, old);
    // tracked and unchanged, but touched just now
    touch(&dir.join("a.txt"), SystemTime::now());
    touch(&dir.join("b.txt"), SystemTime::now());
    let status = repo.status().unwrap();
    assert!(status.changes.is_clean());
    assert_eq!(status.modified, Some(old));
}

#[test]
fn changes_and_status_agree() {
    let fleet = Fleet::new();
    let repo = open(&fleet.dirty);
    assert_eq!(repo.status().unwrap().changes, repo.changes().unwrap());
}

#[test]
fn dated_worktrees_newest_first_with_the_missing_last() {
    let fleet = Fleet::new();
    let dated = open(&fleet.main).dated_worktrees().unwrap();
    let got: Vec<_> = dated
        .iter()
        .map(|d| {
            (
                canonical_or_raw(&d.worktree.path),
                d.worktree.missing,
                d.modified,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (canonical_or_raw(&fleet.dirty), false, Some(fleet.ago(10))),
            (canonical_or_raw(&fleet.ahead), false, Some(fleet.ago(35))),
            (
                canonical_or_raw(&fleet.detached),
                false,
                Some(fleet.ago(60))
            ),
            (canonical_or_raw(&fleet.gone), true, None),
        ]
    );
}

/// `canonicalize`, or the path itself when it is gone.
fn canonical_or_raw(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        let parent = path.parent().unwrap().canonicalize().unwrap();
        parent.join(path.file_name().unwrap())
    })
}

#[test]
fn dated_worktrees_from_a_linked_worktree_include_the_main_one() {
    let fleet = Fleet::new();
    let dated = open(&fleet.ahead).dated_worktrees().unwrap();
    // from `ahead`: main, dirty, detached and gone; main is dated by its git files
    let main = dated
        .iter()
        .find(|d| canonical(&d.worktree.path) == canonical(&fleet.main))
        .expect("the main worktree");
    assert_eq!(main.modified, Some(fleet.ago(80)));
    assert_eq!(dated.len(), 4);
    assert!(dated.last().unwrap().worktree.missing);
    assert!(dated
        .windows(2)
        .all(|pair| pair[0].modified >= pair[1].modified));
}

#[test]
fn missing_worktrees_are_flagged() {
    let fleet = Fleet::new();
    let worktrees = open(&fleet.main).worktrees().unwrap();
    assert_eq!(worktrees.len(), 4);
    for worktree in &worktrees {
        let is_gone = canonical_or_raw(&worktree.path) == canonical_or_raw(&fleet.gone);
        assert_eq!(worktree.missing, is_gone, "{:?}", worktree.path);
        // a git dir inside the repository's `.git/worktrees`
        assert!(worktree
            .git_dir
            .starts_with(canonical(&fleet.main).join(".git").join("worktrees")));
    }
    // the main worktree seen from a linked one is not missing, and its git dir is `.git`
    let from_linked = open(&fleet.ahead).worktrees().unwrap();
    let main = from_linked
        .iter()
        .find(|w| canonical(&w.path) == canonical(&fleet.main))
        .unwrap();
    assert!(!main.missing);
    assert_eq!(
        canonical(&main.git_dir),
        canonical(&fleet.main).join(".git")
    );
}

#[test]
fn sort_by_modified_orders_newest_first_and_the_undated_by_path() {
    let at = |hours: u64| SystemTime::UNIX_EPOCH + HOUR * hours as u32;
    let entry = |path: &str, modified: Option<SystemTime>, missing: bool| Dated {
        worktree: Worktree {
            path: path.into(),
            branch: None,
            short_id: None,
            git_dir: PathBuf::new(),
            missing,
        },
        modified,
    };
    let mut all = vec![
        entry("/z", None, true),
        entry("/b", Some(at(1)), false),
        entry("/a", None, false),
        entry("/c", Some(at(5)), false),
        entry("/d", Some(at(1)), false),
    ];
    sort_by_modified(&mut all);
    let paths: Vec<_> = all
        .iter()
        .map(|d| d.worktree.path.to_str().unwrap())
        .collect();
    assert_eq!(paths, ["/c", "/b", "/d", "/a", "/z"]);
}

#[test]
fn details_of_each_worktree() {
    let fleet = Fleet::new();
    let worktrees = open(&fleet.main).worktrees().unwrap();
    let details_of = |dir: &Path| {
        worktrees
            .iter()
            .find(|w| canonical_or_raw(&w.path) == canonical_or_raw(dir))
            .unwrap()
            .details(true)
    };

    let dirty = details_of(&fleet.dirty).unwrap();
    assert_eq!(
        dirty.changes,
        Changes {
            staged: 1,
            unstaged: 1,
            untracked: 1
        }
    );
    // `dirty` was branched from `main` without an upstream of its own
    assert_eq!(dirty.upstream, None);
    assert_eq!(dirty.modified, Some(fleet.ago(10)));

    let ahead = details_of(&fleet.ahead).unwrap();
    assert!(ahead.changes.is_clean());
    let upstream = ahead.upstream.expect("an upstream");
    assert_eq!(upstream.ahead, 2);
    assert_eq!(ahead.modified, Some(fleet.ago(35)));

    let detached = details_of(&fleet.detached).unwrap();
    assert!(detached.changes.is_clean());
    assert_eq!(detached.upstream, None);
    assert_eq!(detached.modified, Some(fleet.ago(60)));

    // the main worktree, seen from a linked one, pushed to its remote
    let from_linked = open(&fleet.ahead).worktrees().unwrap();
    let main = from_linked
        .iter()
        .find(|w| canonical(&w.path) == canonical(&fleet.main))
        .unwrap()
        .details(true)
        .unwrap();
    assert!(main.changes.is_clean());
    assert_eq!(main.upstream.unwrap().ahead, 0);
    assert_eq!(main.modified, Some(fleet.ago(80)));
}

#[test]
fn details_of_a_missing_worktree_fail() {
    let fleet = Fleet::new();
    let worktrees = open(&fleet.main).worktrees().unwrap();
    let gone = worktrees.iter().find(|w| w.missing).unwrap();
    assert!(gone.details(true).is_err());
    assert_eq!(gone.quick_modified(), None);
    assert_eq!(gone.modified(true), None);
}

#[test]
fn quick_modified_reads_the_git_dir_only() {
    let fleet = Fleet::new();
    let worktrees = open(&fleet.main).worktrees().unwrap();
    let quick = |dir: &Path| {
        worktrees
            .iter()
            .find(|w| canonical_or_raw(&w.path) == canonical_or_raw(dir))
            .unwrap()
            .quick_modified()
    };
    // dirty files don't show: only its index, HEAD and logs/HEAD do
    assert_eq!(quick(&fleet.dirty), Some(fleet.ago(50)));
    assert_eq!(quick(&fleet.ahead), Some(fleet.ago(35)));
    assert_eq!(quick(&fleet.detached), Some(fleet.ago(60)));
    // the full time is the newer one for the dirty worktree
    let dirty = worktrees
        .iter()
        .find(|w| canonical_or_raw(&w.path) == canonical_or_raw(&fleet.dirty))
        .unwrap();
    assert_eq!(dirty.modified(true), Some(fleet.ago(10)));
}

#[test]
fn current_worktree_of_the_main_and_linked_worktrees() {
    let fleet = Fleet::new();
    let main = open(&fleet.main).current_worktree().unwrap();
    assert_eq!(canonical(&main.path), canonical(&fleet.main));
    assert_eq!(main.branch.as_deref(), Some("main"));
    assert!(!main.missing);
    assert_eq!(
        canonical(&main.git_dir),
        canonical(&fleet.main).join(".git")
    );

    let ahead = open(&fleet.ahead).current_worktree().unwrap();
    assert_eq!(canonical(&ahead.path), canonical(&fleet.ahead));
    assert_eq!(ahead.branch.as_deref(), Some("ahead"));
    assert!(ahead.short_id.is_some());
    assert!(!ahead.missing);
    assert_eq!(
        canonical(&ahead.git_dir),
        canonical(&fleet.main)
            .join(".git")
            .join("worktrees")
            .join("ahead")
    );

    let detached = open(&fleet.detached).current_worktree().unwrap();
    assert_eq!(detached.branch, None);
    assert!(detached.short_id.is_some());
}

#[test]
fn a_worktree_with_a_broken_git_file_is_not_its_parent_repository() {
    let fleet = Fleet::new();
    // a linked worktree nested in the main one, whose `.git` file is garbage
    let nested = fleet.main.join("nested");
    git(
        &fleet.main,
        &["worktree", "add", "--detach", nested.to_str().unwrap()],
    );
    let worktrees = open(&fleet.main).worktrees().unwrap();
    let entry = worktrees
        .iter()
        .find(|w| canonical_or_raw(&w.path) == canonical_or_raw(&nested))
        .expect("the nested worktree");
    assert!(entry.details(true).is_ok());
    assert!(entry.modified(true).is_some());

    fs::write(nested.join(".git"), "garbage\n").unwrap();
    // the folder is still there, but it would open as the main worktree around it
    assert!(entry.details(true).is_err());
    assert_eq!(entry.modified(true), None);
}
