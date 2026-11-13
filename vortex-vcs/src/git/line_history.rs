//! The walk of the line history (see [`crate::line_history`]) through a git repository.
//!
//! It starts at HEAD with the lines followed and goes from each commit to its parents, newest
//! first, carrying the lines mapped to each parent. A commit whose file is the same blob as in
//! its parent passes the lines on as they are, without reading the file, so most commits cost a
//! lookup of the path in their tree. The walk ends when no lines are left to follow: they all
//! reached the commits that added them. It reports each commit it looks at, so the caller can
//! show progress and stop it.
//!
//! Commits are taken in the order of the sections of the commit log (the unpushed commits of the
//! branch, then the pushed ones, then the base branch's), newest first in each. That is an order
//! where a commit comes after its children, like the commit time alone, so the lines a commit
//! gets from all its children are known when it is looked at.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gix::bstr::ByteSlice;
use gix::diff::Rewrites;
use gix::object::tree::diff::ChangeDetached as TreeChange;
use gix::objs::tree::EntryKind;
use gix::{ObjectId, Repository};

use super::{commit_info, log::sections, open_repo};
use crate::line_history::{
    map_back, merge_rule, normalize, touching, LineCommit, LineEvent, Mapped, Step,
};
use crate::staging::{line_hunks, LineHunk};

/// The lines followed in a commit: its file (relative to the working tree), the blob of the
/// file and the line ranges in it.
struct Lines {
    path: PathBuf,
    blob: ObjectId,
    ranges: Vec<Range<u32>>,
}

/// A commit waiting to be looked at. The greatest is taken first: the first section, then the
/// newest, then the first queued.
#[derive(PartialEq, Eq)]
struct Queued {
    section: usize,
    time: i64,
    seq: u64,
    id: ObjectId,
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .section
            .cmp(&self.section)
            .then(self.time.cmp(&other.time))
            .then(other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The sections of the commit log: their labels, and the commits of each but the last (which
/// has all the others).
struct Sections {
    labels: Vec<String>,
    members: Vec<HashSet<ObjectId>>,
}

impl Sections {
    fn read(repo: &Repository) -> Result<Sections> {
        let (head, sections) = sections(repo)?;
        let last = sections.len() - 1;
        let mut members = Vec::with_capacity(last);
        // The first sections hide what's on the upstream and the base branch, so they are
        // small; the last one is everything else.
        for section in &sections[..last] {
            let walk = repo
                .rev_walk([head])
                .with_hidden(section.hide.iter().copied())
                .all()?;
            let mut commits = HashSet::new();
            for info in walk {
                commits.insert(info?.id);
            }
            members.push(commits);
        }
        Ok(Sections {
            labels: sections.into_iter().map(|section| section.label).collect(),
            members,
        })
    }

    fn of(&self, id: &ObjectId) -> usize {
        self.members
            .iter()
            .position(|commits| commits.contains(id))
            .unwrap_or(self.labels.len() - 1)
    }
}

/// A parent of the commit looked at, and its file: `None` when it doesn't have it, or isn't in
/// the repository (a shallow clone).
struct Parent {
    id: ObjectId,
    time: i64,
    section: usize,
    file: Option<(PathBuf, ObjectId)>,
}

/// The commits waiting to be looked at, with the lines followed in each.
#[derive(Default)]
struct Queue {
    heap: BinaryHeap<Queued>,
    lines: HashMap<ObjectId, Lines>,
    /// The commits looked at: lines that reach one of them late (a commit dated before its
    /// parent) are dropped.
    done: HashSet<ObjectId>,
    seq: u64,
}

impl Queue {
    fn push(&mut self, id: ObjectId, time: i64, section: usize, lines: Lines) {
        if lines.ranges.is_empty() || self.done.contains(&id) {
            return;
        }
        if let Some(queued) = self.lines.get_mut(&id) {
            // the lines from another child; under another path (two sides renamed the file
            // differently), the first one is kept
            if queued.path == lines.path {
                queued.ranges.extend(lines.ranges);
                queued.ranges = normalize(std::mem::take(&mut queued.ranges));
            }
            return;
        }
        self.seq += 1;
        self.heap.push(Queued {
            section,
            time,
            seq: self.seq,
            id,
        });
        self.lines.insert(id, lines);
    }

    fn push_parent(&mut self, parent: &Parent, ranges: Vec<Range<u32>>) {
        if let Some((path, blob)) = &parent.file {
            let lines = Lines {
                path: path.clone(),
                blob: *blob,
                ranges,
            };
            self.push(parent.id, parent.time, parent.section, lines);
        }
    }

    fn pop(&mut self) -> Option<(Queued, Lines)> {
        while let Some(queued) = self.heap.pop() {
            if let Some(lines) = self.lines.remove(&queued.id) {
                self.done.insert(queued.id);
                return Some((queued, lines));
            }
        }
        None
    }
}

/// The blob of `path` in `tree`, if it has a file there.
fn blob_at(tree: &gix::Tree<'_>, path: &Path) -> Result<Option<ObjectId>> {
    Ok(tree
        .lookup_entry_by_path(path)?
        .filter(|entry| {
            matches!(
                entry.mode().kind(),
                EntryKind::Blob | EntryKind::BlobExecutable
            )
        })
        .map(|entry| entry.object_id()))
}

/// The text of a blob, as the lines are counted in it (no conversion to the working tree: it
/// changes line endings, not lines).
fn read(repo: &Repository, blob: ObjectId) -> Result<String> {
    let data = repo.find_object(blob)?.detach().data;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// The path `path` of `tree` had in `parent_tree`, when the diff between them is a rename of the
/// file (the same rename detection as the lists of changes).
fn renamed_from(
    repo: &Repository,
    parent_tree: &gix::Tree<'_>,
    tree: &gix::Tree<'_>,
    path: &Path,
) -> Option<PathBuf> {
    let options = gix::diff::Options::default().with_rewrites(Some(Rewrites {
        copies: None,
        percentage: Some(0.5),
        limit: 1000,
        ..Default::default()
    }));
    let changes = repo
        .diff_tree_to_tree(Some(parent_tree), tree, options)
        .ok()?;
    changes.into_iter().find_map(|change| match change {
        TreeChange::Rewrite {
            source_location,
            location,
            copy: false,
            ..
        } if location.to_path().ok()? == path => Some(source_location.to_path().ok()?.to_owned()),
        _ => None,
    })
}

/// The text of the file in the commit looked at, read the first time it's needed.
fn text<'a>(repo: &Repository, text: &'a mut Option<String>, blob: ObjectId) -> Result<&'a str> {
    if text.is_none() {
        *text = Some(read(repo, blob)?);
    }
    Ok(text.as_deref().unwrap_or_default())
}

/// What the commit looked at did to the lines from one of its parents: the parent, the lines
/// mapped to it, and for a parent whose file differs, its text and the diff.
type Compared = (Parent, Mapped, Option<(String, Vec<LineHunk>)>);

fn compare(
    repo: &Repository,
    sections: &Sections,
    tree: &gix::Tree<'_>,
    lines: &Lines,
    id: ObjectId,
    after: &mut Option<String>,
) -> Result<Compared> {
    let added = |after: &str, parent: Parent| -> Compared {
        let hunks = line_hunks("", after);
        (
            parent,
            Mapped::absent(&lines.ranges),
            Some((String::new(), hunks)),
        )
    };
    let section = sections.of(&id);
    let Ok(parent) = repo.find_commit(id) else {
        // a shallow clone: as if the parent didn't have the file
        let parent = Parent {
            id,
            time: 0,
            section,
            file: None,
        };
        return Ok(added(text(repo, after, lines.blob)?, parent));
    };
    let parent_tree = parent.tree()?;
    let time = parent.time()?.seconds;
    let file = match blob_at(&parent_tree, &lines.path)? {
        Some(blob) => Some((lines.path.clone(), blob)),
        None => match renamed_from(repo, &parent_tree, tree, &lines.path) {
            Some(old) => blob_at(&parent_tree, &old)?.map(|blob| (old, blob)),
            None => None,
        },
    };
    let parent = Parent {
        id,
        time,
        section,
        file,
    };
    let Some((_, blob)) = parent.file else {
        return Ok(added(text(repo, after, lines.blob)?, parent));
    };
    if blob == lines.blob {
        return Ok((parent, Mapped::same(&lines.ranges), None));
    }
    let before = read(repo, blob)?;
    let hunks = line_hunks(&before, text(repo, after, lines.blob)?);
    let mapped = map_back(&lines.ranges, &hunks);
    Ok((parent, mapped, Some((before, hunks))))
}

/// Calls `f` with the commits that changed `lines` of `file` (normalized line ranges of its
/// version in HEAD), newest first in the sections of the commit log, each section after its
/// divider, and with the number of commits looked at, before each. Stops when `f` returns
/// `false`, or when all the lines reached the commits that added them.
pub fn for_each_line_commit(
    cwd: &Path,
    trust_full: bool,
    file: &Path,
    lines: Vec<Range<u32>>,
    mut f: impl FnMut(LineEvent<'_>) -> bool,
) -> Result<()> {
    let mut repo = open_repo(cwd, trust_full)?.to_thread_local();
    // trees are looked up again and again from one commit to the next
    repo.object_cache_size_if_unset(64 * 1024 * 1024);
    let work_dir = repo
        .workdir()
        .context("repo has no worktree")?
        .to_path_buf();
    let file = gix::path::realpath(file).unwrap_or_else(|_| file.to_path_buf());
    let path = file
        .strip_prefix(&work_dir)
        .context("the file is outside the working tree")?
        .to_path_buf();
    let head = repo.head_commit()?;
    let blob = blob_at(&head.tree()?, &path)?.context("the file isn't in HEAD")?;
    let sections = Sections::read(&repo)?;

    let mut queue = Queue::default();
    let ranges = normalize(lines);
    let start = Lines { path, blob, ranges };
    queue.push(head.id, head.time()?.seconds, sections.of(&head.id), start);
    let mut searched = 0;
    let mut divider = None;
    while let Some((queued, lines)) = queue.pop() {
        searched += 1;
        if !f(LineEvent::Searched(searched)) {
            return Ok(());
        }
        let commit = repo.find_commit(queued.id)?;
        let tree = commit.tree()?;
        let parent_ids: Vec<ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();

        let mut after = None;
        let mut parents = Vec::with_capacity(parent_ids.len());
        let mut first = None;
        let mut error = None;
        let step = merge_rule(parent_ids.len(), |i| {
            match compare(&repo, &sections, &tree, &lines, parent_ids[i], &mut after) {
                Ok((parent, mapped, diff)) => {
                    if i == 0 {
                        first = diff;
                    }
                    parents.push(parent);
                    mapped
                }
                Err(err) => {
                    error.get_or_insert(err);
                    // stops the rule: the error is returned below
                    Mapped::same(&[])
                }
            }
        });
        if let Some(err) = error {
            return Err(err);
        }
        match step {
            Step::Follow { parent, lines } => queue.push_parent(&parents[parent], lines),
            Step::Show {
                parents: mapped,
                added,
            } => {
                if divider != Some(queued.section) {
                    divider = Some(queued.section);
                    if !f(LineEvent::Divider(&sections.labels[queued.section])) {
                        return Ok(());
                    }
                }
                let after = text(&repo, &mut after, lines.blob)?;
                // a root commit added the whole file
                let (before, hunks) =
                    first.unwrap_or_else(|| (String::new(), line_hunks("", after)));
                let old_path = parents
                    .first()
                    .and_then(|parent| parent.file.as_ref())
                    .filter(|(old, _)| *old != lines.path)
                    .map(|(old, _)| work_dir.join(old));
                let shown = LineCommit {
                    info: commit_info(&commit)?,
                    path: work_dir.join(&lines.path),
                    old_path,
                    hunks: touching(&hunks, &lines.ranges).cloned().collect(),
                    lines: lines.ranges.clone(),
                    added,
                    before: &before,
                    after,
                };
                if !f(LineEvent::Commit(Box::new(shown))) {
                    return Ok(());
                }
                for (parent, ranges) in parents.iter().zip(mapped) {
                    queue.push_parent(parent, ranges);
                }
            }
        }
    }
    Ok(())
}
