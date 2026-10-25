//! Stand-ins for the types of `git::blame` for builds without the `git` feature, so that
//! users of this crate don't need `cfg`s of their own. Nothing can be blamed without git:
//! every request fails with [`BlameError::NotInRepository`].

use std::fmt;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug)]
pub enum BlameError {
    /// The file is not inside of a git repository (or the repository has no working tree).
    NotInRepository,
    /// The file is inside of a git repository but there is no committed version of it.
    NotCommitted,
    /// Anything else.
    Other(anyhow::Error),
}

impl fmt::Display for BlameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInRepository => f.write_str("blame is not supported in this build"),
            Self::NotCommitted => f.write_str("file is not committed"),
            Self::Other(err) => write!(f, "{err:#}"),
        }
    }
}

impl std::error::Error for BlameError {}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct CommitId(());

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("0000000")
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlameCommit {
    pub id: CommitId,
    pub short_id: String,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub time: Option<(i64, i32)>,
    pub date: Option<String>,
}

#[derive(Debug)]
pub struct FileBlame {
    head: CommitId,
}

impl FileBlame {
    pub fn try_new(_file: &Path) -> Result<Self, BlameError> {
        Err(BlameError::NotInRepository)
    }

    pub fn head(&self) -> CommitId {
        self.head
    }

    pub fn len(&self) -> usize {
        0
    }

    pub fn is_empty(&self) -> bool {
        true
    }

    pub fn blame_for_line(
        &self,
        _line: u32,
        _inserted_lines: u32,
        _removed_lines: u32,
    ) -> Option<LineBlame> {
        None
    }
}

pub fn head_commit_id(_file: &Path) -> Result<CommitId, BlameError> {
    Err(BlameError::NotInRepository)
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LineBlame {
    commit: Arc<BlameCommit>,
}

impl LineBlame {
    pub fn commit(&self) -> &BlameCommit {
        &self.commit
    }

    pub fn parse_format(&self, _format: &str) -> String {
        String::new()
    }
}
