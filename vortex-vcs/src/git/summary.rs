//! A summary of a repository for the startup dashboard: HEAD, remote, upstream, uncommitted
//! changes, operation in progress, worktrees and tracked files. Everything is read locally:
//! nothing contacts a remote and nothing is written.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use gix::{
    bstr::ByteSlice,
    remote::Direction,
    status::{index_worktree::Item, plumbing::index_as_worktree::EntryStatus, Item as StatusItem},
    ThreadSafeRepository,
};

use super::open_repo;

/// The length of the short ids shown.
const SHORT_ID_LEN: usize = 7;

/// A repository, opened once and shared by the background readers of the dashboard.
#[derive(Clone)]
pub struct Repo {
    repo: ThreadSafeRepository,
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
    /// Its fetch URL, as written in the config.
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
    /// Unmerged files without any operation, like after a conflicting `git stash pop`.
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
}

/// `branch.<branch>.<key>`, from the repository's config or else its own config file.
fn branch_value(
    repo: &gix::Repository,
    local: Option<&gix::config::File<'static>>,
    branch: &str,
    key: &str,
) -> Option<String> {
    let subsection = Some(branch.as_bytes().as_bstr());
    repo.config_snapshot()
        .plumbing()
        .string_by("branch", subsection, key)
        .map(|value| value.to_str_lossy().into_owned())
        .or_else(|| {
            let value = local?.string_by("branch", subsection, key)?;
            Some(value.to_str_lossy().into_owned())
        })
}

fn short_id(id: &gix::oid) -> String {
    id.to_hex_with_len(SHORT_ID_LEN).to_string()
}

impl Repo {
    /// The repository `cwd` is in, `Ok(None)` when it isn't in one (a bare repository also
    /// counts as none).
    pub fn open(cwd: &Path, trust_full: bool) -> Result<Option<Repo>> {
        let repo = match open_repo(cwd, trust_full) {
            Ok(repo) => repo,
            // not inside a repository
            Err(err)
                if err
                    .downcast_ref::<gix::discover::upwards::Error>()
                    .is_some() =>
            {
                return Ok(None)
            }
            Err(err) => return Err(err),
        };
        let work_dir = repo.to_thread_local().workdir().map(Path::to_path_buf);
        Ok(work_dir.map(|work_dir| Repo { repo, work_dir }))
    }

    /// The root of the working tree.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    fn local(&self) -> gix::Repository {
        self.repo.to_thread_local()
    }

    /// Where HEAD is: its branch and its commit.
    pub fn head(&self) -> Result<Head> {
        let repo = self.local();
        let head = repo.head()?;
        let branch = head
            .referent_name()
            .map(|name| name.shorten().to_str_lossy().into_owned());
        let commit = match repo.head_commit() {
            Ok(commit) => Some(Commit {
                short_id: short_id(&commit.id),
                summary: commit.message()?.summary().to_str_lossy().into_owned(),
            }),
            // unborn
            Err(_) if head.is_unborn() => None,
            Err(err) => return Err(err.into()),
        };
        Ok(Head { branch, commit })
    }

    /// The remote shown: the remote of the upstream of the current branch, else `origin`, else
    /// the first remote by name. `None` without remotes. Nothing is contacted.
    pub fn remote(&self) -> Result<Option<Remote>> {
        let repo = self.local();
        let local = self.local_config();
        let mut names: BTreeSet<String> = repo
            .remote_names()
            .into_iter()
            .map(|name| name.to_str_lossy().into_owned())
            .collect();
        if let Some(sections) = local.as_ref().and_then(|c| c.sections_by_name("remote")) {
            names.extend(sections.filter_map(|section| {
                Some(
                    section
                        .header()
                        .subsection_name()?
                        .to_str_lossy()
                        .into_owned(),
                )
            }));
        }
        let branch = repo
            .head_name()?
            .map(|name| name.shorten().to_str_lossy().into_owned());
        let tracked = branch
            .as_deref()
            .and_then(|branch| branch_value(&repo, local.as_ref(), branch, "remote"))
            .filter(|name| names.contains(name));
        let Some(name) = tracked
            .or_else(|| names.contains("origin").then(|| "origin".to_owned()))
            .or_else(|| names.iter().next().cloned())
        else {
            return Ok(None);
        };
        let url = match repo.find_remote(name.as_str()) {
            Ok(remote) => remote
                .url(Direction::Fetch)
                .map(|url| url.to_bstring().to_str_lossy().into_owned()),
            Err(_) => None,
        }
        .or_else(|| {
            let value = local
                .as_ref()?
                .string_by("remote", Some(name.as_str().into()), "url")?;
            Some(value.to_str_lossy().into_owned())
        })
        .unwrap_or_default();
        Ok(Some(Remote { name, url }))
    }

    /// The repository's own config file (`.git/config`), parsed as plain data. Without full
    /// workspace trust gix leaves it out of the repository's config, but remotes and upstreams
    /// are names and URLs that make nothing run, so they are read from it.
    fn local_config(&self) -> Option<gix::config::File<'static>> {
        let path = self.local().common_dir().join("config");
        gix::config::File::from_path_no_includes(path, gix::config::Source::Local).ok()
    }

    /// The upstream of the current branch and how many commits HEAD has that it doesn't. `None`
    /// when HEAD is detached or unborn, the branch has no upstream configured, or the upstream
    /// doesn't exist locally.
    pub fn upstream(&self) -> Result<Option<Upstream>> {
        let repo = self.local();
        let Some(branch) = repo.head_name()? else {
            return Ok(None);
        };
        let tracking = match repo.branch_remote_tracking_ref_name(branch.as_ref(), Direction::Fetch)
        {
            Some(Ok(tracking)) => tracking.into_owned(),
            // the upstream from the repository's own config when gix didn't read it
            _ => {
                let local = self.local_config();
                let short = branch.shorten().to_str_lossy().into_owned();
                let (Some(remote), Some(merge)) = (
                    branch_value(&repo, local.as_ref(), &short, "remote"),
                    branch_value(&repo, local.as_ref(), &short, "merge"),
                ) else {
                    return Ok(None);
                };
                let merged = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
                let name = if remote == "." {
                    format!("refs/heads/{merged}")
                } else {
                    format!("refs/remotes/{remote}/{merged}")
                };
                match gix::refs::FullName::try_from(name) {
                    Ok(name) => name,
                    Err(_) => return Ok(None),
                }
            }
        };
        let Ok(mut reference) = repo.find_reference(tracking.as_ref()) else {
            return Ok(None);
        };
        let upstream_id = reference.peel_to_id()?.detach();
        let Ok(head_id) = repo.head_id() else {
            return Ok(None);
        };
        let ahead = repo
            .rev_walk([head_id.detach()])
            .with_hidden([upstream_id])
            .all()?
            .count();
        Ok(Some(Upstream {
            name: tracking.shorten().to_str_lossy().into_owned(),
            ahead,
        }))
    }

    /// Counts of uncommitted changes, by file, like `git status --porcelain`. Unmerged files
    /// are in none of the counts.
    pub fn changes(&self) -> Result<Changes> {
        let repo = self.local();
        let platform = repo
            .status(gix::progress::Discard)?
            .untracked_files(gix::status::UntrackedFiles::Files)
            // renames in the working tree don't change the counts, don't look for them
            .index_worktree_rewrites(None);
        let mut changes = Changes::default();
        for item in platform.into_iter(Vec::new())? {
            match item? {
                StatusItem::TreeIndex(_) => changes.staged += 1,
                StatusItem::IndexWorktree(Item::Modification { status, .. }) => match status {
                    EntryStatus::Conflict { .. } => {}
                    EntryStatus::IntentToAdd => changes.untracked += 1,
                    EntryStatus::Change(_) => changes.unstaged += 1,
                    EntryStatus::NeedsUpdate(_) => {}
                },
                StatusItem::IndexWorktree(Item::DirectoryContents { entry, .. })
                    if entry.status == gix::dir::entry::Status::Untracked =>
                {
                    changes.untracked += 1
                }
                StatusItem::IndexWorktree(_) => {}
            }
        }
        Ok(changes)
    }

    /// The operation in progress and the number of unmerged files. `None` when nothing is in
    /// progress. Unmerged files without an operation are [`Operation::Conflicts`].
    pub fn in_progress(&self) -> Result<Option<InProgress>> {
        use gix::state::InProgress as State;
        let repo = self.local();
        let operation = repo.state().map(|state| match state {
            State::ApplyMailbox
            | State::ApplyMailboxRebase
            | State::Rebase
            | State::RebaseInteractive => Operation::Rebase,
            State::Bisect => Operation::Bisect,
            State::CherryPick | State::CherryPickSequence => Operation::CherryPick,
            State::Merge => Operation::Merge,
            State::Revert | State::RevertSequence => Operation::Revert,
        });
        let index = repo.index_or_empty()?;
        let mut conflicted = 0;
        let mut last: Option<&gix::bstr::BStr> = None;
        // the stages of a file are next to each other
        for entry in index.entries() {
            if entry.stage() == gix::index::entry::Stage::Unconflicted {
                continue;
            }
            let path = entry.path(&index);
            if last != Some(path) {
                conflicted += 1;
                last = Some(path);
            }
        }
        let operation = match operation {
            Some(operation) => operation,
            None if conflicted > 0 => Operation::Conflicts,
            None => return Ok(None),
        };
        Ok(Some(InProgress {
            operation,
            conflicted,
        }))
    }

    /// The other worktrees of the repository: the linked ones when this is the main worktree;
    /// the main one first and then the other linked ones when this is a linked one. Linked
    /// worktrees are sorted by path.
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let repo = self.local();
        // may be spelled `.git/worktrees/x/../..`
        let common_dir = repo
            .common_dir()
            .canonicalize()
            .unwrap_or_else(|_| repo.common_dir().to_path_buf());
        let git_dir = repo.git_dir().to_path_buf();
        let is_linked = !same_path(&git_dir, &common_dir);

        let mut linked = Vec::new();
        for proxy in repo.worktrees()? {
            if same_path(proxy.git_dir(), &git_dir) {
                continue;
            }
            let Ok(path) = proxy.base() else { continue };
            linked.push(worktree_at(&repo, path, proxy.git_dir())?);
        }
        linked.sort_by(|a, b| a.path.cmp(&b.path));

        let mut all = Vec::new();
        if is_linked {
            // the main worktree is the parent of its `.git`
            if let Some(main) = common_dir
                .parent()
                .filter(|_| common_dir.file_name().is_some_and(|name| name == ".git"))
            {
                all.push(worktree_at(&repo, main.to_path_buf(), &common_dir)?);
            }
        }
        all.extend(linked);
        Ok(all)
    }

    /// The files git tracks, as absolute paths in index order, once each. Submodules and
    /// symlinks are skipped.
    pub fn tracked_files(&self) -> Result<Vec<PathBuf>> {
        use gix::index::entry::{Mode, Stage};
        let repo = self.local();
        let index = repo.index_or_empty()?;
        let mut files = Vec::with_capacity(index.entries().len());
        let mut last: Option<&gix::bstr::BStr> = None;
        for entry in index.entries() {
            let mode = entry.mode;
            if mode.contains(Mode::COMMIT) && !mode.contains(Mode::FILE) || mode == Mode::SYMLINK {
                continue;
            }
            let path = entry.path(&index);
            if entry.stage() != Stage::Unconflicted && last == Some(path) {
                continue;
            }
            last = Some(path);
            let path = gix::path::try_from_bstr(path).context("path is not valid")?;
            files.push(self.work_dir.join(path));
        }
        Ok(files)
    }
}

/// Whether both paths are the same directory, even when spelled differently.
fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// The worktree at `path` whose git dir is `git_dir`, read from its HEAD file. Branches are
/// shared by all worktrees, so they are resolved in `repo`.
fn worktree_at(repo: &gix::Repository, path: PathBuf, git_dir: &Path) -> Result<Worktree> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).unwrap_or_default();
    let head = head.trim();
    let (branch, id) = match head.strip_prefix("ref:") {
        Some(name) => {
            let name = name.trim();
            let short = name.strip_prefix("refs/heads/").unwrap_or(name).to_owned();
            let id = repo
                .find_reference(name)
                .ok()
                .and_then(|mut reference| reference.peel_to_id().ok())
                .map(|id| short_id(&id));
            (Some(short), id)
        }
        None => (None, head.get(..SHORT_ID_LEN).map(str::to_owned)),
    };
    Ok(Worktree {
        path,
        branch,
        short_id: id,
    })
}
