use anyhow::{bail, Context, Result};
use arc_swap::ArcSwap;
use gix::filter::plumbing::driver::apply::Delay;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gix::bstr::ByteSlice;
use gix::diff::index::Change as TreeIndexChange;
use gix::diff::Rewrites;
use gix::dir::entry::Status;
use gix::object::tree::diff::ChangeDetached as TreeChange;
use gix::objs::tree::EntryKind;
use gix::sec::trust::DefaultForLevel;
use gix::status::{
    index_worktree::Item,
    plumbing::index_as_worktree::{Change, EntryStatus},
    Item as StatusItem, UntrackedFiles,
};
use gix::{Commit, ObjectId, Repository, ThreadSafeRepository};

use crate::{CommitInfo, FileChange};

#[cfg(test)]
mod log_test;
#[cfg(test)]
mod summary_test;
#[cfg(test)]
mod test;

pub mod blame;
mod log;
pub mod stage;
pub mod summary;

pub use log::for_each_log_entry;

#[inline]
fn get_repo_dir(file: &Path) -> Result<&Path> {
    file.parent().context("file has no parent directory")
}

pub fn get_diff_base(file: &Path, trust_full: bool) -> Result<Vec<u8>> {
    debug_assert!(!file.exists() || file.is_file());
    debug_assert!(file.is_absolute());
    let file = gix::path::realpath(file).context("resolve symlinks")?;

    // TODO cache repository lookup

    let repo_dir = get_repo_dir(&file)?;
    let repo = open_repo(repo_dir, trust_full)
        .context("failed to open git repo")?
        .to_thread_local();
    let head = repo.head_commit()?;
    let file_oid = find_file_in_commit(&repo, &head, &file)?;

    to_worktree(&repo, &file, file_oid)
}

/// The contents of the blob `oid` as they would be checked out at `file`.
fn to_worktree(repo: &Repository, file: &Path, oid: ObjectId) -> Result<Vec<u8>> {
    let file_object = repo.find_object(oid)?;
    let data = file_object.detach().data;
    // Get the actual data that git would make out of the git object.
    // This will apply the user's git config or attributes like crlf conversions.
    //
    // The whole filter pipeline still runs in untrusted (`Trust::Reduced`) mode so built-in
    // conversions like autocrlf keep working, but gix drops `filter.*.clean` / `filter.*.smudge`
    // drivers defined in untrusted (repository-local) config, so those external programs are not
    // executed unless the workspace was explicitly trusted. This relies on `open_repo` forcing the
    // trust level instead of letting gix re-derive it from `.git` ownership; see the note there.
    if let Some(work_dir) = repo.workdir() {
        let rela_path = file.strip_prefix(work_dir)?;
        let rela_path = gix::path::try_into_bstr(rela_path)?;
        let (mut pipeline, _) = repo.filter_pipeline(None)?;
        let mut worktree_outcome =
            pipeline.convert_to_worktree(&data, rela_path.as_ref(), Delay::Forbid)?;
        let mut buf = Vec::with_capacity(data.len());
        worktree_outcome.read_to_end(&mut buf)?;
        Ok(buf)
    } else {
        Ok(data)
    }
}

pub fn get_current_head_name(file: &Path, trust_full: bool) -> Result<Arc<ArcSwap<Box<str>>>> {
    debug_assert!(!file.exists() || file.is_file());
    debug_assert!(file.is_absolute());
    let file = gix::path::realpath(file).context("resolve symlinks")?;

    let repo_dir = get_repo_dir(&file)?;
    let repo = open_repo(repo_dir, trust_full)
        .context("failed to open git repo")?
        .to_thread_local();
    let head_ref = repo.head_ref()?;
    let head_commit = repo.head_commit()?;

    let name = match head_ref {
        Some(reference) => reference.name().shorten().to_string(),
        None => head_commit.id.to_hex_with_len(8).to_string(),
    };

    Ok(Arc::new(ArcSwap::from_pointee(name.into_boxed_str())))
}

fn commit_info(commit: &Commit) -> Result<CommitInfo> {
    let author = commit.author()?;
    Ok(CommitInfo {
        id: commit.id.to_string(),
        // as short as git abbreviates it (`core.abbrev`), unique in the repository
        short_id: commit.short_id().map_or_else(
            |_| commit.id.to_hex_with_len(7).to_string(),
            |id| id.to_string(),
        ),
        summary: commit.message()?.summary().to_string(),
        author: author.name.to_string(),
        date: author
            .time()?
            .format_or_unix(gix::date::time::format::SHORT),
    })
}

/// The files changed by the commits from `oldest` to `newest`: the difference between the first
/// parent of `oldest` (nothing, for a root commit) and `newest`.
pub fn range_changes(
    cwd: &Path,
    trust_full: bool,
    oldest: &str,
    newest: &str,
) -> Result<Vec<FileChange>> {
    let repo = open_repo(cwd, trust_full)?.to_thread_local();
    let work_dir = repo.workdir().context("repo has no worktree")?;
    let oldest = repo.find_commit(ObjectId::from_hex(oldest.as_bytes())?)?;
    let tree = repo
        .find_commit(ObjectId::from_hex(newest.as_bytes())?)?
        .tree()?;
    let parent_tree = match oldest.parent_ids().next() {
        Some(parent) => Some(
            parent
                .object()
                .with_context(|| missing_parent(&oldest))?
                .into_commit()
                .tree()?,
        ),
        None => None,
    };
    let options = gix::diff::Options::default().with_rewrites(Some(Rewrites {
        copies: None,
        percentage: Some(0.5),
        limit: 1000,
        ..Default::default()
    }));
    let path = |location: &[u8]| -> Result<PathBuf> { Ok(work_dir.join(location.to_path()?)) };
    let mut changes = Vec::new();
    for change in repo.diff_tree_to_tree(parent_tree.as_ref(), &tree, options)? {
        if !change.entry_mode().is_blob_or_symlink() {
            continue;
        }
        changes.push(match change {
            TreeChange::Addition { location, .. } => FileChange::Added {
                path: path(&location)?,
            },
            TreeChange::Deletion { location, .. } => FileChange::Deleted {
                path: path(&location)?,
            },
            TreeChange::Modification { location, .. } => FileChange::Modified {
                path: path(&location)?,
            },
            TreeChange::Rewrite { location, copy, .. } if copy => FileChange::Added {
                path: path(&location)?,
            },
            TreeChange::Rewrite {
                source_location,
                location,
                ..
            } => FileChange::Renamed {
                from_path: path(&source_location)?,
                to_path: path(&location)?,
            },
        });
    }
    changes.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(changes)
}

/// The contents of `file` in commit `id` (`id^` for its first parent), or `None` if it isn't
/// in that commit.
pub fn file_at_commit(
    cwd: &Path,
    trust_full: bool,
    id: &str,
    file: &Path,
) -> Result<Option<Vec<u8>>> {
    let repo = open_repo(cwd, trust_full)?.to_thread_local();
    let (id, parent) = match id.strip_suffix('^') {
        Some(id) => (id, true),
        None => (id, false),
    };
    let mut commit = repo.find_commit(ObjectId::from_hex(id.as_bytes())?)?;
    if parent {
        let parent_id = commit.parent_ids().next().map(|id| id.detach());
        match parent_id {
            Some(parent_id) => {
                commit = repo
                    .find_commit(parent_id)
                    .with_context(|| missing_parent(&commit))?
            }
            None => return Ok(None),
        }
    }
    let Ok(oid) = find_file_in_commit(&repo, &commit, file) else {
        return Ok(None);
    };
    to_worktree(&repo, file, oid).map(Some)
}

/// The contents of the files of the range of commits from `oldest` to `newest`, for its list of
/// changes: for each `(old path, new path)` of `files`, in order, calls `f` with its index, the
/// contents of the old path in the first parent of `oldest` and of the new path in `newest`
/// (`None` where the commit doesn't have the file, or it can't be read), until `f` returns
/// `false`. The repository, the trees and the filters are read once for all of them, where
/// [`file_at_commit`] reads them for each file.
pub fn for_each_range_file(
    cwd: &Path,
    trust_full: bool,
    oldest: &str,
    newest: &str,
    files: &[(PathBuf, PathBuf)],
    mut f: impl FnMut(usize, Option<Vec<u8>>, Option<Vec<u8>>) -> bool,
) -> Result<()> {
    let repo = open_repo(cwd, trust_full)?.to_thread_local();
    let work_dir = repo
        .workdir()
        .context("repo has no worktree")?
        .to_path_buf();
    let oldest = repo.find_commit(ObjectId::from_hex(oldest.as_bytes())?)?;
    let parent_tree = match oldest.parent_ids().next() {
        Some(parent) => Some(
            parent
                .object()
                .with_context(|| missing_parent(&oldest))?
                .into_commit()
                .tree()?,
        ),
        None => None,
    };
    let tree = repo
        .find_commit(ObjectId::from_hex(newest.as_bytes())?)?
        .tree()?;
    let (mut pipeline, _) = repo.filter_pipeline(None)?;
    let mut read = |tree: Option<&gix::Tree<'_>>, file: &Path| -> Option<Vec<u8>> {
        let rela_path = file.strip_prefix(&work_dir).ok()?;
        let entry = tree?.lookup_entry_by_path(rela_path).ok()??;
        if !matches!(
            entry.mode().kind(),
            EntryKind::Blob | EntryKind::BlobExecutable
        ) {
            return None;
        }
        let data = repo.find_object(entry.object_id()).ok()?.detach().data;
        // the same conversions (and filters, when trusted) as `to_worktree`
        let rela_path = gix::path::try_into_bstr(rela_path).ok()?;
        let mut outcome = pipeline
            .convert_to_worktree(&data, rela_path.as_ref(), Delay::Forbid)
            .ok()?;
        let mut buf = Vec::with_capacity(data.len());
        outcome.read_to_end(&mut buf).ok()?;
        Some(buf)
    };
    for (index, (old_path, new_path)) in files.iter().enumerate() {
        let old = read(parent_tree.as_ref(), old_path);
        let new = read(Some(&tree), new_path);
        if !f(index, old, new) {
            break;
        }
    }
    Ok(())
}

fn missing_parent(commit: &Commit) -> String {
    format!(
        "the parent of {} is not in the repository (shallow clone?)",
        commit.id.to_hex_with_len(9)
    )
}

pub fn for_each_changed_file(
    cwd: &Path,
    trust_full: bool,
    f: impl Fn(Result<FileChange>) -> bool,
) -> Result<()> {
    status(&open_repo(cwd, trust_full)?.to_thread_local(), f)
}

/// The working tree and the git dir of the repository `cwd` is in, and the id of HEAD (`None`
/// before the first commit).
pub fn repo_state(cwd: &Path, trust_full: bool) -> Result<crate::RepoState> {
    let repo = open_repo(cwd, trust_full)?.to_thread_local();
    let work_dir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("working tree not found"))?
        .to_path_buf();
    let head = repo.head_id().ok().map(|id| id.detach().to_string());
    Ok(crate::RepoState {
        work_dir,
        git_dir: repo.git_dir().to_path_buf(),
        head,
    })
}

/// The working tree of the repository `cwd` is in, and its unmerged files (absolute, sorted):
/// those with conflict stages in the index, as `git diff --name-only --diff-filter=U` lists them.
pub fn conflicted_files(cwd: &Path, trust_full: bool) -> Result<(PathBuf, Vec<PathBuf>)> {
    let repo = open_repo(cwd, trust_full)?.to_thread_local();
    let work_dir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("working tree not found"))?
        .to_path_buf();
    let index = repo.index_or_empty()?;
    let mut files: Vec<PathBuf> = Vec::new();
    // the entries are sorted by path, then stage: the stages of a file are next to each other
    for entry in index.entries() {
        if entry.stage() == gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let path = work_dir.join(gix::path::from_bstr(entry.path(&index)));
        if files.last() != Some(&path) {
            files.push(path);
        }
    }
    Ok((work_dir, files))
}

fn open_repo(path: &Path, trust_full: bool) -> Result<ThreadSafeRepository> {
    // `trust_full` is the workspace-trust decision made by the caller, and it must be the
    // authority on the gix trust level. gix's own discovery (`discover_*`) ignores a
    // caller-supplied trust level: it always re-derives trust from `.git` ownership, so a malicious
    // `.git/config` in a user-owned directory would be opened as `Trust::Full` regardless of our
    // gate. Worse, the GIT_DIR-environment branch of that discovery panics because it never sets a
    // trust level at all. So we split discovery from opening: find the repository path ourselves,
    // then `open_opts(..).with(trust)`, which forces the trust level and skips gix's ownership
    // check. Under `Trust::Reduced`, gix then refuses to honor untrusted repository-local config
    // such as `filter.*` smudge/clean drivers.

    let trust = if trust_full {
        gix::sec::Trust::Full
    } else {
        gix::sec::Trust::Reduced
    };

    let config = gix::open::permissions::Config {
        system: true,
        git: true,
        user: true,
        env: true,
        includes: true,
        git_binary: false,
    };

    let permissions = gix::open::Permissions {
        config,
        ..gix::open::Permissions::default_for_level(trust)
    };

    let discover_options = gix::discover::upwards::Options {
        dot_git_only: true,
        ..Default::default()
    };
    let (repo_path, _trust_from_ownership) = gix::discover::upwards_opts(path, discover_options)
        .context("failed to discover git repo")?;
    let (git_dir, _work_dir) = repo_path.into_repository_and_work_tree_directories();

    let options = gix::open::Options::default()
        .permissions(permissions)
        // `git_dir` is the discovered `.git` directory (or a linked-worktree git dir), so open it
        // as-is rather than letting gix append `.git` again.
        .open_path_as_is(true)
        .with(trust);

    Ok(ThreadSafeRepository::open_opts(git_dir, options)?)
}

/// Emulates the result of running `git status` from the command line: both staged (HEAD to
/// index) and unstaged (index to worktree) changes, one entry per file.
fn status(repo: &Repository, f: impl Fn(Result<FileChange>) -> bool) -> Result<()> {
    let work_dir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("working tree not found"))?
        .to_path_buf();

    let status_platform = repo
        .status(gix::progress::Discard)?
        // Here we discard the `status.showUntrackedFiles` config, as it makes little sense in
        // our case to not list new (untracked) files. We could have respected this config
        // if the default value weren't `Collapsed` though, as this default value would render
        // the feature unusable to many.
        .untracked_files(UntrackedFiles::Files)
        // Turn on file rename detection, which is off by default.
        .index_worktree_rewrites(Some(Rewrites {
            copies: None,
            percentage: Some(0.5),
            limit: 1000,
            ..Default::default()
        }));

    // No filtering based on path
    let empty_patterns = vec![];

    // A file can be both staged and modified again afterwards, so collect the staged changes
    // first and merge them with the unstaged change of the same file.
    let mut staged: HashMap<PathBuf, FileChange> = HashMap::new();
    let mut unstaged = Vec::new();
    for item in status_platform.into_iter(empty_patterns)? {
        let Ok(item) = item.map_err(|err| f(Err(err.into()))) else {
            continue;
        };
        match item {
            StatusItem::IndexWorktree(item) => {
                if let Some(change) = index_worktree_change(&work_dir, item)? {
                    unstaged.push(change);
                }
            }
            StatusItem::TreeIndex(change) => {
                let change = tree_index_change(&work_dir, change)?;
                staged.insert(change.path().to_path_buf(), change);
            }
        }
    }

    for change in unstaged {
        let change = match staged.remove(change.path()) {
            Some(staged) => match combine_changes(staged, change) {
                Some(change) => change,
                None => continue,
            },
            None => change,
        };
        if !f(Ok(change)) {
            return Ok(());
        }
    }
    for change in staged.into_values() {
        if !f(Ok(change)) {
            break;
        }
    }

    Ok(())
}

/// A change between the index and the worktree, i.e. not staged.
fn index_worktree_change(work_dir: &Path, item: Item) -> Result<Option<FileChange>> {
    let change = match item {
        Item::Modification {
            rela_path, status, ..
        } => {
            let path = work_dir.join(rela_path.to_path()?);
            match status {
                EntryStatus::Conflict { .. } => FileChange::Conflict { path },
                EntryStatus::Change(Change::Removed) => FileChange::Deleted { path },
                EntryStatus::Change(Change::Modification { .. }) => FileChange::Modified { path },
                // Files marked with `git add --intent-to-add`. Such files
                // still show up as new in `git status`, so it's appropriate
                // to show them the same way as untracked files in the
                // "changed file" picker. One example of this being used
                // is Jujutsu, a Git-compatible VCS. It marks all new files
                // with `--intent-to-add` automatically.
                EntryStatus::IntentToAdd => FileChange::Untracked { path },
                _ => return Ok(None),
            }
        }
        Item::DirectoryContents { entry, .. } if entry.status == Status::Untracked => {
            FileChange::Untracked {
                path: work_dir.join(entry.rela_path.to_path()?),
            }
        }
        Item::Rewrite {
            source,
            dirwalk_entry,
            ..
        } => FileChange::Renamed {
            from_path: work_dir.join(source.rela_path().to_path()?),
            to_path: work_dir.join(dirwalk_entry.rela_path.to_path()?),
        },
        _ => return Ok(None),
    };
    Ok(Some(change))
}

/// A change between the tree of HEAD and the index, i.e. staged.
fn tree_index_change(work_dir: &Path, change: TreeIndexChange) -> Result<FileChange> {
    let path = |location: &[u8]| -> Result<PathBuf> { Ok(work_dir.join(location.to_path()?)) };
    Ok(match change {
        TreeIndexChange::Addition { location, .. } => FileChange::Added {
            path: path(&location)?,
        },
        TreeIndexChange::Deletion { location, .. } => FileChange::Deleted {
            path: path(&location)?,
        },
        TreeIndexChange::Modification { location, .. } => FileChange::Modified {
            path: path(&location)?,
        },
        TreeIndexChange::Rewrite { location, copy, .. } if copy => FileChange::Added {
            path: path(&location)?,
        },
        TreeIndexChange::Rewrite {
            source_location,
            location,
            ..
        } => FileChange::Renamed {
            from_path: path(&source_location)?,
            to_path: path(&location)?,
        },
    })
}

/// Merges the staged and the unstaged change of the same file into its change compared to HEAD,
/// or `None` if, all things considered, the file is the same as in HEAD.
fn combine_changes(staged: FileChange, unstaged: FileChange) -> Option<FileChange> {
    use FileChange::*;
    match (staged, unstaged) {
        (_, conflict @ Conflict { .. }) => Some(conflict),
        // added to the index and then deleted: HEAD doesn't have it either
        (Added { .. }, Deleted { .. }) => None,
        // later edits don't change that the file is new, or moved, compared to HEAD
        (staged @ (Added { .. } | Renamed { .. }), Modified { .. }) => Some(staged),
        (Renamed { from_path, .. }, Deleted { .. }) => Some(Deleted { path: from_path }),
        // removed from the index only (`git rm --cached`), so still on disk
        (Deleted { path }, Untracked { .. }) => Some(Modified { path }),
        (_, unstaged) => Some(unstaged),
    }
}

/// Finds the object that contains the contents of a file at a specific commit.
fn find_file_in_commit(repo: &Repository, commit: &Commit, file: &Path) -> Result<ObjectId> {
    let repo_dir = repo.workdir().context("repo has no worktree")?;
    let rel_path = file.strip_prefix(repo_dir)?;
    let tree = commit.tree()?;
    let tree_entry = tree
        .lookup_entry_by_path(rel_path)?
        .context("file is untracked")?;
    match tree_entry.mode().kind() {
        // not a file, everything is new, do not show diff
        mode @ (EntryKind::Tree | EntryKind::Commit | EntryKind::Link) => {
            bail!("entry at {} is not a file but a {mode:?}", file.display())
        }
        // found a file
        EntryKind::Blob | EntryKind::BlobExecutable => Ok(tree_entry.object_id()),
    }
}
