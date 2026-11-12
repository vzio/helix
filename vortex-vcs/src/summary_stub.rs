//! Stand-in for `git::summary` for builds without the `git` feature, so that users of this crate
//! don't need `cfg`s of their own. No repository is ever found: [`Repo::open`] returns `None`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Result};

/// A repository. Without git there are none, so no value of this type exists.
#[derive(Clone)]
pub struct Repo {
    work_dir: PathBuf,
}

/// Where HEAD is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The short name of the current branch, `None` when HEAD is detached.
    pub branch: Option<String>,
    /// The commit HEAD points to, `None` before the first commit.
    pub commit: Option<Commit>,
}

/// A commit, as shown in one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// The first 7 hex digits of its id.
    pub short_id: String,
    /// The first line of its message.
    pub summary: String,
}

/// A configured remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// Its name, like `origin`.
    pub name: String,
    /// Its fetch URL.
    pub url: String,
}

/// The upstream branch of the current branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// Its short name, like `origin/main`.
    pub name: String,
    /// The number of commits HEAD has that the upstream doesn't.
    pub ahead: usize,
}

/// Counts of uncommitted changes, by file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// Files whose index entry differs from HEAD.
    pub staged: usize,
    /// Tracked files whose working tree version differs from the index.
    pub unstaged: usize,
    /// Files git doesn't track and doesn't ignore.
    pub untracked: usize,
}

impl Changes {
    /// Whether there is nothing to commit and nothing untracked.
    pub fn is_clean(&self) -> bool {
        *self == Changes::default()
    }
}

/// An operation that is not finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    /// Unmerged files without any operation.
    Conflicts,
}

impl Operation {
    /// Its name: "merge", "rebase", "cherry-pick", "revert", "bisect" or "conflicts".
    pub fn name(&self) -> &'static str {
        match self {
            Operation::Merge => "merge",
            Operation::Rebase => "rebase",
            Operation::CherryPick => "cherry-pick",
            Operation::Revert => "revert",
            Operation::Bisect => "bisect",
            Operation::Conflicts => "conflicts",
        }
    }
}

/// An operation in progress and its conflicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InProgress {
    /// What is going on.
    pub operation: Operation,
    /// The number of unmerged files.
    pub conflicted: usize,
}

/// A working tree of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Its root.
    pub path: PathBuf,
    /// The short name of its branch, `None` when detached.
    pub branch: Option<String>,
    /// The short id of its HEAD, `None` when unborn.
    pub short_id: Option<String>,
    /// Its directory in the repository's `.git`.
    pub git_dir: PathBuf,
    /// Its folder is gone.
    pub missing: bool,
}

/// A worktree with when it was last modified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dated {
    pub worktree: Worktree,
    pub modified: Option<SystemTime>,
}

/// The state of a worktree's files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub changes: Changes,
    pub modified: Option<SystemTime>,
}

/// What the `:worktrees` picker shows of a worktree besides its path and branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    pub changes: Changes,
    pub upstream: Option<Upstream>,
    pub modified: Option<SystemTime>,
}

/// Orders worktrees by when they were last modified, newest first; the undated last, by path.
pub fn sort_by_modified(worktrees: &mut [Dated]) {
    worktrees.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.worktree.path.cmp(&b.worktree.path))
    });
}

impl Worktree {
    /// Fails: there is no git support compiled in.
    pub fn details(&self, _trust_full: bool) -> Result<Details> {
        bail!("no git support")
    }

    /// Always `None`: there is no git support compiled in.
    pub fn quick_modified(&self) -> Option<SystemTime> {
        None
    }

    /// Always `None`: there is no git support compiled in.
    pub fn modified(&self, _trust_full: bool) -> Option<SystemTime> {
        None
    }
}

impl Repo {
    /// Always `Ok(None)`: there is no git support compiled in.
    pub fn open(_cwd: &Path, _trust_full: bool) -> Result<Option<Repo>> {
        Ok(None)
    }

    /// The root of the working tree.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// Fails: there is no git support compiled in.
    pub fn head(&self) -> Result<Head> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn remote(&self) -> Result<Option<Remote>> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn upstream(&self) -> Result<Option<Upstream>> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn changes(&self) -> Result<Changes> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn in_progress(&self) -> Result<Option<InProgress>> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn status(&self) -> Result<Status> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn dated_worktrees(&self) -> Result<Vec<Dated>> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn current_worktree(&self) -> Result<Worktree> {
        bail!("no git support")
    }

    /// Fails: there is no git support compiled in.
    pub fn tracked_files(&self) -> Result<Vec<PathBuf>> {
        bail!("no git support")
    }
}
