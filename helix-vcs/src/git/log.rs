//! The commit log of HEAD, in sections: the commits of the current branch that aren't pushed,
//! the ones that are, and the ones that are on the branch the current branch is based on.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;
use gix::{ObjectId, Repository};

use super::{commit_info, open_repo};
use crate::LogEntry;

/// Commits reachable from HEAD but not from any of the `hide` tips, with the label of their
/// divider.
struct Section {
    label: String,
    hide: Vec<ObjectId>,
}

/// The id of the commit `name` points to, if there is such a reference.
fn resolve(repo: &Repository, name: &str) -> Option<ObjectId> {
    let mut reference = repo.try_find_reference(name).ok()??;
    Some(reference.peel_to_id().ok()?.detach())
}

/// The branch the current branch is built on, as its full reference name and the label of its
/// section: the first that exists of the remote's default branch (`<remote>/HEAD`), `<remote>/main`,
/// `<remote>/master` and the local `main` and `master`. The current branch itself is never its
/// own base.
fn base_branch(
    repo: &Repository,
    remote: &str,
    current: Option<&str>,
) -> Option<(String, ObjectId)> {
    let remote_head = format!("refs/remotes/{remote}/HEAD");
    let default_branch = repo
        .try_find_reference(remote_head.as_str())
        .ok()
        .flatten()
        .and_then(|reference| match reference.target() {
            gix::refs::TargetRef::Symbolic(name) => Some(name.as_bstr().to_string()),
            gix::refs::TargetRef::Object(_) => None,
        });
    default_branch
        .into_iter()
        .chain(
            ["main", "master"]
                .into_iter()
                .map(|branch| format!("refs/remotes/{remote}/{branch}")),
        )
        .chain(
            ["main", "master"]
                .into_iter()
                .map(|branch| format!("refs/heads/{branch}")),
        )
        .filter(|name| Some(name.as_str()) != current)
        .find_map(|name| Some((short_name(&name), resolve(repo, &name)?)))
}

/// `refs/heads/main` as `main`, `refs/remotes/origin/main` as `origin/main`.
fn short_name(name: &str) -> String {
    let name = name.strip_prefix("refs/").unwrap_or(name);
    let name = name
        .strip_prefix("heads/")
        .or_else(|| name.strip_prefix("remotes/"))
        .unwrap_or(name);
    name.to_string()
}

/// The sections of the log of HEAD, in the order they are listed.
fn sections(repo: &Repository) -> Result<(ObjectId, Vec<Section>)> {
    let head_id = repo.head_id()?.detach();
    let head_name = repo.head_name()?;
    let current = head_name.as_ref().map(|name| name.as_bstr().to_string());
    let branch = head_name.as_ref().map(|name| name.shorten().to_string());

    // the remote tracking branch of the current branch, if it's configured and exists
    let upstream = head_name.as_ref().and_then(|name| {
        let tracking = repo
            .branch_remote_tracking_ref_name(name.as_ref(), gix::remote::Direction::Fetch)?
            .ok()?;
        resolve(repo, &tracking.as_bstr().to_string())
    });
    let remote = branch
        .as_deref()
        .and_then(|branch| {
            repo.branch_remote_name(branch, gix::remote::Direction::Fetch)?
                .as_symbol()
                .map(str::to_string)
        })
        .unwrap_or_else(|| "origin".to_string());
    let base = base_branch(repo, &remote, current.as_deref());

    let own = branch.unwrap_or_else(|| "HEAD (detached)".to_string());
    let hide_base = || base.iter().map(|(_, id)| *id);
    let mut sections = vec![Section {
        label: match (&current, upstream) {
            (Some(_), Some(_)) => format!("{own} (unpushed)"),
            _ => own.clone(),
        },
        hide: upstream.into_iter().chain(hide_base()).collect(),
    }];
    if upstream.is_some() {
        sections.push(Section {
            label: own,
            hide: hide_base().collect(),
        });
    }
    if let Some((label, _)) = base {
        sections.push(Section {
            label,
            hide: Vec::new(),
        });
    }
    Ok((head_id, sections))
}

/// Calls `f` with the commits reachable from HEAD, newest first within each of the sections of
/// the log, each preceded by its divider. Empty sections have no divider. Stops when `f`
/// returns `false`.
pub fn for_each_log_entry(
    cwd: &Path,
    trust_full: bool,
    f: impl Fn(Result<LogEntry>) -> bool,
) -> Result<()> {
    let repo = open_repo(cwd, trust_full)?.to_thread_local();
    let (head, sections) = sections(&repo)?;
    // the commits of the sections listed so far, for the sections after them to skip. The first
    // sections hide what's on other branches so they are small, but the last one can be the
    // whole history: nothing is remembered after it.
    let mut listed = HashSet::new();
    let last = sections.len() - 1;
    for (i, section) in sections.iter().enumerate() {
        let walk = repo
            .rev_walk([head])
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
            .with_hidden(section.hide.iter().copied())
            .all()?;
        let mut started = false;
        for info in walk {
            let info = match info {
                Ok(info) => info,
                Err(err) => {
                    if f(Err(err.into())) {
                        continue;
                    }
                    return Ok(());
                }
            };
            if listed.contains(&info.id) {
                continue;
            }
            if !started {
                started = true;
                if !f(Ok(LogEntry::Divider(section.label.clone()))) {
                    return Ok(());
                }
            }
            let commit = info
                .object()
                .map_err(anyhow::Error::from)
                .and_then(|commit| commit_info(&commit));
            if !f(commit.map(LogEntry::Commit)) {
                return Ok(());
            }
            if i < last {
                listed.insert(info.id);
            }
        }
    }
    Ok(())
}
