//! Spectator mode: the program plays line by line at a chosen speed while you watch one thread,
//! fast forwards to the first change of a watched value, and keeps every stop it saw so you can
//! scrub back through them. This module holds the state; `vortex-term`'s `debug::playback`
//! drives the adapter.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vortex_dap::{registry::DebugAdapterId, StackFrame, ThreadId};

use super::history::{Caps, History};
use super::timing::Timing;

/// The playback speeds, in lines per second.
pub const SPEEDS: [f64; 9] = [0.5, 1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 18.0, 25.0];
/// The speed playback starts at: 2 lines per second.
pub const DEFAULT_SPEED: usize = 2;
/// A step that hasn't stopped after this long shows as blocked.
pub const BLOCKED_AFTER: Duration = Duration::from_millis(500);
/// At most this many `stepOut`s to get back to code the mode follows, per playback step.
pub const MAX_STEP_OUTS: u32 = 32;

/// A thread of a session: thread ids are only unique within their session (js-debug runs each
/// worker in a session of its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThreadKey {
    pub session: DebugAdapterId,
    pub thread: ThreadId,
}

/// What playback steps into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Follow {
    /// Everything, the language's runtime included.
    Std,
    /// Everything but the runtime.
    Lib,
    /// Only the workspace's own code.
    #[default]
    Own,
}

impl Follow {
    /// std -> lib -> own -> std.
    pub fn next(self) -> Follow {
        match self {
            Follow::Std => Follow::Lib,
            Follow::Lib => Follow::Own,
            Follow::Own => Follow::Std,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Follow::Std => "std",
            Follow::Lib => "lib",
            Follow::Own => "own",
        }
    }

    /// Whether playback stays in a frame of this origin.
    pub fn allows(self, origin: Origin) -> bool {
        match self {
            Follow::Std => true,
            Follow::Lib => origin != Origin::Std,
            Follow::Own => origin == Origin::Own,
        }
    }
}

/// Where a frame's code comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Inside the workspace (and not in a library folder there).
    Own,
    /// A library: a registry, `node_modules`, anything outside the workspace.
    Lib,
    /// The language's runtime, or a frame without source.
    Std,
}

/// Path pieces that mark the runtime when no debugger config says otherwise: Rust's sysroot
/// (as rustc records it, and as rustup installs its sources), C headers and system sources,
/// Node's internals.
pub const DEFAULT_STD_PATHS: &[&str] = &[
    "/rustc/",
    "/lib/rustlib/src/",
    "/usr/include/",
    "/usr/lib/",
    "/usr/src/",
    "/usr/local/include/",
    "node:",
    "<node_internals>",
];

/// Path pieces that mark a library inside the workspace.
pub const DEFAULT_LIB_PATHS: &[&str] = &[
    "/.cargo/registry/",
    "/.cargo/git/",
    "/node_modules/",
    "/vendor/",
];

/// How frames are classified: by their source path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginRules {
    /// Folders of your own code: the workspace, and the program's folder and working directory
    /// when they are elsewhere.
    pub roots: Vec<PathBuf>,
    /// A path containing one of these is the runtime's.
    pub std: Vec<String>,
    /// A path containing one of these is a library's, even inside the workspace.
    pub lib: Vec<String>,
}

impl OriginRules {
    pub fn new(roots: Vec<PathBuf>, std: Option<Vec<String>>, lib: Option<Vec<String>>) -> Self {
        let owned = |list: &[&str]| list.iter().map(|s| s.to_string()).collect();
        Self {
            roots,
            std: std.unwrap_or_else(|| owned(DEFAULT_STD_PATHS)),
            lib: lib.unwrap_or_else(|| owned(DEFAULT_LIB_PATHS)),
        }
    }

    /// The origin of a frame: std when it has no source or its path (or source name, for
    /// sources without a path) contains a runtime piece; lib when it contains a library piece
    /// or is outside the roots; own otherwise.
    pub fn classify(&self, frame: &StackFrame) -> Origin {
        let Some(source) = frame.source.as_ref() else {
            return Origin::Std;
        };
        let text = match (&source.path, &source.name) {
            (Some(path), _) => path.to_string_lossy().replace('\\', "/"),
            (None, Some(name)) => name.clone(),
            (None, None) => return Origin::Std,
        };
        if self.std.iter().any(|piece| text.contains(piece.as_str())) {
            return Origin::Std;
        }
        if self.lib.iter().any(|piece| text.contains(piece.as_str())) {
            return Origin::Lib;
        }
        match &source.path {
            Some(path)
                if path.is_absolute() && self.roots.iter().any(|root| path.starts_with(root)) =>
            {
                Origin::Own
            }
            _ => Origin::Lib,
        }
    }
}

/// What playback does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Paused,
    /// Steps at the chosen speed.
    Playing,
    /// Steps as fast as the adapter goes until a watched value changes.
    FastForward,
}

/// The unit of a step, when the adapter offers a choice (`supportsSteppingGranularity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Granularity {
    #[default]
    Line,
    Statement,
    Instruction,
}

impl Granularity {
    pub fn next(self) -> Granularity {
        match self {
            Granularity::Line => Granularity::Statement,
            Granularity::Statement => Granularity::Instruction,
            Granularity::Instruction => Granularity::Line,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Granularity::Line => "line",
            Granularity::Statement => "statement",
            Granularity::Instruction => "instruction",
        }
    }
}

/// The playback step being executed: sent, and not stopped yet (or stopped outside the code
/// the mode follows, and stepping out of it).
#[derive(Debug, Clone)]
pub struct StepInFlight {
    pub key: ThreadKey,
    /// When the first request was sent.
    pub sent: Instant,
    /// Step requests sent for it (the step, then any `stepOut`s).
    pub requests: u32,
    /// When its last request was sent.
    pub request_sent: Instant,
    /// The time the adapter took for each request so far: from the request to its stop's event
    /// (without what the editor loads in between).
    pub times: Vec<Duration>,
    /// The file and 0-based line it leaves, which gets its time.
    pub from: Option<(PathBuf, usize)>,
    /// The function it leaves, for "blocked in".
    pub function: String,
    /// The stack depth it started at: a step at the same depth with one request calibrates the
    /// overhead.
    pub depth: usize,
    /// The thread's (or process's) CPU time when it was sent.
    pub cpu_before: Option<Duration>,
    /// One step (`n`): playback pauses after it.
    pub single: bool,
    /// Cleared when the step ends: the blocked timer's ticks stop.
    pub alive: Arc<AtomicBool>,
}

/// Where run to cursor goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTo {
    pub path: PathBuf,
    /// 0-based.
    pub line: usize,
}

/// A watched value at the start of a fast forward, and what it became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    pub old: String,
    pub new: String,
}

/// Spectator mode's state, kept for the whole editor session (the speed and the mode carry
/// over to the next debug session; the rest is per session).
#[derive(Debug)]
pub struct Spectator {
    pub mode: Mode,
    /// An index into [`SPEEDS`].
    pub speed: usize,
    pub follow: Follow,
    pub granularity: Granularity,
    /// Bumped on every pause and mode change: a scheduled step from before is dropped.
    pub token: u64,
    /// Playback steps since play (or fast forward) started.
    pub steps: usize,
    pub step: Option<StepInFlight>,
    pub run_to: Option<RunTo>,
    /// The values fast forward compares against: watch expressions and marked variables, by
    /// their name in the panel.
    pub baseline: HashMap<String, String>,
    /// The last fast forward's result, shown until playback starts again.
    pub changes: Vec<Change>,
    /// The thread playback follows.
    pub spectated: Option<ThreadKey>,
    /// The frame shown while a step runs (the debugger forgets it on resume), so the editor
    /// keeps the line and "blocked in" has a name.
    pub shown_frame: Option<StackFrame>,
    /// Threads that never run while frozen.
    pub frozen: HashSet<ThreadKey>,
    /// Steps and continues run only the thread they are for.
    pub solo: bool,
    /// Sessions whose stops leave the other threads running (`allThreadsStopped: false`):
    /// their threads can be resumed one by one. In the others one thread runs alone or all of
    /// them do, so a frozen thread makes steps solo.
    pub non_stop: HashSet<DebugAdapterId>,
    /// When the last playback step was sent: the next waits out the speed's interval from it.
    pub last_sent: Option<Instant>,
    /// Variables marked for fast forward (`*`), by path names in the scopes.
    pub marked: HashSet<Vec<String>>,
    pub history: History,
    pub caps: Caps,
    pub timing: Timing,
    /// The capture settings are open in the panel, with this row selected.
    pub settings: Option<usize>,
    /// The process id of a local debuggee (the DAP `process` event), for CPU times.
    pub pid: Option<u32>,
    /// When the last `stopped` event arrived, before its stack loaded.
    pub stop_arrived: Option<Instant>,
    /// Stops being recorded now: playback waits for them.
    pub recording: usize,
}

impl Default for Spectator {
    fn default() -> Self {
        Self {
            mode: Mode::Paused,
            speed: DEFAULT_SPEED,
            follow: Follow::default(),
            granularity: Granularity::default(),
            token: 0,
            steps: 0,
            step: None,
            run_to: None,
            baseline: HashMap::new(),
            changes: Vec::new(),
            spectated: None,
            shown_frame: None,
            frozen: HashSet::new(),
            solo: false,
            non_stop: HashSet::new(),
            last_sent: None,
            marked: HashSet::new(),
            history: History::default(),
            caps: Caps::default(),
            timing: Timing::default(),
            settings: None,
            pid: None,
            stop_arrived: None,
            recording: 0,
        }
    }
}

impl Spectator {
    pub fn lines_per_second(&self) -> f64 {
        SPEEDS[self.speed.min(SPEEDS.len() - 1)]
    }

    /// The time between two playback steps.
    pub fn interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.lines_per_second())
    }

    pub fn faster(&mut self) {
        self.speed = (self.speed + 1).min(SPEEDS.len() - 1);
    }

    pub fn slower(&mut self) {
        self.speed = self.speed.saturating_sub(1);
    }

    /// The speed, as shown: `0.5 l/s`, `12 l/s`.
    pub fn speed_label(&self) -> String {
        let speed = self.lines_per_second();
        if speed.fract() == 0.0 {
            format!("{speed:.0} l/s")
        } else {
            format!("{speed} l/s")
        }
    }

    pub fn is_playing(&self) -> bool {
        self.mode != Mode::Paused
    }

    /// Stops playback (or fast forward); a scheduled step is dropped.
    pub fn pause(&mut self) {
        self.mode = Mode::Paused;
        self.token += 1;
    }

    /// Starts playing (or fast forwarding).
    pub fn play(&mut self, mode: Mode) {
        self.mode = mode;
        self.token += 1;
        self.steps = 0;
        self.changes.clear();
    }

    /// A new debug session: forgets the last one's threads, history and labels, keeps the
    /// speed, the mode, the caps and the marks.
    pub fn start_session(&mut self) {
        self.pause();
        self.finish_step();
        self.run_to = None;
        self.recording = 0;
        self.baseline.clear();
        self.changes.clear();
        self.spectated = None;
        self.shown_frame = None;
        self.frozen.clear();
        self.solo = false;
        self.non_stop.clear();
        self.last_sent = None;
        self.history.clear();
        self.timing.clear();
        self.settings = None;
        self.pid = None;
    }

    pub fn end_session(&mut self) {
        self.pause();
        self.finish_step();
        self.run_to = None;
        self.shown_frame = None;
        self.history.live();
        self.settings = None;
        self.pid = None;
    }

    /// Ends the step in flight, and returns it.
    pub fn finish_step(&mut self) -> Option<StepInFlight> {
        let step = self.step.take()?;
        step.alive.store(false, Ordering::Relaxed);
        Some(step)
    }

    /// The step that has been running longer than [`BLOCKED_AFTER`], and for how long.
    pub fn blocked(&self, now: Instant) -> Option<(&StepInFlight, Duration)> {
        let step = self.step.as_ref()?;
        let elapsed = now.saturating_duration_since(step.sent);
        (elapsed >= BLOCKED_AFTER).then_some((step, elapsed))
    }

    pub fn is_frozen(&self, key: ThreadKey) -> bool {
        self.frozen.contains(&key)
    }

    /// Freezes or thaws `key`; returns whether it is frozen now.
    pub fn toggle_frozen(&mut self, key: ThreadKey) -> bool {
        if !self.frozen.remove(&key) {
            self.frozen.insert(key);
            true
        } else {
            false
        }
    }

    /// Whether a step or a continue of `key` runs it alone: solo is on, or another thread of
    /// its session is frozen.
    pub fn runs_alone(&self, key: ThreadKey) -> bool {
        self.solo
            || self
                .frozen
                .iter()
                .any(|frozen| frozen.session == key.session && frozen.thread != key.thread)
    }

    /// The status of playback for the statusline and the panel: `▶ 3 l/s · own`,
    /// `⏩ fast forward 132`, `⏸ 3 l/s`, `blocked in read 1.2 s`.
    pub fn label(&self, now: Instant) -> Option<String> {
        if let Some((step, elapsed)) = self.blocked(now) {
            return Some(format!(
                "blocked in {} {}",
                if step.function.is_empty() {
                    "?"
                } else {
                    step.function.as_str()
                },
                super::timing::format_duration(elapsed)
            ));
        }
        match self.mode {
            Mode::Paused => None,
            Mode::Playing => Some(format!(
                "▶ {} · {}{}",
                self.speed_label(),
                self.follow.label(),
                if self.recording > 0 {
                    " · recording…"
                } else {
                    ""
                }
            )),
            Mode::FastForward => Some(format!("⏩ fast forward · {} steps", self.steps)),
        }
    }
}

/// `path` with the workspace's prefix taken off, for messages.
pub fn short_path<'a>(path: &'a Path, workspace: &Path) -> &'a Path {
    path.strip_prefix(workspace).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vortex_dap::Source;

    fn frame(path: Option<&str>, name: Option<&str>) -> StackFrame {
        StackFrame {
            id: 1,
            name: "f".into(),
            source: (path.is_some() || name.is_some()).then(|| Source {
                name: name.map(str::to_owned),
                path: path.map(PathBuf::from),
                ..Default::default()
            }),
            line: 1,
            column: 1,
            end_line: None,
            end_column: None,
            can_restart: None,
            instruction_pointer_reference: None,
            module_id: None,
            presentation_hint: None,
        }
    }

    fn rules() -> OriginRules {
        OriginRules::new(vec![PathBuf::from("/work/app")], None, None)
    }

    #[test]
    fn frames_are_classified_by_their_path() {
        let rules = rules();
        let cases = [
            (Some("/work/app/src/main.rs"), None, Origin::Own),
            (Some("/work/app/node_modules/x/index.js"), None, Origin::Lib),
            (
                Some("/home/u/.cargo/registry/src/index/serde-1.0/src/lib.rs"),
                None,
                Origin::Lib,
            ),
            (
                Some("/rustc/90b35a6/library/core/src/iter.rs"),
                None,
                Origin::Std,
            ),
            (
                Some(
                    "/home/u/.rustup/toolchains/stable/lib/rustlib/src/rust/library/std/src/io.rs",
                ),
                None,
                Origin::Std,
            ),
            (Some("/usr/include/stdio.h"), None, Origin::Std),
            (Some("/elsewhere/lib.c"), None, Origin::Lib),
            (None, Some("node:internal/timers"), Origin::Std),
            (None, Some("<node_internals>/internal/main"), Origin::Std),
            (None, Some("eval-1234"), Origin::Lib),
            (None, None, Origin::Std),
        ];
        for (path, name, origin) in cases {
            assert_eq!(
                rules.classify(&frame(path, name)),
                origin,
                "{path:?} {name:?}"
            );
        }
    }

    #[test]
    fn the_modes_allow_what_they_say() {
        assert!(Follow::Std.allows(Origin::Std));
        assert!(Follow::Lib.allows(Origin::Lib) && !Follow::Lib.allows(Origin::Std));
        assert!(Follow::Own.allows(Origin::Own) && !Follow::Own.allows(Origin::Lib));
        assert_eq!(Follow::Std.next().next().next(), Follow::Std);
        assert_eq!(Follow::default(), Follow::Own);
    }

    #[test]
    fn speeds_are_clamped_and_labelled() {
        let mut spectator = Spectator::default();
        assert_eq!(spectator.speed_label(), "2 l/s");
        for _ in 0..20 {
            spectator.slower();
        }
        assert_eq!(spectator.speed_label(), "0.5 l/s");
        assert_eq!(spectator.interval(), Duration::from_secs(2));
        for _ in 0..20 {
            spectator.faster();
        }
        assert_eq!(spectator.speed_label(), "25 l/s");
    }

    #[test]
    fn a_frozen_thread_makes_steps_of_its_neighbours_solo() {
        let mut registry: slotmap::SlotMap<DebugAdapterId, ()> = slotmap::SlotMap::with_key();
        let a = registry.insert(());
        let b = registry.insert(());
        let key = |session, thread: isize| ThreadKey {
            session,
            thread: serde_json::from_value(serde_json::json!(thread)).unwrap(),
        };
        let mut spectator = Spectator::default();
        assert!(!spectator.runs_alone(key(a, 1)));
        assert!(spectator.toggle_frozen(key(a, 2)));
        assert!(spectator.runs_alone(key(a, 1)));
        // another session's threads are independent
        assert!(!spectator.runs_alone(key(b, 1)));
        assert!(!spectator.toggle_frozen(key(a, 2)));
        assert!(!spectator.runs_alone(key(a, 1)));
        spectator.solo = true;
        assert!(spectator.runs_alone(key(b, 1)));
    }

    #[test]
    fn a_step_is_blocked_after_a_while() {
        let mut registry: slotmap::SlotMap<DebugAdapterId, ()> = slotmap::SlotMap::with_key();
        let session = registry.insert(());
        let mut spectator = Spectator::default();
        let sent = Instant::now();
        spectator.step = Some(StepInFlight {
            key: ThreadKey {
                session,
                thread: serde_json::from_str("1").unwrap(),
            },
            sent,
            requests: 1,
            request_sent: sent,
            times: Vec::new(),
            from: None,
            function: "read".into(),
            depth: 1,
            cpu_before: None,
            single: false,
            alive: Arc::new(AtomicBool::new(true)),
        });
        assert!(spectator
            .blocked(sent + Duration::from_millis(100))
            .is_none());
        let label = spectator.label(sent + Duration::from_millis(1200)).unwrap();
        assert_eq!(label, "blocked in read 1.2 s");
    }
}
