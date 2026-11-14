//! The per-line breakpoint engine's state: hidden breakpoints on every executable line of the
//! code the follow mode covers, so the program runs (`continue`) and stops at each line, and
//! Vortex decides at each hit whether to pause there or to continue, and when.
//!
//! The hidden breakpoints of each session are known by the ids the adapter gave them: a
//! `stopped` event's `hitBreakpointIds` says whether a hit is hidden (a line), one of yours, or
//! a hidden breakpoint the adapter moved off the line it was asked for (a stray: ignored, and
//! dropped at the next send). Where a hit is comes from the id, not from a `stackTrace`, so the
//! hits of threads nobody watches cost only the event, a lookup and a `continue`.
//!
//! `vortex-term`'s `debug::engine` installs the breakpoints and drives the hits; the record of
//! the hits is [`super::lineline`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use vortex_dap::registry::DebugAdapterId;

use super::lineline::{Counter, Lineline};
use super::spectator::{Follow, Mode, ThreadKey};
use crate::editor::Breakpoint;

/// The reason a hit of a hidden breakpoint shows as, once the engine took it: `DBG stopped:
/// line`.
pub const LINE_REASON: &str = "line";

/// How playback advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EngineKind {
    /// Hidden breakpoints on every line, and `continue`.
    #[default]
    Breakpoints,
    /// `stepIn`, then `stepOut`s until a followed frame (spectator mode's first playback).
    Stepping,
}

/// A hidden breakpoint: the line it was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hidden {
    pub path: Arc<Path>,
    /// 0-based.
    pub line: usize,
    /// The adapter put it on another line: its hits are ignored.
    pub stray: bool,
}

/// How the executable lines of a file were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// DAP `breakpointLocations`.
    Locations,
    /// A breakpoint on every line, keeping the ones verified on the line asked for.
    Verified,
    /// A breakpoint on every line that the adapter didn't verify yet (js-debug, before the
    /// script loads): each is checked as its `breakpoint` event comes.
    Pending,
}

/// The hidden breakpoints of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenFile {
    /// The 0-based executable lines found. A line with one of your breakpoints is skipped when
    /// the file is sent (adapters merge two breakpoints on a line into one).
    pub lines: Vec<usize>,
    pub found: Found,
    /// Lines whose breakpoint the adapter moved: dropped at the next send.
    pub strays: BTreeSet<usize>,
}

/// The engine in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The hidden breakpoints are being sent.
    Installing,
    /// They are in place: the program stops at each line.
    Ready,
    /// Lifted for a full-speed run.
    Lifted,
    /// No hidden breakpoints: the adapter can't do breakpoints on lines, or verified none.
    Off,
}

#[derive(Debug, Clone)]
pub struct SessionEngine {
    pub state: SessionState,
    /// What is installed (or being installed): the follow mode it covers, and its files.
    pub follow: Follow,
    pub files: BTreeMap<PathBuf, HiddenFile>,
    /// The hidden breakpoints by the adapter's id.
    pub ids: HashMap<usize, Hidden>,
    /// The ids of each file's hidden breakpoints.
    pub file_ids: HashMap<PathBuf, Vec<usize>>,
    /// The executable lines found so far, by file, so a lift and a restore (or switching the
    /// follow mode back) needn't find them again.
    pub known: HashMap<PathBuf, (Found, Vec<usize>)>,
    /// Files that frames showed and the follow mode covers but aren't installed: the stepping
    /// fallback covers them until they are.
    pub uncovered: BTreeSet<PathBuf>,
    /// The program runs while the rest of the files are installed: lines of the files not done
    /// yet aren't hit (nor counted) until they are.
    pub partial: bool,
}

impl SessionEngine {
    pub fn new(follow: Follow) -> Self {
        Self {
            state: SessionState::Installing,
            follow,
            files: BTreeMap::new(),
            ids: HashMap::new(),
            file_ids: HashMap::new(),
            known: HashMap::new(),
            uncovered: BTreeSet::new(),
            partial: false,
        }
    }

    /// The hidden lines of `path` to send: the lines found, without the strays and the lines of
    /// your breakpoints.
    pub fn lines_to_send(&self, path: &Path, mine: &[usize]) -> Vec<usize> {
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        file.lines
            .iter()
            .copied()
            .filter(|line| !file.strays.contains(line) && !mine.contains(line))
            .collect()
    }

    /// The number of hidden breakpoints in place.
    pub fn count(&self) -> usize {
        self.ids.values().filter(|hidden| !hidden.stray).count()
    }

    /// Whether a frame in `path` is covered by hidden breakpoints.
    pub fn covers(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
}

/// What a hit is, from its breakpoint ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitKind {
    /// At least one of your breakpoints (it may share the stop with hidden ones).
    Mine(Vec<usize>),
    /// A line: a hidden breakpoint on the line asked for.
    Line { path: Arc<Path>, line: usize },
    /// Only hidden breakpoints the adapter moved off their line.
    Stray,
    /// Ids the engine doesn't know (a temporary breakpoint, or an answer still on its way).
    Unknown,
}

/// What the engine does with a hit of a hidden breakpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineDecision {
    /// Not the spectated thread: recorded, and continued at once, unpaced.
    Continue,
    /// Nothing is spectated yet (the program's first line): spectate this thread and pause.
    Spectate,
    /// The spectated thread: stop and show it (playback paces it, records it, and continues;
    /// paused, it stays).
    Stop,
    /// The spectated thread, running towards a target that needs no frames (run to cursor):
    /// checked against the target by its position alone.
    Check,
}

/// What a hit of a hidden breakpoint of `thread` gets.
pub fn decide_line(
    thread: ThreadKey,
    spectated: Option<ThreadKey>,
    running_to: bool,
    mode: Mode,
) -> LineDecision {
    match spectated {
        None => LineDecision::Spectate,
        Some(key) if key != thread => LineDecision::Continue,
        Some(_) if running_to && mode == Mode::Paused => LineDecision::Check,
        Some(_) => LineDecision::Stop,
    }
}

/// Whether a stop at one of your breakpoints pauses playback: the breakpoint, the thread that hit
/// it and the program's main thread (the first thread the adapter reported). A breakpoint's kind
/// (debug mode's `B`: any thread, the main thread only, sub-threads only) filters here; a hit that
/// doesn't match is recorded and continued, and doesn't move the camera. Until a main thread is
/// known, every thread counts as the main one.
pub fn breakpoint_pauses(
    breakpoint: &Breakpoint,
    thread: ThreadKey,
    main_thread: Option<ThreadKey>,
) -> bool {
    let is_main = main_thread.is_none_or(|main| main == thread);
    breakpoint.kind.matches(is_main)
}

/// A manual step (debug mode's `n`, `i`, `o`) while the hidden breakpoints are in place: the thread
/// continues, and its hits end the step where a step would have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualStep {
    pub key: ThreadKey,
    pub kind: vortex_dap::StepKind,
    /// The stack depth it started at.
    pub depth: usize,
}

impl ManualStep {
    /// Whether a hit of the stepping thread at `depth` ends the step: a step in ends at the next
    /// line anywhere, a step over at the next line not deeper than where it started, a step out
    /// at the next line above it.
    pub fn ends_at(&self, depth: usize) -> bool {
        match self.kind {
            vortex_dap::StepKind::In => true,
            vortex_dap::StepKind::Over => depth <= self.depth,
            vortex_dap::StepKind::Out => depth < self.depth,
        }
    }
}

/// What a background task of the engine is doing, for the loading animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Install,
    /// The follow mode changed.
    Reinstall,
    /// Lifting the hidden breakpoints for a full-speed run.
    Lift,
    /// Putting them back after it.
    Restore,
}

impl TaskKind {
    pub fn verb(self) -> &'static str {
        match self {
            TaskKind::Install | TaskKind::Reinstall => "installing breakpoints",
            TaskKind::Lift => "lifting breakpoints",
            TaskKind::Restore => "restoring breakpoints",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub kind: TaskKind,
    pub session: DebugAdapterId,
    pub started: Instant,
    pub files_total: usize,
    pub files_done: usize,
    /// Hidden breakpoints sent so far.
    pub lines: usize,
    /// The program runs meanwhile: the files first in line are done, the rest go in behind it.
    pub running: bool,
    /// Bumped for every new task: the answers of an older one are dropped.
    pub token: u64,
}

impl Task {
    /// `installing breakpoints · 12/48 files · 3,410 lines`
    pub fn label(&self) -> String {
        if self.files_total == 0 {
            return format!("{} · finding files", self.kind.verb());
        }
        if self.running {
            return format!(
                "{} · {}/{} files: lines in the rest aren't counted yet",
                self.kind.verb(),
                self.files_done,
                self.files_total,
            );
        }
        format!(
            "{} · {}/{} files · {} lines",
            self.kind.verb(),
            self.files_done,
            self.files_total,
            thousands(self.lines)
        )
    }
}

/// A full-speed run: the hidden breakpoints are lifted until the next stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullSpeed {
    pub session: DebugAdapterId,
    /// When the program was continued.
    pub since: Option<Instant>,
    /// The counter when it started (it doesn't move until the hidden breakpoints are back).
    pub at: Counter,
}

/// The engine: its setting, its state per session, its background task, and the record.
#[derive(Debug, Clone, Default)]
pub struct Engine {
    pub kind: EngineKind,
    pub sessions: HashMap<DebugAdapterId, SessionEngine>,
    pub task: Option<Task>,
    /// The last task's token.
    pub token: u64,
    pub record: Lineline,
    pub full_speed: Option<FullSpeed>,
    pub manual: Option<ManualStep>,
    /// Hits of other threads since the spectated thread's last hit: in a session that stops
    /// every thread at a hit, each costs the spectated thread a round trip, taken off its time.
    pub others_since: u32,
    /// The thread and counter of the last stop handed on to be shown (rewind's snapshot of it
    /// gets the counter).
    pub last_stop: Option<(ThreadKey, Counter)>,
    /// A stop recorded before its place was known: placed when its frames load.
    pub unplaced: Option<(ThreadKey, Counter)>,
    /// Playback waits for the hidden breakpoints to be back before its next advance.
    pub pending_advance: bool,
    /// A full-speed run asked for while hidden breakpoints were going in (debug mode's `L` right
    /// after a stop, while they are put back): it starts once they are in.
    pub pending_full_speed: Option<ThreadKey>,
    /// The follow mode changed during a task: reinstall after it.
    pub refollow: bool,
    /// Files to send again without their strays.
    pub redo: BTreeSet<PathBuf>,
    /// Cleared when the running task ends: the loading animation stops redrawing.
    pub animation: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// When a hit the engine took last asked for a redraw (the counter shows in the panel).
    pub drawn: Option<Instant>,
    /// The executable lines found in earlier sessions, by file, with the file's modification
    /// time then: a new session (a restart) sends them without finding them again. In memory
    /// only.
    pub remembered: HashMap<PathBuf, (std::time::SystemTime, Found, Vec<usize>)>,
}

impl Engine {
    /// A new debug session: forgets the last one (the setting, the cap and the lines found
    /// stay).
    pub fn start_session(&mut self) {
        self.sessions.clear();
        self.task = None;
        self.record.clear();
        self.full_speed = None;
        self.manual = None;
        self.others_since = 0;
        self.last_stop = None;
        self.unplaced = None;
        self.pending_advance = false;
        self.pending_full_speed = None;
        self.refollow = false;
        self.redo.clear();
        self.stop_animation();
    }

    /// Whether a hit the engine took should redraw: at most five times a second, so the counter
    /// moves without a redraw per hit.
    pub fn redraw_due(&mut self) -> bool {
        let now = Instant::now();
        let due = self
            .drawn
            .is_none_or(|drawn| now.duration_since(drawn) >= std::time::Duration::from_millis(200));
        if due {
            self.drawn = Some(now);
        }
        due
    }

    /// Stops the loading animation's redraws.
    pub fn stop_animation(&mut self) {
        if let Some(alive) = self.animation.take() {
            alive.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn end_session(&mut self) {
        self.task = None;
        self.stop_animation();
        self.pending_advance = false;
        self.pending_full_speed = None;
        self.full_speed = None;
        self.manual = None;
        for session in self.sessions.values_mut() {
            session.state = SessionState::Off;
        }
    }

    /// Whether playback in `session` runs on hidden breakpoints now.
    pub fn is_ready(&self, session: DebugAdapterId) -> bool {
        self.kind == EngineKind::Breakpoints
            && self
                .sessions
                .get(&session)
                .is_some_and(|engine| engine.state == SessionState::Ready)
    }

    /// Whether the engine has (or is putting) hidden breakpoints in `session`.
    pub fn is_on(&self, session: DebugAdapterId) -> bool {
        self.kind == EngineKind::Breakpoints
            && self.sessions.get(&session).is_some_and(|engine| {
                matches!(
                    engine.state,
                    SessionState::Ready | SessionState::Installing | SessionState::Lifted
                )
            })
    }

    /// What the breakpoint ids of a hit in `session` are. `mine` says whether an id is one of
    /// your breakpoints in that session.
    pub fn classify(
        &self,
        session: DebugAdapterId,
        ids: &[usize],
        mine: impl Fn(usize) -> bool,
    ) -> HitKind {
        let yours: Vec<usize> = ids.iter().copied().filter(|id| mine(*id)).collect();
        if !yours.is_empty() {
            return HitKind::Mine(yours);
        }
        let Some(engine) = self.sessions.get(&session) else {
            return HitKind::Unknown;
        };
        let mut stray = false;
        for id in ids {
            match engine.ids.get(id) {
                Some(hidden) if !hidden.stray => {
                    return HitKind::Line {
                        path: hidden.path.clone(),
                        line: hidden.line,
                    }
                }
                Some(_) => stray = true,
                None => {}
            }
        }
        if stray {
            HitKind::Stray
        } else {
            HitKind::Unknown
        }
    }

    /// Starts a background task, and returns its token.
    pub fn start_task(&mut self, kind: TaskKind, session: DebugAdapterId, files: usize) -> u64 {
        self.token += 1;
        self.task = Some(Task {
            kind,
            session,
            started: Instant::now(),
            files_total: files,
            files_done: 0,
            lines: 0,
            running: false,
            token: self.token,
        });
        self.token
    }

    /// The task of `token`, if it is still the current one.
    pub fn task_mut(&mut self, token: u64) -> Option<&mut Task> {
        self.task.as_mut().filter(|task| task.token == token)
    }

    /// The short label of the task for the statusline: `◜ 12/48`.
    pub fn short_label(&self, frame: usize) -> Option<String> {
        if self.task.as_ref()?.files_total == 0 {
            return Some(spinner(frame).to_string());
        }
        let task = self.task.as_ref()?;
        Some(format!(
            "{} {}/{}",
            spinner(frame),
            task.files_done,
            task.files_total
        ))
    }
}

/// The spinner's glyph at `frame` (the dashboard's).
pub fn spinner(frame: usize) -> char {
    const FRAMES: [char; 4] = ['◜', '◝', '◞', '◟'];
    FRAMES[frame % FRAMES.len()]
}

/// `3410` -> `3,410`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (DebugAdapterId, ThreadKey, ThreadKey) {
        let mut registry: slotmap::SlotMap<DebugAdapterId, ()> = slotmap::SlotMap::with_key();
        let session = registry.insert(());
        let key = |thread: i64| ThreadKey {
            session,
            thread: serde_json::from_value(serde_json::json!(thread)).unwrap(),
        };
        (session, key(1), key(2))
    }

    #[test]
    fn hits_are_classified_by_their_ids() {
        let (session, ..) = keys();
        let mut engine = Engine::default();
        let mut state = SessionEngine::new(Follow::Own);
        let path: Arc<Path> = Arc::from(Path::new("/w/main.rs"));
        state.ids.insert(
            7,
            Hidden {
                path: path.clone(),
                line: 3,
                stray: false,
            },
        );
        state.ids.insert(
            8,
            Hidden {
                path: path.clone(),
                line: 4,
                stray: true,
            },
        );
        engine.sessions.insert(session, state);
        let mine = |id: usize| id == 1;
        assert_eq!(
            engine.classify(session, &[1, 7], mine),
            HitKind::Mine(vec![1])
        );
        assert_eq!(
            engine.classify(session, &[8, 7], mine),
            HitKind::Line {
                path: path.clone(),
                line: 3
            }
        );
        assert_eq!(engine.classify(session, &[8], mine), HitKind::Stray);
        assert_eq!(engine.classify(session, &[99], mine), HitKind::Unknown);
    }

    #[test]
    fn the_spectated_thread_stops_and_the_others_continue() {
        let (_, a, b) = keys();
        assert_eq!(
            decide_line(a, None, false, Mode::Paused),
            LineDecision::Spectate
        );
        assert_eq!(
            decide_line(b, Some(a), false, Mode::Playing),
            LineDecision::Continue
        );
        assert_eq!(
            decide_line(a, Some(a), false, Mode::Playing),
            LineDecision::Stop
        );
        assert_eq!(
            decide_line(a, Some(a), false, Mode::Paused),
            LineDecision::Stop
        );
        assert_eq!(
            decide_line(a, Some(a), true, Mode::Paused),
            LineDecision::Check
        );
        // playing to a line: playback checks it at each shown stop
        assert_eq!(
            decide_line(a, Some(a), true, Mode::Playing),
            LineDecision::Stop
        );
    }

    #[test]
    fn manual_steps_end_where_a_step_would() {
        let (_, a, _) = keys();
        let step = |kind| ManualStep {
            key: a,
            kind,
            depth: 3,
        };
        use vortex_dap::StepKind::*;
        assert!(step(In).ends_at(4));
        assert!(!step(Over).ends_at(4) && step(Over).ends_at(3) && step(Over).ends_at(2));
        assert!(!step(Out).ends_at(3) && step(Out).ends_at(2));
    }

    #[test]
    fn lines_to_send_skip_strays_and_your_breakpoints() {
        let mut state = SessionEngine::new(Follow::Own);
        let path = PathBuf::from("/w/main.rs");
        state.files.insert(
            path.clone(),
            HiddenFile {
                lines: vec![1, 2, 3, 4],
                found: Found::Verified,
                strays: BTreeSet::from([2]),
            },
        );
        assert_eq!(state.lines_to_send(&path, &[4]), vec![1, 3]);
        assert!(state
            .lines_to_send(Path::new("/w/other.rs"), &[])
            .is_empty());
    }

    #[test]
    fn labels() {
        assert_eq!(thousands(3410), "3,410");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1,234,567");
        let mut registry: slotmap::SlotMap<DebugAdapterId, ()> = slotmap::SlotMap::with_key();
        let mut engine = Engine::default();
        engine.start_task(TaskKind::Install, registry.insert(()), 0);
        assert_eq!(
            engine.task.as_ref().unwrap().label(),
            "installing breakpoints · finding files"
        );
        engine.start_task(TaskKind::Install, registry.insert(()), 48);
        let task = engine.task.as_mut().unwrap();
        task.files_done = 12;
        task.lines = 3410;
        assert_eq!(
            engine.task.as_ref().unwrap().label(),
            "installing breakpoints · 12/48 files · 3,410 lines"
        );
        assert_eq!(engine.short_label(0).unwrap(), "◜ 12/48");
    }
}
