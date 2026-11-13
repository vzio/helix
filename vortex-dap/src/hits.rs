//! The fast path of the per-line breakpoint engine: a `stopped` event at hidden breakpoints of a
//! thread nobody watches is continued right here, as it comes off the connection, without
//! waiting for the editor (which may be drawing or loading values). The event still goes on to
//! the editor, marked, so it records the hit.
//!
//! The editor owns the decisions: it keeps this filter in step with its state (which ids are
//! hidden, which thread it watches) and takes every event the filter doesn't continue.

use std::collections::HashSet;

use crate::{events::StoppedBody, ThreadId};

/// What the client did with a `stopped` event before handing it to the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    /// A hit of hidden breakpoints on a thread the editor doesn't watch: continued.
    Continued,
    /// A hit of hidden breakpoints the adapter moved off their lines: continued, and not a line.
    Stray,
}

/// What the client continues by itself. Off by default.
#[derive(Debug, Clone, Default)]
pub struct HitFilter {
    /// Continue hits by itself at all (the hidden breakpoints are in place).
    pub enabled: bool,
    /// The ids of the hidden breakpoints on their lines.
    pub lines: HashSet<usize>,
    /// The ids of hidden breakpoints the adapter moved to another line.
    pub strays: HashSet<usize>,
    /// The thread of this session whose hits go to the editor (the spectated one); the hits of
    /// every other thread are continued.
    pub watched: Option<ThreadId>,
    /// Nothing is spectated yet: every hit goes to the editor, which picks the thread to follow
    /// at the first one.
    pub pick: bool,
    /// Continue with `singleThread`.
    pub alone: bool,
}

impl HitFilter {
    /// What to do with a `stopped` event: `None` hands it to the editor as it is.
    pub fn take(&self, body: &StoppedBody) -> Option<Taken> {
        if !self.enabled || body.reason != "breakpoint" || body.preserve_focus_hint == Some(true) {
            return None;
        }
        let thread = body.thread_id?;
        let ids = body
            .hit_breakpoint_ids
            .as_deref()
            .filter(|ids| !ids.is_empty())?;
        let mut line = false;
        for id in ids {
            let id = usize::try_from(*id).ok()?;
            if self.lines.contains(&id) {
                line = true;
            } else if !self.strays.contains(&id) {
                // one of yours, or an id the editor doesn't know yet
                return None;
            }
        }
        if !line {
            return Some(Taken::Stray);
        }
        if self.pick || self.watched == Some(thread) {
            return None;
        }
        Some(Taken::Continued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(thread: i64, ids: &[i32]) -> StoppedBody {
        StoppedBody {
            reason: "breakpoint".into(),
            description: None,
            thread_id: Some(serde_json::from_value(serde_json::json!(thread)).unwrap()),
            preserve_focus_hint: None,
            text: None,
            all_threads_stopped: Some(true),
            hit_breakpoint_ids: Some(ids.to_vec()),
        }
    }

    #[test]
    fn only_hidden_hits_of_unwatched_threads_are_taken() {
        let mut filter = HitFilter {
            enabled: true,
            lines: HashSet::from([1, 2]),
            strays: HashSet::from([3]),
            watched: Some(serde_json::from_value(serde_json::json!(1)).unwrap()),
            pick: false,
            alone: false,
        };
        assert_eq!(filter.take(&stop(2, &[1])), Some(Taken::Continued));
        assert_eq!(filter.take(&stop(2, &[3, 2])), Some(Taken::Continued));
        // the watched thread goes to the editor, unless only strays stopped it
        assert_eq!(filter.take(&stop(1, &[1])), None);
        assert_eq!(filter.take(&stop(1, &[3])), Some(Taken::Stray));
        // one of yours (an id it doesn't know) goes to the editor
        assert_eq!(filter.take(&stop(2, &[1, 9])), None);
        let mut hint = stop(2, &[1]);
        hint.preserve_focus_hint = Some(true);
        assert_eq!(filter.take(&hint), None);
        let mut exception = stop(2, &[1]);
        exception.reason = "exception".into();
        assert_eq!(filter.take(&exception), None);
        // nothing spectated yet: the editor picks the thread at the first hit
        filter.pick = true;
        assert_eq!(filter.take(&stop(2, &[1])), None);
        // the spectated thread is another session's: every hit here is continued
        filter.pick = false;
        filter.watched = None;
        assert_eq!(filter.take(&stop(1, &[1])), Some(Taken::Continued));
        filter.enabled = false;
        assert_eq!(filter.take(&stop(1, &[3])), None);
    }
}
