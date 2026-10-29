/// A commit, as listed by the commit log picker.
#[derive(Clone)]
pub struct CommitInfo {
    /// Full hex id.
    pub id: String,
    pub short_id: String,
    /// First line of the message.
    pub summary: String,
    pub author: String,
    /// Author date, `YYYY-MM-DD`.
    pub date: String,
}

/// A row of the commit log: a commit, or the divider that starts a section of the log (the
/// commits of a branch).
#[derive(Clone)]
pub enum LogEntry {
    /// The label of the section that follows.
    Divider(String),
    Commit(CommitInfo),
}
