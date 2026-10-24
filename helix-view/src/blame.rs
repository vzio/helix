//! Per-document state of inline git blame (fork feature).
//!
//! Computing the blame of a file is expensive, so it happens in the background (see
//! `helix-term/src/handlers/blame.rs`) and the result is cached in [`DocumentBlame`]. Looking
//! up a line is cheap and happens on every render: [`Document::line_blame`] translates the line
//! of the buffer into a line of the file at `HEAD` using the hunks of the document's diff
//! (the diff is buffer against `HEAD`, so it accounts for unsaved edits too).

use std::borrow::Borrow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use helix_vcs::{CommitId, FileBlame, Hunk, LineBlame};

use crate::{Document, ViewId};

/// What is shown for lines that are not in `HEAD`.
pub const NOT_COMMITTED_YET: &str = "Not committed yet";

/// What we know about the blame of a document.
#[derive(Debug, Clone, Default)]
pub enum BlameState {
    /// Nothing has been requested yet (or the feature was turned off).
    #[default]
    Unknown,
    /// The first computation is in flight.
    Pending,
    /// Blame as of `FileBlame::head`. Kept (and shown) while a refresh is in flight.
    Ready(Arc<FileBlame>),
    /// The file is in a repository but not in `HEAD`: every line is "not committed yet".
    NotCommitted,
    /// No blame can be shown: the file is not in a repository or something went wrong.
    Unavailable,
}

/// What a background computation found out.
#[derive(Debug)]
pub enum BlameOutcome {
    /// `HEAD` is still the commit the cached blame was computed for.
    Unchanged,
    Ready(FileBlame),
    NotCommitted,
    Unavailable,
}

/// What a background computation needs to know. Handed out by [`DocumentBlame::begin`].
#[derive(Debug)]
pub struct BlameRequest {
    /// Hand this back to [`DocumentBlame::finish`].
    pub generation: u64,
    pub path: PathBuf,
    /// The `HEAD` the cached blame is for. When `HEAD` is still this commit the expensive
    /// computation can be skipped.
    pub cached_head: Option<CommitId>,
}

/// The result of [`DocumentBlame::finish`].
#[derive(Debug, PartialEq, Eq)]
pub enum Finished {
    /// The request was made obsolete (see [`DocumentBlame::reset`]) and its result dropped.
    Stale,
    /// The result is in. `rerun` says that more requests were made while it was computed, which
    /// should now be answered with one new request.
    Applied { rerun: bool },
}

/// The blame state of one document: the cached result, bookkeeping to run at most one
/// computation at a time, and the lines to show blame for in cursor-line mode.
#[derive(Debug, Default)]
pub struct DocumentBlame {
    state: BlameState,
    /// The path the state is for. A document that is saved elsewhere starts over.
    path: Option<PathBuf>,
    /// Incremented by `reset`; results of requests from before it are dropped.
    generation: u64,
    in_flight: bool,
    /// A request was made while one was in flight.
    rerun: bool,
    /// The line the cursor of a view stayed on for the configured delay.
    ready_lines: HashMap<ViewId, usize>,
    /// Show the blame of this line in the statusline once the state is known (`blame_line`).
    status_line: Option<usize>,
}

impl DocumentBlame {
    pub fn state(&self) -> &BlameState {
        &self.state
    }

    /// Whether a computation is in flight.
    pub fn is_in_flight(&self) -> bool {
        self.in_flight
    }

    /// Forget everything and make results of requests in flight obsolete.
    pub fn reset(&mut self) {
        self.state = BlameState::Unknown;
        self.path = None;
        self.generation += 1;
        self.in_flight = false;
        self.rerun = false;
        self.ready_lines.clear();
        self.status_line = None;
    }

    /// Start a request for the file at `path`.
    ///
    /// Returns `None` when a computation is already in flight: the request is remembered and
    /// [`finish`](Self::finish) reports that a new one should be made.
    pub fn begin(&mut self, path: &Path) -> Option<BlameRequest> {
        if self.path.as_deref() != Some(path) {
            self.reset();
            self.path = Some(path.to_owned());
        }
        if self.in_flight {
            self.rerun = true;
            return None;
        }
        self.in_flight = true;
        if matches!(self.state, BlameState::Unknown) {
            self.state = BlameState::Pending;
        }
        Some(BlameRequest {
            generation: self.generation,
            path: path.to_owned(),
            cached_head: match &self.state {
                BlameState::Ready(blame) => Some(blame.head()),
                _ => None,
            },
        })
    }

    /// Apply the result of the request that got `generation` from [`begin`](Self::begin).
    pub fn finish(&mut self, generation: u64, outcome: BlameOutcome) -> Finished {
        if generation != self.generation || !self.in_flight {
            return Finished::Stale;
        }
        self.in_flight = false;
        match outcome {
            BlameOutcome::Unchanged => (),
            BlameOutcome::Ready(blame) => self.state = BlameState::Ready(Arc::new(blame)),
            BlameOutcome::NotCommitted => self.state = BlameState::NotCommitted,
            BlameOutcome::Unavailable => self.state = BlameState::Unavailable,
        }
        Finished::Applied {
            rerun: std::mem::take(&mut self.rerun),
        }
    }

    /// The line the cursor of `view` stayed on for the configured delay.
    pub fn ready_line(&self, view: ViewId) -> Option<usize> {
        self.ready_lines.get(&view).copied()
    }

    pub fn set_ready_line(&mut self, view: ViewId, line: usize) {
        self.ready_lines.insert(view, line);
    }

    pub fn clear_ready_line(&mut self, view: ViewId) {
        self.ready_lines.remove(&view);
    }

    /// Ask for the blame of `line` to be shown in the statusline once it is known.
    pub fn set_status_line(&mut self, line: Option<usize>) {
        self.status_line = line;
    }

    pub fn take_status_line(&mut self) -> Option<usize> {
        self.status_line.take()
    }
}

/// What to show for a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineBlameResult<T = LineBlame> {
    Blame(T),
    /// The line is new or edited compared to `HEAD`.
    NotCommittedYet,
    /// There is nothing to show, now or yet.
    Nothing,
}

impl LineBlameResult {
    /// The text to show, with the blame formatted with `format`.
    pub fn display(&self, format: &str) -> Option<String> {
        match self {
            Self::Blame(blame) => Some(blame.parse_format(format)).filter(|text| !text.is_empty()),
            Self::NotCommittedYet => Some(NOT_COMMITTED_YET.to_owned()),
            Self::Nothing => None,
        }
    }
}

/// Resolve what to show for `line`, a 0-based line of the buffer.
///
/// `hunks` are the hunks of the diff of the buffer against `HEAD`, ascending. Lines in the
/// `after` range of a hunk are edited or added, so they are not committed yet. For any other
/// line the blame of the file at `HEAD` is looked up with `lookup`, which is given the number of
/// lines inserted and removed before `line` to translate the line number.
pub fn resolve_line<T, H: Borrow<Hunk>>(
    hunks: impl IntoIterator<Item = H>,
    line: u32,
    lookup: impl FnOnce(u32, u32) -> Option<T>,
) -> LineBlameResult<T> {
    let (mut inserted, mut removed) = (0u32, 0u32);
    for hunk in hunks {
        let hunk = hunk.borrow();
        if hunk.after.start > line {
            break;
        }
        if hunk.after.end > line {
            return LineBlameResult::NotCommittedYet;
        }
        inserted = inserted.saturating_add(hunk.after.end - hunk.after.start);
        removed = removed.saturating_add(hunk.before.end - hunk.before.start);
    }
    match lookup(inserted, removed) {
        Some(blame) => LineBlameResult::Blame(blame),
        None => LineBlameResult::Nothing,
    }
}

impl Document {
    /// What to show as inline blame for `line`, a 0-based line of the buffer.
    ///
    /// Never blocks and never touches the disk. Gives [`LineBlameResult::Nothing`] while the
    /// diff is not available or not up to date with the buffer, so that no wrong blame (or
    /// "not committed yet") is shown for a moment.
    pub fn line_blame(&self, line: usize) -> LineBlameResult {
        let blame = match self.blame.state() {
            BlameState::Unknown | BlameState::Pending | BlameState::Unavailable => {
                return LineBlameResult::Nothing
            }
            BlameState::NotCommitted => return LineBlameResult::NotCommittedYet,
            BlameState::Ready(blame) => blame,
        };
        let Some(diff) = self.diff_handle().and_then(|handle| handle.try_load()) else {
            return LineBlameResult::Nothing;
        };
        // The diff is updated asynchronously after an edit. A cheap check that catches most
        // of the cases where it is behind.
        if line >= self.text().len_lines() || diff.doc().len_chars() != self.text().len_chars() {
            return LineBlameResult::Nothing;
        }
        let line = line as u32;
        resolve_line(
            diff.hunks_intersecting_line_ranges(std::iter::once((0, line as usize))),
            line,
            |inserted, removed| blame.blame_for_line(line, inserted, removed),
        )
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::editor::{Config, InlineBlameShow};

    fn hunk(before: std::ops::Range<u32>, after: std::ops::Range<u32>) -> Hunk {
        Hunk { before, after }
    }

    /// Resolve with a lookup that reports the arguments it got.
    fn resolve(hunks: &[Hunk], line: u32) -> LineBlameResult<(u32, u32)> {
        resolve_line(hunks, line, |inserted, removed| Some((inserted, removed)))
    }

    #[test]
    fn no_hunks_means_no_translation() {
        assert_eq!(resolve(&[], 0), LineBlameResult::Blame((0, 0)));
        assert_eq!(resolve(&[], 41), LineBlameResult::Blame((0, 0)));
    }

    #[test]
    fn lines_in_the_after_range_of_a_hunk_are_not_committed() {
        // lines 2 and 3 were added
        let hunks = [hunk(2..2, 2..4)];
        assert_eq!(resolve(&hunks, 1), LineBlameResult::Blame((0, 0)));
        assert_eq!(resolve(&hunks, 2), LineBlameResult::NotCommittedYet);
        assert_eq!(resolve(&hunks, 3), LineBlameResult::NotCommittedYet);
        assert_eq!(resolve(&hunks, 4), LineBlameResult::Blame((2, 0)));
    }

    #[test]
    fn edited_lines_are_not_committed() {
        // line 5 was replaced by two lines
        let hunks = [hunk(5..6, 5..7)];
        assert_eq!(resolve(&hunks, 4), LineBlameResult::Blame((0, 0)));
        assert_eq!(resolve(&hunks, 5), LineBlameResult::NotCommittedYet);
        assert_eq!(resolve(&hunks, 6), LineBlameResult::NotCommittedYet);
        // 2 inserted, 1 removed
        assert_eq!(resolve(&hunks, 7), LineBlameResult::Blame((2, 1)));
    }

    #[test]
    fn removed_lines_shift_the_lines_after_them() {
        // lines 3, 4 and 5 of HEAD were deleted: nothing in the buffer is new
        let hunks = [hunk(3..6, 3..3)];
        assert_eq!(resolve(&hunks, 2), LineBlameResult::Blame((0, 0)));
        // the line right after the deletion is committed, and is line 6 in HEAD
        assert_eq!(resolve(&hunks, 3), LineBlameResult::Blame((0, 3)));
        assert_eq!(resolve(&hunks, 10), LineBlameResult::Blame((0, 3)));
    }

    #[test]
    fn deletion_at_the_start_of_the_file() {
        let hunks = [hunk(0..1, 0..0)];
        assert_eq!(resolve(&hunks, 0), LineBlameResult::Blame((0, 1)));
    }

    #[test]
    fn several_hunks_accumulate() {
        let hunks = [
            hunk(1..1, 1..3),    // +2
            hunk(5..8, 7..7),    // -3
            hunk(10..11, 9..12), // line 10 -> lines 9..12 (+3, -1)
        ];
        assert_eq!(resolve(&hunks, 0), LineBlameResult::Blame((0, 0)));
        assert_eq!(resolve(&hunks, 1), LineBlameResult::NotCommittedYet);
        assert_eq!(resolve(&hunks, 3), LineBlameResult::Blame((2, 0)));
        assert_eq!(resolve(&hunks, 6), LineBlameResult::Blame((2, 0)));
        assert_eq!(resolve(&hunks, 7), LineBlameResult::Blame((2, 3)));
        assert_eq!(resolve(&hunks, 8), LineBlameResult::Blame((2, 3)));
        assert_eq!(resolve(&hunks, 9), LineBlameResult::NotCommittedYet);
        assert_eq!(resolve(&hunks, 11), LineBlameResult::NotCommittedYet);
        assert_eq!(resolve(&hunks, 12), LineBlameResult::Blame((5, 4)));
    }

    #[test]
    fn owned_hunks_work_too() {
        let hunks = vec![hunk(0..0, 0..1)];
        assert_eq!(
            resolve_line(hunks, 0, |_, _| Some(())),
            LineBlameResult::NotCommittedYet
        );
    }

    #[test]
    fn lookup_without_result_is_nothing() {
        assert_eq!(
            resolve_line(&[] as &[Hunk], 0, |_, _| None::<()>),
            LineBlameResult::Nothing
        );
        // not even asked for lines that are not committed
        assert_eq!(
            resolve_line(&[hunk(0..0, 0..1)], 0, |_, _| -> Option<()> { panic!() }),
            LineBlameResult::NotCommittedYet
        );
    }

    #[test]
    fn display() {
        assert_eq!(
            LineBlameResult::NotCommittedYet
                .display("{author}")
                .as_deref(),
            Some("Not committed yet")
        );
        assert_eq!(LineBlameResult::Nothing.display("{author}"), None);
    }

    #[test]
    fn requests_are_coalesced() {
        let mut blame = DocumentBlame::default();
        let path = Path::new("/repo/file.rs");

        let request = blame.begin(path).expect("first request starts");
        assert!(matches!(blame.state(), BlameState::Pending));
        assert!(request.cached_head.is_none());
        assert_eq!(request.path, path);
        assert!(blame.is_in_flight());

        // any number of requests in the meantime result in one rerun
        assert!(blame.begin(path).is_none());
        assert!(blame.begin(path).is_none());
        assert_eq!(
            blame.finish(request.generation, BlameOutcome::NotCommitted),
            Finished::Applied { rerun: true }
        );
        assert!(matches!(blame.state(), BlameState::NotCommitted));
        assert!(!blame.is_in_flight());

        let request = blame.begin(path).expect("the rerun starts");
        assert_eq!(
            blame.finish(request.generation, BlameOutcome::Unavailable),
            Finished::Applied { rerun: false }
        );
        assert!(matches!(blame.state(), BlameState::Unavailable));
    }

    #[test]
    fn reset_drops_results_in_flight() {
        let mut blame = DocumentBlame::default();
        let path = Path::new("/repo/file.rs");
        let old = blame.begin(path).unwrap();
        blame.reset();
        assert!(matches!(blame.state(), BlameState::Unknown));
        assert!(!blame.is_in_flight());

        let new = blame.begin(path).unwrap();
        assert_ne!(old.generation, new.generation);
        // the old result neither applies nor ends the new request
        assert_eq!(
            blame.finish(old.generation, BlameOutcome::Unavailable),
            Finished::Stale
        );
        assert!(blame.is_in_flight());
        assert!(matches!(blame.state(), BlameState::Pending));
        assert_eq!(
            blame.finish(new.generation, BlameOutcome::NotCommitted),
            Finished::Applied { rerun: false }
        );
    }

    #[test]
    fn a_new_path_starts_over() {
        let mut blame = DocumentBlame::default();
        let old = blame.begin(Path::new("/repo/a.rs")).unwrap();
        // saved as another file while computing: not coalesced into the request in flight
        let new = blame
            .begin(Path::new("/repo/b.rs"))
            .expect("a different file is a different request");
        assert_eq!(
            blame.finish(old.generation, BlameOutcome::NotCommitted),
            Finished::Stale
        );
        assert_eq!(new.path, Path::new("/repo/b.rs"));
        assert!(blame.is_in_flight());
    }

    #[test]
    fn ready_lines_are_per_view() {
        let mut views = slotmap::SlotMap::<ViewId, ()>::with_key();
        let (a, b) = (views.insert(()), views.insert(()));
        let mut blame = DocumentBlame::default();
        assert_eq!(blame.ready_line(a), None);
        blame.set_ready_line(a, 3);
        blame.set_ready_line(b, 7);
        assert_eq!(blame.ready_line(a), Some(3));
        blame.clear_ready_line(a);
        assert_eq!(blame.ready_line(a), None);
        assert_eq!(blame.ready_line(b), Some(7));
        blame.reset();
        assert_eq!(blame.ready_line(b), None);
    }

    #[test]
    fn config_defaults() {
        let config = Config::default().inline_blame;
        assert!(!config.enable);
        assert_eq!(config.show, InlineBlameShow::CursorLine);
        assert_eq!(config.format, "{author}, {time-ago} • {title}");
        assert_eq!(config.delay, 300);
        assert_eq!(toml::from_str::<Config>("").unwrap().inline_blame, config);
    }

    #[test]
    fn config_deserialization() {
        let config: Config = toml::from_str(
            r#"
            [inline-blame]
            enable = true
            show = "all-lines"
            format = "{author}"
            delay = 0
            "#,
        )
        .unwrap();
        assert!(config.inline_blame.enable);
        assert_eq!(config.inline_blame.show, InlineBlameShow::AllLines);
        assert_eq!(config.inline_blame.format, "{author}");
        assert_eq!(config.inline_blame.delay, 0);

        // missing keys keep their defaults
        let config: Config = toml::from_str("[inline-blame]\nenable = true").unwrap();
        assert!(config.inline_blame.enable);
        assert_eq!(config.inline_blame.delay, 300);
        let config: Config = toml::from_str("[inline-blame]\nshow = \"cursor-line\"").unwrap();
        assert_eq!(config.inline_blame.show, InlineBlameShow::CursorLine);
    }

    #[test]
    fn config_rejects_unknown_keys_and_values() {
        assert!(toml::from_str::<Config>("[inline-blame]\nenabled = true").is_err());
        assert!(toml::from_str::<Config>("[inline-blame]\nshow = \"never\"").is_err());
        assert!(toml::from_str::<Config>("[inline-blame]\ndelay = -1").is_err());
    }

    #[test]
    fn config_can_be_changed_through_json_like_set_does() {
        // `:set inline-blame.enable true` edits the serialized config
        let mut json = serde_json::json!(&Config::default());
        *json.pointer_mut("/inline-blame/enable").unwrap() = true.into();
        *json.pointer_mut("/inline-blame/show").unwrap() = "all-lines".into();
        let config: Config = serde_json::from_value(json).unwrap();
        assert!(config.inline_blame.enable);
        assert_eq!(config.inline_blame.show, InlineBlameShow::AllLines);
    }
}
