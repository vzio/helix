use std::{fs, path::Path};

use tempfile::TempDir;

use super::{
    summary::{Changes, Commit, Head, InProgress, Operation, Remote, Repo, Upstream, Worktree},
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

#[test]
fn reduced_trust_really_hides_the_local_config_from_gix() {
    // the premise of the tests below: without the direct read they would see nothing
    let tmp = repo_with_commit();
    let dir = tmp.path();
    git(
        dir,
        &["remote", "add", "origin", "https://example.com/o.git"],
    );
    let reduced = super::open_repo(dir, false).unwrap().to_thread_local();
    assert!(
        reduced.remote_names().is_empty(),
        "gix read the local config at reduced trust, the tests of it prove nothing"
    );
    let full = super::open_repo(dir, true).unwrap().to_thread_local();
    assert_eq!(full.remote_names().len(), 1);
}

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
