//! Git blame for a whole file.
//!
//! [`FileBlame::try_new`] does all of the (expensive) work up front: it runs the blame
//! algorithm for the file at `HEAD` and resolves every commit that shows up in the result
//! exactly once. The resulting [`FileBlame`] is plain data: it holds no repository handle and
//! no locks, so querying it with [`FileBlame::blame_for_line`] never touches the disk and is
//! cheap enough to do on every render. `try_new` should therefore be run on a background
//! thread, never on the UI thread.
//!
//! [`head_commit_id`] is a much cheaper function that only reads `HEAD`. A caller holding a
//! [`FileBlame`] can compare its [`FileBlame::head`] against it to find out whether the blame
//! is stale (a commit was made, or another branch was checked out) before recomputing it.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;

use super::open_repo;

/// Why a blame could not be computed.
///
/// The distinction between [`NotInRepository`](Self::NotInRepository) and
/// [`NotCommitted`](Self::NotCommitted) is meant for the editor: for a file that is not part
/// of any git repository nothing should be shown, whereas for a file inside of a repository
/// that has not been committed yet (an untracked or new file, or any file in a repository
/// without commits) "Not committed yet" can be shown.
#[derive(Debug)]
pub enum BlameError {
    /// The file is not inside of a git repository (or the repository has no working tree).
    NotInRepository,
    /// The file is inside of a git repository but there is no committed version of it at
    /// `HEAD`: it is untracked, newly added, or the repository does not have a commit yet.
    NotCommitted,
    /// Anything else: corrupt repository, IO errors, `HEAD` not being a commit, etc.
    Other(anyhow::Error),
}

impl fmt::Display for BlameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInRepository => f.write_str("file is not in a git repository"),
            Self::NotCommitted => f.write_str("file is not committed"),
            Self::Other(err) => write!(f, "{err:#}"),
        }
    }
}

impl std::error::Error for BlameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Other(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for BlameError {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err)
    }
}

/// The id of a commit.
///
/// This exists so that users of `helix-vcs` don't need to depend on `gix` just to compare or
/// print commit ids. It is `Copy` and does not allocate.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(gix::ObjectId);

impl fmt::Display for CommitId {
    /// The full hexadecimal id.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CommitId({})", self.0)
    }
}

/// Everything we know about a commit that is blamed for some lines.
///
/// One of these is created per distinct commit and shared (through an [`Arc`]) between all
/// the lines the commit is blamed for. Only the fields that can be missing for a damaged
/// commit object are optional.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlameCommit {
    /// The full id of the commit.
    pub id: CommitId,
    /// The abbreviated hexadecimal id, unambiguous in the repository at the time of blaming.
    pub short_id: String,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    /// The first line of the commit message, without trailing whitespace.
    pub title: Option<String>,
    /// The rest of the commit message, without trailing whitespace. `None` when there is
    /// no more than a title.
    pub body: Option<String>,
    /// Author time as seconds since the unix epoch and the author's timezone offset in
    /// seconds. Used to compute the `{time-ago}` variable at the time of formatting.
    pub time: Option<(i64, i32)>,
    /// The author date as `YYYY-MM-DD`, in the author's timezone.
    pub date: Option<String>,
}

/// Blame information for a file, as of `HEAD` at the time it was computed.
///
/// Holds no repository handle and has no interior mutability, so it is `Send + Sync` and can be
/// kept around (e.g. in an `Arc`) and read from any thread.
#[derive(Debug)]
pub struct FileBlame {
    /// The commit `HEAD` pointed to when the blame was computed. Compare with
    /// [`head_commit_id`] to detect that the blame is stale.
    head: CommitId,
    /// The commit that is blamed for each line of the file at `head`, indexed by 0-based
    /// line number. Lines of the same commit share one allocation.
    lines: Vec<Option<Arc<BlameCommit>>>,
}

impl FileBlame {
    /// Compute the blame for `file` at `HEAD` (expensive: this walks the history of the file).
    ///
    /// All repository access happens here, so this is meant to be run on a background thread.
    ///
    /// The repository is opened with reduced trust: blame only reads objects and has no use
    /// for repository-local configuration that could run programs.
    ///
    /// # Errors
    ///
    /// Fails quickly, without running the blame, with [`BlameError::NotInRepository`] if the
    /// file is not inside of a git repository and with [`BlameError::NotCommitted`] if it is
    /// but `HEAD` does not contain the file.
    pub fn try_new(file: &Path) -> Result<Self, BlameError> {
        let file = resolve_path(file);
        let repo = open_repo_for_file(&file)?;
        let (head, rel_path) = head_and_rel_path(&repo, &file)?;

        let mut resource_cache = repo
            .diff_resource_cache_for_tree_diff()
            .context("failed to create diff resource cache")?;
        let commit_graph = repo.commit_graph_if_enabled().ok().flatten();
        let outcome = gix::blame::file(
            &repo.objects,
            head.0,
            commit_graph,
            &mut resource_cache,
            // NOTE: bstr always uses unix separators `/`, even on
            // Windows which uses the `\` separator
            &gix::path::to_unix_separators_on_windows(
                gix::path::try_into_bstr(rel_path).context("path is not valid")?,
            ),
            gix::blame::Options {
                // follow the file through renames, like `git blame` does. Doesn't make a
                // measurable difference in time.
                rewrites: Some(gix::diff::Rewrites::default()),
                ..Default::default()
            },
        );
        let entries = match outcome {
            Ok(outcome) => outcome.entries,
            Err(gix::blame::Error::FileMissing { .. }) => return Err(BlameError::NotCommitted),
            Err(err) => return Err(BlameError::Other(anyhow::Error::new(err))),
        };

        // Resolve every distinct commit once.
        let mut commits: HashMap<gix::ObjectId, Arc<BlameCommit>> = HashMap::new();
        let line_count = entries
            .iter()
            .map(|entry| entry.start_in_blamed_file + entry.len.get())
            .max()
            .unwrap_or(0);
        let mut lines = vec![None; line_count as usize];
        for entry in entries {
            let commit = commits
                .entry(entry.commit_id)
                .or_insert_with(|| Arc::new(resolve_commit(&repo, entry.commit_id)))
                .clone();
            let start = entry.start_in_blamed_file as usize;
            for line in &mut lines[start..start + entry.len.get() as usize] {
                *line = Some(commit.clone());
            }
        }

        Ok(Self { head, lines })
    }

    /// The commit `HEAD` pointed to when this blame was computed.
    pub fn head(&self) -> CommitId {
        self.head
    }

    /// The number of lines of the file at `HEAD`.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Get the blame information corresponding to a line in the file and diff for that line.
    ///
    /// `line` is the 0-based line in the document, `inserted_lines` and `removed_lines` are the
    /// number of lines inserted and removed (compared to `HEAD`) *before* that line.
    ///
    /// Returns `None` when there is no information for the line: the line is past the end of
    /// the file at `HEAD`, or no blame was reported for it. Cheap: no repository access.
    #[inline]
    pub fn blame_for_line(
        &self,
        line: u32,
        inserted_lines: u32,
        removed_lines: u32,
    ) -> Option<LineBlame> {
        // Because gix_blame doesn't care about stuff that is not commited, we have to "normalize" the
        // line number to account for uncommited code.
        //
        // You'll notice that blame_line can be 0 when, for instance we have:
        // - removed 0 lines
        // - added 10 lines
        // - cursor_line is 8
        //
        // So when our cursor is on the 10th added line or earlier, blame_line will be 0. This means
        // the blame will be incorrect. But that's fine, because when the cursor_line is on some hunk,
        // we can show to the user nothing at all. This is detected in the editor
        let blame_line = line.saturating_sub(inserted_lines) + removed_lines;
        let commit = self.lines.get(blame_line as usize)?.as_ref()?;
        Some(LineBlame {
            commit: Arc::clone(commit),
        })
    }
}

/// Get the id of the commit `HEAD` points to for the repository `file` is in.
///
/// This is cheap compared to [`FileBlame::try_new`] and can be used to check if a
/// [`FileBlame`] is stale by comparing against [`FileBlame::head`].
///
/// Fails with [`BlameError::NotInRepository`] outside of a repository and with
/// [`BlameError::NotCommitted`] when the repository has no commit yet.
pub fn head_commit_id(file: &Path) -> Result<CommitId, BlameError> {
    let file = resolve_path(file);
    let repo = open_repo_for_file(&file)?;
    head_id(&repo)
}

/// Resolve symlinks of the file like the rest of this crate does, but keep going for files
/// that don't exist (yet) as that is not an error to us: they are "not committed".
fn resolve_path(file: &Path) -> PathBuf {
    if let Ok(file) = gix::path::realpath(file) {
        return file;
    }
    match (file.parent(), file.file_name()) {
        (Some(dir), Some(name)) => gix::path::realpath(dir)
            .map(|dir| dir.join(name))
            .unwrap_or_else(|_| file.to_owned()),
        _ => file.to_owned(),
    }
}

fn open_repo_for_file(file: &Path) -> Result<gix::Repository, BlameError> {
    // The directory of the file might not exist if it is a new file. Discovery starts at an
    // existing directory.
    let mut dir = file.parent().ok_or(BlameError::NotInRepository)?;
    while !dir.exists() {
        dir = dir.parent().ok_or(BlameError::NotInRepository)?;
    }
    match open_repo(dir, false) {
        Ok(repo) => Ok(repo.to_thread_local()),
        Err(err) => {
            if matches!(
                err.downcast_ref::<gix::discover::upwards::Error>(),
                Some(
                    gix::discover::upwards::Error::NoGitRepository { .. }
                        | gix::discover::upwards::Error::NoGitRepositoryWithinCeiling { .. }
                        | gix::discover::upwards::Error::NoGitRepositoryWithinFs { .. }
                )
            ) {
                Err(BlameError::NotInRepository)
            } else {
                Err(BlameError::Other(err.context("failed to open git repo")))
            }
        }
    }
}

fn head_id(repo: &gix::Repository) -> Result<CommitId, BlameError> {
    let head = repo.head().context("failed to read HEAD")?;
    if head.is_unborn() {
        return Err(BlameError::NotCommitted);
    }
    let commit = head
        .into_peeled_id()
        .context("HEAD does not point to a commit")?;
    Ok(CommitId(commit.detach()))
}

/// Find `HEAD` and the path of `file` relative to the repository, checking that `HEAD` has it.
fn head_and_rel_path<'a>(
    repo: &gix::Repository,
    file: &'a Path,
) -> Result<(CommitId, &'a Path), BlameError> {
    let workdir = repo.workdir().ok_or(BlameError::NotInRepository)?;
    let rel_path = file
        .strip_prefix(workdir)
        .context("file is not inside of the working tree of its repository")?;
    let head = head_id(repo)?;
    let tree = repo
        .find_commit(head.0)
        .context("failed to find HEAD commit")?
        .tree()
        .context("failed to find HEAD tree")?;
    let entry = tree
        .lookup_entry_by_path(rel_path)
        .context("failed to look up file in HEAD tree")?
        .ok_or(BlameError::NotCommitted)?;
    match entry.mode().kind() {
        EntryKind::Blob | EntryKind::BlobExecutable => Ok((head, rel_path)),
        kind => Err(BlameError::Other(anyhow::anyhow!(
            "{} is not a file but a {kind:?}",
            file.display()
        ))),
    }
}

/// Collect what we want to show about `id`. Never fails: when the commit can't be read we are
/// left with its id.
fn resolve_commit(repo: &gix::Repository, id: gix::ObjectId) -> BlameCommit {
    let commit = repo.find_commit(id).ok();
    let message = commit.as_ref().and_then(|c| c.message().ok());
    let author = commit.as_ref().and_then(|c| c.author().ok());
    let time = author.and_then(|a| a.time.parse::<gix::date::Time>().ok());

    // `title` and `body` of a gix message keep their trailing newlines
    let trimmed = |text: &gix::bstr::BStr| {
        let text = text.trim_end().to_str_lossy();
        (!text.is_empty()).then(|| text.into_owned())
    };

    BlameCommit {
        id: CommitId(id),
        short_id: commit
            .as_ref()
            .and_then(|c| c.short_id().ok())
            .map_or_else(|| id.to_hex_with_len(7).to_string(), |id| id.to_string()),
        author_name: author.map(|a| a.name.to_string()),
        author_email: author.map(|a| a.email.to_string()),
        title: message.as_ref().and_then(|msg| trimmed(msg.title)),
        body: message.as_ref().and_then(|msg| msg.body.and_then(trimmed)),
        time: time.map(|time| (time.seconds, time.offset)),
        date: time.and_then(|time| time.format(gix::date::time::format::SHORT).ok()),
    }
}

/// The blame for a single line. Cheap to create and to clone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LineBlame {
    commit: Arc<BlameCommit>,
}

impl LineBlame {
    /// The commit that is blamed for the line.
    pub fn commit(&self) -> &BlameCommit {
        &self.commit
    }

    /// # Returns
    ///
    /// None => Invalid variable
    /// Some(None) => Valid variable, but is empty
    #[inline]
    fn get_variable(&self, var: &str) -> Option<Option<Cow<'_, str>>> {
        let commit = &*self.commit;
        fn borrowed(value: &Option<String>) -> Option<Cow<'_, str>> {
            value.as_deref().map(Cow::Borrowed)
        }
        Some(match var {
            // an empty id is treated as missing
            "commit" => Some(commit.short_id.as_str())
                .filter(|id| !id.is_empty())
                .map(Cow::Borrowed),
            "author" => borrowed(&commit.author_name),
            "date" => borrowed(&commit.date),
            "title" => borrowed(&commit.title),
            "email" => borrowed(&commit.author_email),
            "body" => borrowed(&commit.body),
            // The only one that changes over time, so it is computed every time.
            "time-ago" => commit.time.map(|(utc_seconds, timezone_offset)| {
                Cow::Owned(helix_stdx::time::format_relative_time(
                    utc_seconds,
                    timezone_offset,
                ))
            }),
            _ => return None,
        })
    }

    /// Parse the user's blame format
    ///
    /// Available variables are `{author}`, `{date}`, `{time-ago}`, `{title}`, `{body}`,
    /// `{commit}` and `{email}`. `{time-ago}` is relative to the time of this call.
    #[inline]
    pub fn parse_format(&self, format: &str) -> String {
        let mut line_blame = String::new();
        let mut content_before_variable = String::with_capacity(format.len());

        let mut chars = format.char_indices().peekable();
        // in all cases, when any of the variables is empty we exclude the content before the variable
        // However, if the variable is the first and it is empty - then exclude the content after the variable
        let mut exclude_content_after_variable = false;
        while let Some((ch_idx, ch)) = chars.next() {
            if ch == '{' {
                let mut variable = String::new();
                // eat all characters until the end
                while let Some((_, ch)) = chars.next_if(|(_, ch)| *ch != '}') {
                    variable.push(ch);
                }
                // eat the '}' if it was found
                let has_closing = chars.next().is_some();

                enum Variable<'a> {
                    Valid(Cow<'a, str>),
                    Invalid(Cow<'a, str>),
                    Empty,
                }

                let variable_value = self.get_variable(&variable).map_or_else(
                    || {
                        // Invalid variable. So just add whatever we parsed before.
                        // The length of the variable, including opening and optionally
                        // closing curly braces
                        let variable_len = 1 + variable.len() + has_closing as usize;

                        Variable::Invalid(Cow::Borrowed(&format[ch_idx..ch_idx + variable_len]))
                    },
                    |s| s.map(Variable::Valid).unwrap_or(Variable::Empty),
                );

                match variable_value {
                    Variable::Invalid(value) | Variable::Valid(value) => {
                        if exclude_content_after_variable {
                            // don't push anything.
                            exclude_content_after_variable = false;
                        } else {
                            line_blame.push_str(&content_before_variable);
                        }
                        line_blame.push_str(&value);
                    }
                    Variable::Empty => {
                        if line_blame.is_empty() {
                            // exclude content AFTER this variable (at next iteration of the loop,
                            // we'll exclude the content before a valid variable)
                            exclude_content_after_variable = true;
                        } else {
                            // exclude content BEFORE this variable
                            // also just don't add anything.
                        }
                    }
                }

                // we've consumed the content before the variable so just get rid of it and
                // make space for new
                content_before_variable.drain(..);
            } else {
                content_before_variable.push(ch);
            }
        }

        line_blame
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::git::test::{create_commit_with_message, empty_git_repo, exec_git_args};
    use std::fs::File;

    /// describes how a line was modified
    #[derive(PartialEq, PartialOrd, Ord, Eq)]
    enum LineDiff {
        /// this line is added
        Insert,
        /// this line is deleted
        Delete,
        /// no changes for this line
        None,
    }

    /// checks if the first argument is `no_commit` or not
    macro_rules! no_commit_flag {
        (no_commit, $commit_msg:literal) => {
            false
        };
        (, $commit_msg:literal) => {
            true
        };
        ($any:tt, $commit_msg:literal) => {
            compile_error!(concat!(
                "expected `no_commit` or nothing for commit ",
                $commit_msg
            ))
        };
    }

    /// checks if the first argument is `insert` or `delete`
    macro_rules! line_diff_flag {
        (insert, $commit_msg:literal, $line:expr) => {
            LineDiff::Insert
        };
        (delete, $commit_msg:literal, $line:expr) => {
            LineDiff::Delete
        };
        (, $commit_msg:literal, $line:expr) => {
            LineDiff::None
        };
        ($any:tt, $commit_msg:literal, $line:expr) => {
            compile_error!(concat!(
                "expected `insert`, `delete` or nothing for commit ",
                $commit_msg,
                " line ",
                $line
            ))
        };
    }

    /// This macro exists because we can't pass a `match` statement into `concat!`
    /// we would like to exclude any lines that are `delete`
    macro_rules! line_diff_flag_str {
        (insert, $commit_msg:literal, $line:expr) => {
            concat!($line, newline_literal!())
        };
        (delete, $commit_msg:literal, $line:expr) => {
            ""
        };
        (, $commit_msg:literal, $line:expr) => {
            concat!($line, newline_literal!())
        };
        ($any:tt, $commit_msg:literal, $line:expr) => {
            compile_error!(concat!(
                "expected `insert`, `delete` or nothing for commit ",
                $commit_msg,
                " line ",
                $line
            ))
        };
    }

    #[cfg(windows)]
    macro_rules! newline_literal {
        () => {
            "\r\n"
        };
    }
    #[cfg(not(windows))]
    macro_rules! newline_literal {
        () => {
            "\n"
        };
    }

    /// Helper macro to create a history of the same file being modified.
    macro_rules! assert_line_blame_progress {
        (
            $(
                // a unique identifier for the commit, other commits must not use this
                // If `no_commit` option is used, use the identifier of the previous commit
                $commit_msg:literal
                // must be `no_commit` if exists.
                // If exists, this block won't be committed
                $($no_commit:ident)? =>
                $(
                    // contents of a line in the file
                    $line:literal
                    // what commit identifier we are expecting for this line
                    $($expected:literal)?
                    // must be `insert` or `delete` if exists
                    // if exists, must be used with `no_commit`
                    // - `insert`: this line is added
                    // - `delete`: this line is deleted
                    $($line_diff:ident)?
                ),+
            );+
            $(;)?
        ) => {{
            use std::fs::OpenOptions;
            use std::io::Write;

            let repo = empty_git_repo();
            let file = repo.path().join("file.txt");
            File::create(&file).expect("could not create file");

            $(
                let file_content = concat!(
                    $(
                        line_diff_flag_str!($($line_diff)?, $commit_msg, $line),
                    )*
                );
                eprintln!("at commit {}:\n\n{file_content}", stringify!($commit_msg));

                let mut f = OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&file)
                    .unwrap();

                f.write_all(file_content.as_bytes()).unwrap();

                let should_commit = no_commit_flag!($($no_commit)?, $commit_msg);
                if should_commit {
                    create_commit_with_message(repo.path(), true, stringify!($commit_msg));
                }

                // the blame is of what is committed, i.e. HEAD
                let blame = FileBlame::try_new(&file).unwrap();

                let mut line_number = 0;
                let mut added_lines = 0;
                let mut removed_lines = 0;

                $(
                    let line_diff_flag = line_diff_flag!($($line_diff)?, $commit_msg, $line);
                    #[allow(unused_assignments)]
                    match line_diff_flag {
                        LineDiff::Insert => added_lines += 1,
                        LineDiff::Delete => removed_lines += 1,
                        LineDiff::None => ()
                    }
                    // completely skip lines that are marked as `delete`
                    if line_diff_flag != LineDiff::Delete {
                        // if there is no $expected, then we don't care what blame_line returns
                        // because we won't show it to the user.
                        $(
                            let blame_result = blame
                                .blame_for_line(line_number, added_lines, removed_lines)
                                .and_then(|line_blame| line_blame.commit().title.clone());

                            assert_eq!(
                                blame_result,
                                Some(stringify!($expected).to_owned()),
                                "Blame mismatch\nat commit: {}\nat line: {}\nline contents: {}\nexpected commit: {}\nbut got commit: {}",
                                $commit_msg,
                                line_number,
                                file_content
                                    .lines()
                                    .nth(line_number.try_into().unwrap())
                                    .unwrap(),
                                stringify!($expected),
                                blame_result
                                    .as_ref()
                                    .map(|blame| blame.trim_end())
                                    .unwrap_or("<no commit>")
                            );
                        )?
                        #[allow(unused_assignments)]
                        {
                            line_number += 1;
                        }
                    }
                )*
            )*
        }};
    }

    // For some reasons the CI is failing on windows with the message "Commits not found".
    // The created temporary repository has no commits... But this is not an issue on unix.
    // There is nothing platform-specific in this implementation. This is a problem only
    // for tests on Windows.
    // As such it should be fine to disable this test in Windows.
    // As long as these tests pass on other platforms, on Windows it will work too.
    #[cfg(not(windows))]
    #[test]
    pub fn blamed_lines() {
        assert_line_blame_progress! {
            // initialize
            1 =>
                "fn main() {" 1,
                "" 1,
                "}" 1;
            // modifying a line works
            2 =>
                "fn main() {" 1,
                "  one" 2,
                "}" 1;
            // inserting a line works
            3 =>
                "fn main() {" 1,
                "  one" 2,
                "  two" 3,
                "}" 1;
            // deleting a line works
            4 =>
                "fn main() {" 1,
                "  two" 3,
                "}" 1;
            // when a line is inserted in-between the blame order is preserved
            4 no_commit =>
                "fn main() {" 1,
                "  hello world" insert,
                "  two" 3,
                "}" 1;
            // Having a bunch of random lines interspersed should not change which lines
            // have blame for which commits
            4 no_commit =>
                "  six" insert,
                "  three" insert,
                "fn main() {" 1,
                "  five" insert,
                "  four" insert,
                "  two" 3,
                "  five" insert,
                "  four" insert,
                "}" 1,
                "  five" insert,
                "  four" insert;
            // committing all of those insertions should recognize that they are
            // from the current commit, while still keeping the information about
            // previous commits
            5 =>
                "  six" 5,
                "  three" 5,
                "fn main() {" 1,
                "  five" 5,
                "  four" 5,
                "  two" 3,
                "  five" 5,
                "  four" 5,
                "}" 1,
                "  five" 5,
                "  four" 5;
            // several lines deleted
            5 no_commit =>
                "  six" 5,
                "  three" 5,
                "fn main() {" delete,
                "  five" delete,
                "  four" delete,
                "  two" delete,
                "  five" delete,
                "  four" 5,
                "}" 1,
                "  five" 5,
                "  four" 5;
            // committing the deleted changes
            6 =>
                "  six" 5,
                "  three" 5,
                "  four" 5,
                "}" 1,
                "  five" 5,
                "  four" 5;
            // mixing inserts with deletes
            6 no_commit =>
                "  six" delete,
                "  2" insert,
                "  three" delete,
                "  four" 5,
                "  1" insert,
                "}" 1,
                "]" insert,
                "  five" delete,
                "  four" 5;
            // committing inserts and deletes
            7 =>
                "  2" 7,
                "  four" 5,
                "  1" 7,
                "}" 1,
                "]" 7,
                "  four" 5;
        };
    }

    fn bob_commit() -> BlameCommit {
        BlameCommit {
            id: CommitId(gix::ObjectId::empty_tree(gix::hash::Kind::Sha1)),
            short_id: "f14ab1cf".to_owned(),
            author_name: Some("Bob TheBuilder".to_owned()),
            author_email: Some("bob@bob.com".to_owned()),
            date: Some("2028-01-10".to_owned()),
            title: Some("feat!: extend house".to_owned()),
            body: Some("BREAKING CHANGE: Removed door".to_owned()),
            time: None,
        }
    }

    fn line_blame(commit: BlameCommit) -> LineBlame {
        LineBlame {
            commit: Arc::new(commit),
        }
    }

    fn bob() -> LineBlame {
        line_blame(bob_commit())
    }

    #[test]
    pub fn inline_blame_format_parser() {
        let format = "{author}, {date} • {title} • {commit}";

        assert_eq!(
            bob().parse_format(format),
            "Bob TheBuilder, 2028-01-10 • feat!: extend house • f14ab1cf".to_owned()
        );
        assert_eq!(
            line_blame(BlameCommit {
                author_name: None,
                ..bob_commit()
            })
            .parse_format(format),
            "2028-01-10 • feat!: extend house • f14ab1cf".to_owned()
        );
        assert_eq!(
            line_blame(BlameCommit {
                date: None,
                ..bob_commit()
            })
            .parse_format(format),
            "Bob TheBuilder • feat!: extend house • f14ab1cf".to_owned()
        );
        assert_eq!(
            line_blame(BlameCommit {
                title: None,
                author_email: None,
                ..bob_commit()
            })
            .parse_format(format),
            "Bob TheBuilder, 2028-01-10 • f14ab1cf".to_owned()
        );
        assert_eq!(
            line_blame(BlameCommit {
                short_id: String::new(),
                ..bob_commit()
            })
            .parse_format(format),
            "Bob TheBuilder, 2028-01-10 • feat!: extend house".to_owned()
        );
        assert_eq!(
            line_blame(BlameCommit {
                date: None,
                author_name: None,
                ..bob_commit()
            })
            .parse_format(format),
            "feat!: extend house • f14ab1cf".to_owned()
        );
        assert_eq!(
            line_blame(BlameCommit {
                author_name: None,
                title: None,
                ..bob_commit()
            })
            .parse_format(format),
            "2028-01-10 • f14ab1cf".to_owned()
        );
    }

    #[test]
    fn inline_blame_format_other_variables() {
        let format = "{email} {body} {nope} {foo";
        assert_eq!(
            bob().parse_format(format),
            "bob@bob.com BREAKING CHANGE: Removed door {nope} {foo".to_owned()
        );
        // no time known: the variable is empty so is the content before it
        assert_eq!(
            bob().parse_format("{author} {time-ago}"),
            "Bob TheBuilder".to_owned()
        );
        // time-ago is computed when formatting, not when blaming
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let blame = line_blame(BlameCommit {
            time: Some((now - 3 * 24 * 60 * 60 - 60, 3600)),
            ..bob_commit()
        });
        assert_eq!(
            blame.parse_format("{author}, {time-ago}"),
            "Bob TheBuilder, 3 days ago".to_owned()
        );
    }

    fn assert_send_sync<T: Send + Sync + std::fmt::Debug>() {}

    #[test]
    fn file_blame_is_send_sync() {
        assert_send_sync::<FileBlame>();
        assert_send_sync::<LineBlame>();
        assert_send_sync::<BlameCommit>();
        assert_send_sync::<CommitId>();
    }

    #[test]
    fn commit_details() {
        let repo = empty_git_repo();
        let file = repo.path().join("file.txt");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        create_commit_with_message(repo.path(), true, "the title\n\nfirst paragraph\n\nsecond");
        std::fs::write(&file, "one\nTWO\nthree\n").unwrap();
        create_commit_with_message(repo.path(), true, "only a title");

        let blame = FileBlame::try_new(&file).unwrap();
        assert_eq!(blame.len(), 3);
        assert_eq!(blame.head(), head_commit_id(&file).unwrap());

        let first = blame.blame_for_line(0, 0, 0).unwrap();
        let first = first.commit();
        assert_eq!(first.title.as_deref(), Some("the title"));
        assert_eq!(first.body.as_deref(), Some("first paragraph\n\nsecond"));
        assert_eq!(first.author_name.as_deref(), Some("author"));
        assert_eq!(first.author_email.as_deref(), Some("author@example.com"));
        // GIT_AUTHOR_DATE in `git::test::exec_git_cmd`
        assert_eq!(first.date.as_deref(), Some("2000-01-01"));
        assert_eq!(first.time, Some((946684800, 0)));
        assert_eq!(first.short_id.len(), 7);
        assert!(first.id.to_string().starts_with(&first.short_id));
        assert_eq!(first.id.to_string().len(), 40);

        let second = blame.blame_for_line(1, 0, 0).unwrap();
        assert_eq!(second.commit().title.as_deref(), Some("only a title"));
        assert_eq!(second.commit().body, None);
        assert_eq!(
            second.parse_format("{title} by {author} ({commit}"),
            format!("only a title by author ({}", second.commit().short_id)
        );

        // lines of the same commit share the same allocation
        let third = blame.blame_for_line(2, 0, 0).unwrap();
        assert!(Arc::ptr_eq(&first_arc(&blame, 0), &first_arc(&blame, 2)));
        assert_eq!(third.commit(), first);
    }

    fn first_arc(blame: &FileBlame, line: u32) -> Arc<BlameCommit> {
        blame.blame_for_line(line, 0, 0).unwrap().commit
    }

    #[test]
    fn lines_out_of_range_have_no_blame() {
        let repo = empty_git_repo();
        let file = repo.path().join("file.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        create_commit_with_message(repo.path(), true, "init");

        let blame = FileBlame::try_new(&file).unwrap();
        assert!(blame.blame_for_line(1, 0, 0).is_some());
        assert!(blame.blame_for_line(2, 0, 0).is_none());
        assert!(blame.blame_for_line(u32::MAX, 0, 0).is_none());
        // three lines were added before the second line, which makes it the first of HEAD
        assert!(blame.blame_for_line(1, 3, 0).is_some());
        // lines removed before the first line
        assert!(blame.blame_for_line(0, 0, 1).is_some());
        assert!(blame.blame_for_line(0, 0, 2).is_none());
    }

    #[test]
    fn empty_file_has_no_lines() {
        let repo = empty_git_repo();
        let file = repo.path().join("file.txt");
        File::create(&file).unwrap();
        create_commit_with_message(repo.path(), true, "init");

        let blame = FileBlame::try_new(&file).unwrap();
        assert!(blame.is_empty());
        assert!(blame.blame_for_line(0, 0, 0).is_none());
    }

    #[test]
    fn file_in_subdirectory_and_deleted_from_worktree() {
        let repo = empty_git_repo();
        let dir = repo.path().join("a").join("b");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file.txt");
        std::fs::write(&file, "one\n").unwrap();
        create_commit_with_message(repo.path(), true, "init");

        assert!(FileBlame::try_new(&file).is_ok());
        // blame is of HEAD so the state of the working tree does not matter
        std::fs::remove_file(&file).unwrap();
        assert!(FileBlame::try_new(&file).is_ok());
    }

    #[test]
    fn untracked_file_is_not_committed() {
        let repo = empty_git_repo();
        std::fs::write(repo.path().join("tracked.txt"), "one\n").unwrap();
        create_commit_with_message(repo.path(), true, "init");
        let file = repo.path().join("untracked.txt");
        std::fs::write(&file, "two\n").unwrap();

        assert!(matches!(
            FileBlame::try_new(&file),
            Err(BlameError::NotCommitted)
        ));
        // not even staged
        exec_git_args(&["add", "untracked.txt"], repo.path());
        assert!(matches!(
            FileBlame::try_new(&file),
            Err(BlameError::NotCommitted)
        ));
        // a file that was never saved, in a directory that does not exist
        assert!(matches!(
            FileBlame::try_new(&repo.path().join("new-dir").join("new.txt")),
            Err(BlameError::NotCommitted)
        ));
    }

    #[test]
    fn file_outside_of_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "one\n").unwrap();

        assert!(matches!(
            FileBlame::try_new(&file),
            Err(BlameError::NotInRepository)
        ));
        assert!(matches!(
            head_commit_id(&file),
            Err(BlameError::NotInRepository)
        ));
        assert!(matches!(
            FileBlame::try_new(&dir.path().join("missing").join("file.txt")),
            Err(BlameError::NotInRepository)
        ));
    }

    #[test]
    fn repository_without_commits() {
        let repo = empty_git_repo();
        let file = repo.path().join("file.txt");
        std::fs::write(&file, "one\n").unwrap();
        exec_git_args(&["add", "file.txt"], repo.path());

        assert!(matches!(
            FileBlame::try_new(&file),
            Err(BlameError::NotCommitted)
        ));
        assert!(matches!(
            head_commit_id(&file),
            Err(BlameError::NotCommitted)
        ));
    }

    #[test]
    fn head_commit_id_follows_new_commits() {
        let repo = empty_git_repo();
        let file = repo.path().join("file.txt");
        std::fs::write(&file, "one\n").unwrap();
        create_commit_with_message(repo.path(), true, "first");

        let blame = FileBlame::try_new(&file).unwrap();
        let first = head_commit_id(&file).unwrap();
        assert_eq!(blame.head(), first);
        // calling it again does not change anything
        assert_eq!(head_commit_id(&file).unwrap(), first);

        std::fs::write(&file, "one\ntwo\n").unwrap();
        create_commit_with_message(repo.path(), true, "second");
        let second = head_commit_id(&file).unwrap();
        assert_ne!(first, second);
        // the old blame still reports the commit it was computed for
        assert_eq!(blame.head(), first);
        assert_ne!(blame.head(), head_commit_id(&file).unwrap());

        let blame = FileBlame::try_new(&file).unwrap();
        assert_eq!(blame.head(), second);
        assert_eq!(blame.len(), 2);
    }

    #[test]
    fn blame_follows_renames() {
        let repo = empty_git_repo();
        std::fs::write(repo.path().join("old.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        create_commit_with_message(repo.path(), true, "add");
        exec_git_args(&["mv", "old.txt", "new.txt"], repo.path());
        create_commit_with_message(repo.path(), false, "rename");

        let blame = FileBlame::try_new(&repo.path().join("new.txt")).unwrap();
        for line in 0..4 {
            let line_blame = blame.blame_for_line(line, 0, 0).unwrap();
            assert_eq!(line_blame.commit().title.as_deref(), Some("add"));
        }
    }
}
