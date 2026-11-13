//! Rewind: every stop recorded whole (the call stack of the spectated thread and the variables of
//! all its frames, within caps), because a value missed at a stop can never be fetched again.
//! Scrubbing shows a recorded stop read-only; nothing executes.

use std::collections::VecDeque;

use vortex_dap::StackFrame;

use super::spectator::ThreadKey;
use super::{VarNode, Watch};

/// How much a stop's capture loads, and how many stops are kept. `None` is unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// Levels of variables below a scope: 1 is the scope's own variables.
    pub depth: Option<usize>,
    /// The children of a variable loaded at most (its length is kept).
    pub collection: Option<usize>,
    /// Stops kept; the oldest go first.
    pub history: Option<usize>,
    /// Bytes recorded per open memory view at each stop: what it shows and a margin around it,
    /// at most this many (`None`: all of that).
    pub memory: Option<usize>,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            depth: Some(3),
            collection: Some(100),
            history: Some(2000),
            memory: Some(4096),
        }
    }
}

/// The steps of each cap in the capture settings; the last is unlimited.
pub const DEPTH_STEPS: &[Option<usize>] = &[
    Some(0),
    Some(1),
    Some(2),
    Some(3),
    Some(4),
    Some(6),
    Some(8),
    None,
];
pub const COLLECTION_STEPS: &[Option<usize>] = &[
    Some(10),
    Some(25),
    Some(50),
    Some(100),
    Some(250),
    Some(1000),
    None,
];
pub const HISTORY_STEPS: &[Option<usize>] = &[
    Some(0),
    Some(100),
    Some(500),
    Some(1000),
    Some(2000),
    Some(5000),
    Some(20000),
    None,
];

pub const MEMORY_STEPS: &[Option<usize>] = &[
    Some(0),
    Some(256),
    Some(1024),
    Some(4096),
    Some(16384),
    Some(65536),
    None,
];

/// The rows of the capture settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Depth,
    Collection,
    History,
    Memory,
}

impl Setting {
    pub const ALL: [Setting; 4] = [
        Setting::Depth,
        Setting::Collection,
        Setting::History,
        Setting::Memory,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Setting::Depth => "depth",
            Setting::Collection => "collection size",
            Setting::History => "history",
            Setting::Memory => "memory window",
        }
    }

    fn steps(self) -> &'static [Option<usize>] {
        match self {
            Setting::Depth => DEPTH_STEPS,
            Setting::Collection => COLLECTION_STEPS,
            Setting::History => HISTORY_STEPS,
            Setting::Memory => MEMORY_STEPS,
        }
    }
}

impl Caps {
    pub fn get(&self, setting: Setting) -> Option<usize> {
        match setting {
            Setting::Depth => self.depth,
            Setting::Collection => self.collection,
            Setting::History => self.history,
            Setting::Memory => self.memory,
        }
    }

    fn get_mut(&mut self, setting: Setting) -> &mut Option<usize> {
        match setting {
            Setting::Depth => &mut self.depth,
            Setting::Collection => &mut self.collection,
            Setting::History => &mut self.history,
            Setting::Memory => &mut self.memory,
        }
    }

    /// Moves `setting` `by` steps (up is bigger; unlimited is last).
    pub fn change(&mut self, setting: Setting, by: isize) {
        let steps = setting.steps();
        let value = self.get_mut(setting);
        // the step at or above the current value
        let index = steps
            .iter()
            .position(|step| match (step, *value) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(step), Some(value)) => *step >= value,
            })
            .unwrap_or(steps.len() - 1) as isize;
        let index = (index + by).clamp(0, steps.len() as isize - 1) as usize;
        *value = steps[index];
    }

    /// The value as shown: `3`, `unlimited`, `off` (a history of 0).
    pub fn show(&self, setting: Setting) -> String {
        match (setting, self.get(setting)) {
            (_, None) => "unlimited".into(),
            (Setting::History, Some(0)) => "off".into(),
            (Setting::History, Some(n)) => format!("{n} stops"),
            (Setting::Depth, Some(n)) => format!("{n} levels"),
            (Setting::Collection, Some(n)) => format!("{n} elements"),
            (Setting::Memory, Some(0)) => "off".into(),
            (Setting::Memory, Some(n)) => format!("{} per view", format_bytes(n)),
        }
    }

    pub fn is_unlimited(&self) -> bool {
        self.depth.is_none()
            || self.collection.is_none()
            || self.history.is_none()
            || self.memory.is_none()
    }

    pub fn records(&self) -> bool {
        self.history != Some(0)
    }
}

/// A stop, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Its number in the session, from 1.
    pub step: usize,
    /// Its place on the lineline: the counter of its hit (the engine's), when it had one.
    pub counter: Option<super::lineline::Counter>,
    pub key: ThreadKey,
    pub thread_name: String,
    /// The stop's reason: `step`, `breakpoint`, ...
    pub reason: String,
    pub frames: Vec<StackFrame>,
    /// The scopes of each frame, loaded within the caps; `None` when the frame's scopes couldn't
    /// be loaded.
    pub scopes: Vec<Option<Vec<VarNode>>>,
    pub watches: Vec<Watch>,
    /// The bytes of each open memory view, around what it showed.
    pub memory: Vec<MemoryWindow>,
    /// Requests the capture made.
    pub requests: usize,
    /// About how much memory it takes.
    pub bytes: usize,
    /// A cap cut something off.
    pub truncated: bool,
}

impl Snapshot {
    /// The approximate size of the snapshot in memory.
    pub fn measure(&mut self) {
        let frames: usize = self
            .frames
            .iter()
            .map(|frame| {
                std::mem::size_of::<StackFrame>()
                    + frame.name.len()
                    + frame
                        .source
                        .as_ref()
                        .and_then(|source| source.path.as_ref())
                        .map_or(0, |path| path.as_os_str().len())
            })
            .sum();
        let scopes: usize = self
            .scopes
            .iter()
            .flatten()
            .flatten()
            .map(VarNode::approx_bytes)
            .sum();
        let watches: usize = self
            .watches
            .iter()
            .map(|watch| {
                watch.expression.len() + watch.node.as_ref().map_or(0, VarNode::approx_bytes)
            })
            .sum();
        let memory: usize = self
            .memory
            .iter()
            .map(|window| std::mem::size_of::<MemoryWindow>() + window.bytes.len() * 2)
            .sum();
        self.bytes = std::mem::size_of::<Snapshot>() + frames + scopes + watches + memory;
    }
}

/// The bytes of a memory view recorded at a stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWindow {
    /// The document of the memory view.
    pub doc: crate::DocumentId,
    pub start: u64,
    /// `None` where unreadable.
    pub bytes: Vec<Option<u8>>,
}

/// The recorded stops of the session, and where rewind is.
#[derive(Debug, Clone, Default)]
pub struct History {
    pub stops: VecDeque<Snapshot>,
    /// The number of the last stop recorded.
    pub last_step: usize,
    /// While rewinding: the index in `stops` shown.
    pub at: Option<usize>,
    /// The frame shown in rewind.
    pub frame: usize,
}

impl History {
    pub fn clear(&mut self) {
        self.stops.clear();
        self.last_step = 0;
        self.at = None;
        self.frame = 0;
    }

    /// The number the next stop gets.
    pub fn next_step(&mut self) -> usize {
        self.last_step += 1;
        self.last_step
    }

    /// Keeps `snapshot`, dropping the oldest stops over `limit`. A rewind stays on the stop it
    /// shows.
    pub fn push(&mut self, snapshot: Snapshot, limit: Option<usize>) {
        if limit == Some(0) {
            return;
        }
        self.stops.push_back(snapshot);
        if let Some(limit) = limit {
            while self.stops.len() > limit {
                self.stops.pop_front();
                if let Some(at) = &mut self.at {
                    *at = at.saturating_sub(1);
                }
            }
        }
    }

    /// Applies a smaller limit at once.
    pub fn trim(&mut self, limit: Option<usize>) {
        if let Some(limit) = limit {
            while self.stops.len() > limit {
                self.stops.pop_front();
                if let Some(at) = &mut self.at {
                    *at = at.saturating_sub(1);
                }
            }
            if self.stops.is_empty() {
                self.at = None;
            }
        }
    }

    pub fn is_rewinding(&self) -> bool {
        self.at.is_some()
    }

    /// The stop shown in rewind.
    pub fn current(&self) -> Option<&Snapshot> {
        self.stops.get(self.at?)
    }

    /// Goes back `by` stops (entering rewind at the last one, which is the live stop: so the
    /// first `[` shows the stop before it). Returns false when there is nothing before.
    pub fn back(&mut self, by: usize) -> bool {
        let len = self.stops.len();
        if len == 0 {
            return false;
        }
        let from = self.at.unwrap_or(len - 1);
        if from == 0 {
            return self.at.is_some();
        }
        self.at = Some(from.saturating_sub(by));
        self.frame = 0;
        true
    }

    /// Goes forward `by` stops; past the last one, rewind ends (back to live). Returns whether it
    /// is still rewinding.
    pub fn forward(&mut self, by: usize) -> bool {
        let Some(at) = self.at else {
            return false;
        };
        let to = at + by;
        if to >= self.stops.len() - 1 {
            self.live();
            false
        } else {
            self.at = Some(to);
            self.frame = 0;
            true
        }
    }

    /// Back to the live position.
    pub fn live(&mut self) {
        self.at = None;
        self.frame = 0;
    }

    /// The marker shown in rewind: `REWIND · step 132/480 · nothing is executing`.
    pub fn marker(&self) -> Option<String> {
        let snapshot = self.current()?;
        Some(format!(
            "REWIND · step {}/{} · nothing is executing",
            snapshot.step, self.last_step
        ))
    }

    /// The average requests and bytes per stop over the last stops recorded.
    pub fn cost(&self) -> Option<(f64, usize)> {
        let recent: Vec<&Snapshot> = self.stops.iter().rev().take(20).collect();
        if recent.is_empty() {
            return None;
        }
        let requests =
            recent.iter().map(|s| s.requests).sum::<usize>() as f64 / recent.len() as f64;
        let bytes = recent.iter().map(|s| s.bytes).sum::<usize>() / recent.len();
        Some((requests, bytes))
    }

    /// The memory the history takes.
    pub fn bytes(&self) -> usize {
        self.stops.iter().map(|s| s.bytes).sum()
    }
}

/// `512 B`, `12 KB`, `3.4 MB`.
pub fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// An estimate of a capture's cost before any stop was recorded: `frames` frames of `scopes`
/// scopes, each variable with `fanout` children, within `caps`.
pub fn estimate(caps: &Caps, frames: usize, scopes: usize, fanout: usize) -> (usize, usize) {
    let fanout = fanout.max(1);
    let per_level = caps.collection.unwrap_or(fanout * 4).min(fanout).max(1);
    let depth = caps.depth.unwrap_or(6);
    // a variables request per expandable node, per level; assume a third have children
    let mut requests_per_scope = 1usize;
    let mut nodes = 1usize;
    for _ in 1..depth {
        nodes = nodes.saturating_mul(per_level) / 3;
        requests_per_scope = requests_per_scope.saturating_add(nodes);
    }
    let requests = frames * (1 + scopes * requests_per_scope);
    let bytes = frames * scopes * requests_per_scope * per_level * 96;
    (requests, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ThreadKey {
        let mut registry: slotmap::SlotMap<vortex_dap::registry::DebugAdapterId, ()> =
            slotmap::SlotMap::with_key();
        ThreadKey {
            session: registry.insert(()),
            thread: serde_json::from_str("1").unwrap(),
        }
    }

    fn snapshot(step: usize) -> Snapshot {
        Snapshot {
            step,
            counter: None,
            key: key(),
            thread_name: "main".into(),
            reason: "step".into(),
            frames: Vec::new(),
            scopes: Vec::new(),
            watches: Vec::new(),
            memory: Vec::new(),
            requests: 10,
            bytes: 100,
            truncated: false,
        }
    }

    fn history(n: usize, limit: Option<usize>) -> History {
        let mut history = History::default();
        for _ in 0..n {
            let step = history.next_step();
            history.push(snapshot(step), limit);
        }
        history
    }

    #[test]
    fn the_oldest_stops_go_first() {
        let history = history(10, Some(4));
        let steps: Vec<_> = history.stops.iter().map(|s| s.step).collect();
        assert_eq!(steps, [7, 8, 9, 10]);
        assert_eq!(history.last_step, 10);
        let mut off = History::default();
        off.push(snapshot(1), Some(0));
        assert!(off.stops.is_empty());
    }

    #[test]
    fn scrubbing_goes_back_and_forward_and_ends_at_live() {
        let mut history = history(5, None);
        assert!(!history.is_rewinding());
        assert!(history.back(1));
        assert_eq!(history.current().unwrap().step, 4);
        assert_eq!(
            history.marker().unwrap(),
            "REWIND · step 4/5 · nothing is executing"
        );
        assert!(history.back(10));
        assert_eq!(history.current().unwrap().step, 1);
        // nothing before the first
        assert!(history.back(1));
        assert_eq!(history.current().unwrap().step, 1);
        assert!(history.forward(2));
        assert_eq!(history.current().unwrap().step, 3);
        assert!(history.forward(1));
        assert_eq!(history.current().unwrap().step, 4);
        // the last stop is the live one
        assert!(!history.forward(1));
        assert!(!history.is_rewinding());
    }

    #[test]
    fn a_rewind_stays_on_its_stop_while_old_ones_drop() {
        let mut history = history(4, Some(4));
        history.back(2);
        assert_eq!(history.current().unwrap().step, 2);
        let step = history.next_step();
        history.push(snapshot(step), Some(4));
        assert_eq!(history.current().unwrap().step, 2);
    }

    #[test]
    fn caps_move_by_steps_up_to_unlimited() {
        let mut caps = Caps::default();
        assert_eq!(caps.show(Setting::Depth), "3 levels");
        caps.change(Setting::Depth, 1);
        assert_eq!(caps.depth, Some(4));
        caps.change(Setting::Depth, 100);
        assert_eq!(caps.depth, None);
        assert!(caps.is_unlimited());
        assert_eq!(caps.show(Setting::Depth), "unlimited");
        caps.change(Setting::Depth, -1);
        assert_eq!(caps.depth, Some(8));
        caps.history = Some(1234);
        caps.change(Setting::History, 1);
        assert_eq!(caps.history, Some(5000));
        caps.change(Setting::History, -100);
        assert_eq!(caps.show(Setting::History), "off");
        assert!(!caps.records());
    }

    #[test]
    fn the_cost_is_averaged_over_recent_stops() {
        let history = history(3, None);
        assert_eq!(history.cost(), Some((10.0, 100)));
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(12 * 1024), "12 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
        let (small, _) = estimate(&Caps::default(), 5, 2, 10);
        let deep = Caps {
            depth: Some(6),
            ..Caps::default()
        };
        let (big, _) = estimate(&deep, 5, 2, 10);
        assert!(big > small);
    }
}
