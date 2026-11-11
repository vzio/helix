//! Revisions that blame looks through, like `git blame --ignore-revs-file`: the lines a listed
//! commit changed are blamed on whatever its parent blamed them on.
//!
//! The revisions come from `.git-blame-ignore-revs` at the root of the working tree (read even
//! when git isn't configured to, the way GitHub does) and from every file named by git's
//! `blame.ignoreRevsFile` setting. Each line of those files holds one full commit id; `#` starts
//! a comment.
//!
//! A line of a changed hunk is matched to the parent's line at the same offset in the hunk, so a
//! commit that rewrites lines in place (a rename) passes them all on; lines it added beyond the
//! hunk's old length have no counterpart and stay blamed on it. Git matches lines by similarity
//! instead, which differs for hunks that also move lines around.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gix::bstr::{BString, ByteSlice};
use gix::ObjectId;

/// The default file, at the root of the working tree.
pub const IGNORE_REVS_FILE: &str = ".git-blame-ignore-revs";

/// Where one line of the blamed file comes from: a line of the file as of a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Origin {
    pub commit: ObjectId,
    /// The file's path as of `commit`.
    pub path: BString,
    /// 0-based line in the file as of `commit`.
    pub line: u32,
}

/// The ignored revisions of `repo`: empty when there is no such file.
pub(super) fn ignored_revs(repo: &gix::Repository) -> HashSet<ObjectId> {
    let Some(workdir) = repo.workdir() else {
        return HashSet::new();
    };
    let mut files = vec![workdir.join(IGNORE_REVS_FILE)];
    let config = repo.config_snapshot();
    for value in config.strings("blame.ignoreRevsFile").unwrap_or_default() {
        if value.is_empty() {
            // like git: an empty value resets the list
            files.truncate(1);
        } else if let Ok(path) = gix::path::try_from_bstr(value.as_ref()) {
            files.push(expand(workdir, &path));
        }
    }
    files.dedup();
    files
        .iter()
        .filter_map(|file| std::fs::read(file).ok())
        .flat_map(|text| parse(&text))
        .collect()
}

/// Relative to the working tree, like git run at its root; `~` is the home directory.
fn expand(workdir: &Path, path: &Path) -> PathBuf {
    workdir.join(vortex_stdx::path::expand_tilde(path))
}

/// The commit ids in an ignore-revs file.
pub(super) fn parse(text: &[u8]) -> Vec<ObjectId> {
    text.lines()
        .filter_map(|line| {
            let line = line.split_str("#").next().unwrap_or_default().trim();
            (!line.is_empty())
                .then(|| ObjectId::from_hex(line).ok())
                .flatten()
        })
        .collect()
}

/// Pass the lines blamed on an ignored commit on to its parent, until no line is left on an
/// ignored commit that has a counterpart in its parent. `blame` blames a file as of a commit.
pub(super) fn pass_over(
    repo: &gix::Repository,
    lines: &mut [Option<Origin>],
    ignored: &HashSet<ObjectId>,
    mut blame: impl FnMut(ObjectId, &BString) -> Option<Vec<Option<Origin>>>,
) {
    // Lines stuck on an ignored commit (no counterpart, or no parent) are left there.
    let mut stuck: HashSet<(ObjectId, BString, u32)> = HashSet::new();
    // Each round moves lines at least one commit down; the history is finite, but bound it.
    for _ in 0..64 {
        let mut by_commit: HashMap<(ObjectId, BString), Vec<usize>> = HashMap::new();
        for (index, origin) in lines.iter().enumerate() {
            let Some(origin) = origin else { continue };
            if ignored.contains(&origin.commit)
                && !stuck.contains(&(origin.commit, origin.path.clone(), origin.line))
            {
                by_commit
                    .entry((origin.commit, origin.path.clone()))
                    .or_default()
                    .push(index);
            }
        }
        if by_commit.is_empty() {
            return;
        }
        for ((commit, path), indices) in by_commit {
            let parent_lines = parent_of(repo, commit).and_then(|parent| {
                let ours = file_at(repo, commit, &path)?;
                let theirs = file_at(repo, parent, &path)?;
                Some((parent, ours, theirs))
            });
            let mapped = parent_lines.and_then(|(parent, ours, theirs)| {
                let hunks = crate::staging::line_hunks(&theirs, &ours);
                let parent_blame = blame(parent, &path)?;
                Some((hunks, parent_blame))
            });
            for index in indices {
                let origin = lines[index].clone().unwrap();
                let passed = mapped.as_ref().and_then(|(hunks, parent_blame)| {
                    let line = counterpart(hunks, origin.line)?;
                    parent_blame.get(line as usize).cloned().flatten()
                });
                match passed {
                    Some(passed) => lines[index] = Some(passed),
                    None => {
                        stuck.insert((origin.commit, origin.path, origin.line));
                    }
                }
            }
        }
    }
}

/// The line of the old file that `line` of the new one corresponds to, given the hunks of the
/// diff from old to new: the same line outside of hunks, the same offset inside of one.
pub(super) fn counterpart(hunks: &[crate::staging::LineHunk], line: u32) -> Option<u32> {
    let mut shift: i64 = 0;
    for hunk in hunks {
        if line < hunk.after.start {
            break;
        }
        if line < hunk.after.end {
            let offset = line - hunk.after.start;
            return (offset < hunk.before.end - hunk.before.start)
                .then(|| hunk.before.start + offset);
        }
        shift = i64::from(hunk.before.end) - i64::from(hunk.after.end);
    }
    u32::try_from(i64::from(line) + shift).ok()
}

fn parent_of(repo: &gix::Repository, commit: ObjectId) -> Option<ObjectId> {
    repo.find_commit(commit)
        .ok()?
        .parent_ids()
        .next()
        .map(|id| id.detach())
}

fn file_at(repo: &gix::Repository, commit: ObjectId, path: &BString) -> Option<String> {
    let tree = repo.find_commit(commit).ok()?.tree().ok()?;
    let path = gix::path::try_from_bstr(path.as_bstr()).ok()?;
    let entry = tree.lookup_entry_by_path(&path).ok()??;
    let blob = repo.find_object(entry.oid()).ok()?;
    Some(String::from_utf8_lossy(&blob.data).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staging::LineHunk;

    #[test]
    fn parses_ids_and_skips_comments() {
        let a = "1111111111111111111111111111111111111111";
        let b = "2222222222222222222222222222222222222222";
        let text = format!("# the rename commit\n{a}\n\n  {b}  # trailing comment\nnot-an-id\n");
        let ids: Vec<String> = parse(text.as_bytes())
            .iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(ids, [a, b]);
    }

    #[test]
    fn counterparts() {
        // old: a b c d e   new: a B c X Y d e   (b -> B in place, X Y added after c)
        let hunks = [
            LineHunk {
                before: 1..2,
                after: 1..2,
            },
            LineHunk {
                before: 3..3,
                after: 3..5,
            },
        ];
        let got: Vec<_> = (0..7).map(|line| counterpart(&hunks, line)).collect();
        assert_eq!(
            got,
            [Some(0), Some(1), Some(2), None, None, Some(3), Some(4)]
        );
    }
}
