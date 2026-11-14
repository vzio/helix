//! The lineline: one counter of lines executed, shared by every thread of every session, and the
//! record of every hit against it. The engine's hidden breakpoints stop the program at each line
//! it runs; every such hit, on any thread, advances the counter by one. The counter (not time) is
//! the index everything else is recorded against: the hits themselves, the marks (breakpoints,
//! exceptions, switches of the spectated thread, full-speed gaps, threads starting and exiting),
//! the time each line of the spectated thread took, and rewind's snapshots.
//!
//! A hit is kept in 8 bytes: its counter is its position (plus how many older hits the cap
//! dropped), its file and thread are indexes into small tables, and why it stopped is a line
//! unless a sparse list of reasons says otherwise (the other reasons are rare by construction:
//! one of your breakpoints, an exception, a pause). 100,000 hits take 800 KB.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::spectator::ThreadKey;

/// A position on the lineline: the number of hits before it.
pub type Counter = u64;

/// The file index of a hit without a source (code without debug info).
pub const NO_FILE: u16 = u16::MAX;

/// Why a thread stopped at a hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Why {
    /// A hidden breakpoint: the thread reached a line.
    Line,
    /// One of your breakpoints.
    Breakpoint,
    Exception,
    Pause,
    /// A step: the stepping fallback, where no hidden breakpoints cover the code.
    Step,
    /// The program's first stop (`entry`), or anything else the adapter reports.
    Other,
}

impl Why {
    /// From the reason of a DAP `stopped` event.
    pub fn from_reason(reason: &str) -> Why {
        match reason {
            "breakpoint" | "function breakpoint" | "data breakpoint" => Why::Breakpoint,
            "exception" => Why::Exception,
            "pause" => Why::Pause,
            "step" | "goto" => Why::Step,
            _ => Why::Other,
        }
    }
}

/// One hit, as kept. See [`Lineline::get`] for it with its counter, thread and path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// 0-based.
    pub line: u32,
    /// An index into [`Lineline::files`], or [`NO_FILE`].
    pub file: u16,
    /// An index into [`Lineline::threads`].
    pub thread: u16,
}

/// A hit with what its indexes stand for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitView<'a> {
    pub at: Counter,
    pub thread: ThreadKey,
    pub path: Option<&'a Path>,
    /// 0-based.
    pub line: u32,
    pub why: Why,
}

/// A thread the lineline saw: its lifetime, for painting the stretch before it existed and after
/// it exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadInfo {
    pub key: ThreadKey,
    /// The counter when the adapter reported it (or its first hit, when it reported nothing
    /// first).
    pub started: Counter,
    /// Its first and last hit, once it had one.
    pub first: Option<Counter>,
    pub last: Option<Counter>,
    /// The counter when the adapter said it exited.
    pub exited: Option<Counter>,
}

/// What a mark on the lineline says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkKind {
    /// A thread stopped at one of your breakpoints.
    Breakpoint {
        thread: u16,
    },
    Exception {
        thread: u16,
    },
    /// A pause asked for (playback's own pauses aren't marked: they stop at the next line).
    Pause {
        thread: u16,
    },
    /// Playback started following another thread.
    Switch {
        from: Option<u16>,
        to: u16,
    },
    /// The program ran at full speed here (`continue` to your next breakpoint): the lines it
    /// ran weren't counted, the counter didn't move, and this is how long it took.
    FullSpeed {
        duration: Duration,
    },
    ThreadStarted {
        thread: u16,
    },
    ThreadExited {
        thread: u16,
    },
    /// The program ended.
    End,
    /// Every file covered has its hidden breakpoints from here. Since the program started (or
    /// since the last follow switch or full-speed run), lines of the files not done yet weren't
    /// hit, nor counted: the program ran while they went in.
    Covered,
    /// The selected variable was read at this hit. Not recorded yet: debug mode's variables
    /// (selecting a variable highlights where it is read and written) add these, and the
    /// lineline has a glyph and a theme scope for them.
    Read {
        thread: u16,
    },
    /// The selected variable was written at this hit (see [`MarkKind::Read`]).
    Write {
        thread: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub at: Counter,
    pub kind: MarkKind,
}

/// The time a line of the spectated thread took: from the hit at that line to the thread's next
/// hit, minus what the hits cost. Kept with the frame the line ran in, so totals can be kept per
/// frame (recursion stays apart) and a line that calls functions can show its inclusive time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineTime {
    /// The counter of the hit that ended it (the thread's next hit).
    pub at: Counter,
    pub thread: u16,
    pub file: u16,
    /// 0-based.
    pub line: u32,
    /// An index into [`Lineline::functions`]: the function of the frame.
    pub function: u32,
    /// The stack depth of the frame (1 is the outermost).
    pub depth: u16,
    /// Corrected wall time.
    pub wall: Duration,
    /// CPU time, when it could be read.
    pub cpu: Option<Duration>,
}

/// The record. Capped: past `cap` hits the oldest go, with their reasons, marks and times.
#[derive(Debug, Clone)]
pub struct Lineline {
    /// The counter of `hits[0]`.
    base: Counter,
    hits: VecDeque<Hit>,
    /// Why the hits that weren't lines stopped, by counter, oldest first.
    reasons: VecDeque<(Counter, Why)>,
    marks: VecDeque<Mark>,
    times: VecDeque<LineTime>,
    files: Vec<Arc<Path>>,
    file_ids: HashMap<Arc<Path>, u16>,
    threads: Vec<ThreadInfo>,
    thread_ids: HashMap<ThreadKey, u16>,
    functions: Vec<String>,
    function_ids: HashMap<String, u32>,
    /// The first thread the adapter reported: the program's main thread.
    pub main_thread: Option<ThreadKey>,
    /// The spectated thread at the last hit, for the switch marks.
    spectated: Option<u16>,
    /// Hits kept at most; `None`: all of them.
    pub cap: Option<usize>,
}

impl Default for Lineline {
    fn default() -> Self {
        Self {
            base: 0,
            hits: VecDeque::new(),
            reasons: VecDeque::new(),
            marks: VecDeque::new(),
            times: VecDeque::new(),
            files: Vec::new(),
            file_ids: HashMap::new(),
            threads: Vec::new(),
            thread_ids: HashMap::new(),
            functions: Vec::new(),
            function_ids: HashMap::new(),
            main_thread: None,
            spectated: None,
            cap: Some(DEFAULT_CAP),
        }
    }
}

/// Hits kept by default: 1,000,000, about 8 MB.
pub const DEFAULT_CAP: usize = 1_000_000;

impl Lineline {
    /// Forgets everything, for a new session (the cap stays).
    pub fn clear(&mut self) {
        let cap = self.cap;
        *self = Self {
            cap,
            ..Self::default()
        };
    }

    /// The counter the next hit gets: the number of hits so far.
    pub fn counter(&self) -> Counter {
        self.base + self.hits.len() as Counter
    }

    /// The counter of the oldest hit kept.
    pub fn first_kept(&self) -> Counter {
        self.base
    }

    /// The number of hits kept.
    pub fn len(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    fn file_id(&mut self, path: &Path) -> u16 {
        if let Some(id) = self.file_ids.get(path) {
            return *id;
        }
        if self.files.len() >= NO_FILE as usize {
            return NO_FILE;
        }
        let id = self.files.len() as u16;
        let path: Arc<Path> = Arc::from(path);
        self.files.push(path.clone());
        self.file_ids.insert(path, id);
        id
    }

    /// The index of `key` in the thread table, adding it (as started now) when it's new.
    pub fn thread_id(&mut self, key: ThreadKey) -> u16 {
        if let Some(id) = self.thread_ids.get(&key) {
            return *id;
        }
        let id = self.threads.len().min(u16::MAX as usize - 1) as u16;
        if (id as usize) == self.threads.len() {
            self.threads.push(ThreadInfo {
                key,
                started: self.counter(),
                first: None,
                last: None,
                exited: None,
            });
        }
        self.thread_ids.insert(key, id);
        self.main_thread.get_or_insert(key);
        id
    }

    fn function_id(&mut self, name: &str) -> u32 {
        if let Some(id) = self.function_ids.get(name) {
            return *id;
        }
        let id = self.functions.len() as u32;
        self.functions.push(name.to_owned());
        self.function_ids.insert(name.to_owned(), id);
        id
    }

    /// Records a hit of `thread` at `location` (`None`: code without a source), and returns its
    /// counter. `spectated`: the thread playback follows now, for the switch marks.
    pub fn hit(
        &mut self,
        thread: ThreadKey,
        location: Option<(&Path, usize)>,
        why: Why,
        spectated: Option<ThreadKey>,
    ) -> Counter {
        let at = self.counter();
        let thread_id = self.thread_id(thread);
        let spectated = spectated.map(|key| self.thread_id(key));
        if spectated.is_some() && spectated != self.spectated {
            if let Some(to) = spectated {
                self.marks.push_back(Mark {
                    at,
                    kind: MarkKind::Switch {
                        from: self.spectated,
                        to,
                    },
                });
            }
            self.spectated = spectated;
        }
        let (file, line) = match location {
            Some((path, line)) => (self.file_id(path), line.min(u32::MAX as usize) as u32),
            None => (NO_FILE, 0),
        };
        self.hits.push_back(Hit {
            line,
            file,
            thread: thread_id,
        });
        if why != Why::Line {
            self.reasons.push_back((at, why));
            let kind = match why {
                Why::Breakpoint => Some(MarkKind::Breakpoint { thread: thread_id }),
                Why::Exception => Some(MarkKind::Exception { thread: thread_id }),
                Why::Pause => Some(MarkKind::Pause { thread: thread_id }),
                _ => None,
            };
            if let Some(kind) = kind {
                self.marks.push_back(Mark { at, kind });
            }
        }
        let info = &mut self.threads[thread_id as usize];
        info.first.get_or_insert(at);
        info.last = Some(at);
        self.trim();
        at
    }

    /// The adapter reported a thread starting (the first one reported is the main thread).
    pub fn thread_started(&mut self, key: ThreadKey) {
        let known = self.thread_ids.contains_key(&key);
        let id = self.thread_id(key);
        if !known {
            let at = self.counter();
            self.marks.push_back(Mark {
                at,
                kind: MarkKind::ThreadStarted { thread: id },
            });
        }
    }

    /// The adapter reported a thread exiting.
    pub fn thread_exited(&mut self, key: ThreadKey) {
        let id = self.thread_id(key);
        let at = self.counter();
        self.threads[id as usize].exited = Some(at);
        self.marks.push_back(Mark {
            at,
            kind: MarkKind::ThreadExited { thread: id },
        });
    }

    /// Gives the hit at `at` its place, once it is known (a stop that isn't a line is placed
    /// when its frames load).
    pub fn relocate(&mut self, at: Counter, path: &Path, line: usize) {
        let Some(index) = at.checked_sub(self.base).map(|index| index as usize) else {
            return;
        };
        if index >= self.hits.len() {
            return;
        }
        let file = self.file_id(path);
        let hit = &mut self.hits[index];
        hit.file = file;
        hit.line = line.min(u32::MAX as usize) as u32;
    }

    /// Adds a mark at the current counter.
    pub fn mark(&mut self, kind: MarkKind) {
        let at = self.counter();
        self.marks.push_back(Mark { at, kind });
    }

    /// Records the time a line of the spectated thread took.
    #[allow(clippy::too_many_arguments)]
    pub fn line_time(
        &mut self,
        at: Counter,
        thread: ThreadKey,
        path: &Path,
        line: usize,
        function: &str,
        depth: usize,
        wall: Duration,
        cpu: Option<Duration>,
    ) {
        let thread = self.thread_id(thread);
        let file = self.file_id(path);
        let function = self.function_id(function);
        self.times.push_back(LineTime {
            at,
            thread,
            file,
            line: line.min(u32::MAX as usize) as u32,
            function,
            depth: depth.min(u16::MAX as usize) as u16,
            wall,
            cpu,
        });
    }

    /// Drops the oldest hits over the cap, and what was recorded against them.
    fn trim(&mut self) {
        let Some(cap) = self.cap else {
            return;
        };
        let over = self.hits.len().saturating_sub(cap);
        if over == 0 {
            return;
        }
        self.hits.drain(..over);
        self.base += over as Counter;
        let base = self.base;
        while self.reasons.front().is_some_and(|(at, _)| *at < base) {
            self.reasons.pop_front();
        }
        while self.marks.front().is_some_and(|mark| mark.at < base) {
            self.marks.pop_front();
        }
        while self.times.front().is_some_and(|time| time.at < base) {
            self.times.pop_front();
        }
    }

    /// Applies a new cap at once.
    pub fn set_cap(&mut self, cap: Option<usize>) {
        self.cap = cap;
        self.trim();
    }

    /// Why the hit at `at` stopped.
    fn why(&self, at: Counter) -> Why {
        match self.reasons.binary_search_by_key(&at, |(at, _)| *at) {
            Ok(index) => self.reasons[index].1,
            Err(_) => Why::Line,
        }
    }

    fn view(&self, at: Counter, hit: &Hit) -> HitView<'_> {
        HitView {
            at,
            thread: self.threads[hit.thread as usize].key,
            path: self.files.get(hit.file as usize).map(|path| &**path),
            line: hit.line,
            why: self.why(at),
        }
    }

    /// The hit at `at`, if it is kept.
    pub fn get(&self, at: Counter) -> Option<HitView<'_>> {
        let index = at.checked_sub(self.base)? as usize;
        let hit = self.hits.get(index)?;
        Some(self.view(at, hit))
    }

    /// The hits kept, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = HitView<'_>> + '_ {
        self.hits
            .iter()
            .enumerate()
            .map(|(index, hit)| self.view(self.base + index as Counter, hit))
    }

    /// Where `thread` was at `at`: its last hit at or before it.
    pub fn where_at(&self, thread: ThreadKey, at: Counter) -> Option<HitView<'_>> {
        let id = *self.thread_ids.get(&thread)?;
        let end = (at.checked_sub(self.base)? as usize + 1).min(self.hits.len());
        (0..end)
            .rev()
            .find(|index| self.hits[*index].thread == id)
            .map(|index| self.view(self.base + index as Counter, &self.hits[index]))
    }

    /// Whether `thread` came straight from its hit at `from` to its hit at `to`: no hit of it in
    /// between, and no full-speed run (whose lines aren't counted). Rewind lends a stop's frames
    /// to the next only across such hits.
    pub fn adjacent(&self, thread: ThreadKey, from: Counter, to: Counter) -> bool {
        let Some(&id) = self.thread_ids.get(&thread) else {
            return false;
        };
        if to <= from || from < self.base || to >= self.counter() {
            return false;
        }
        let full_speed = self
            .marks
            .iter()
            .rev()
            .take_while(|mark| mark.at > from)
            .any(|mark| mark.at <= to && matches!(mark.kind, MarkKind::FullSpeed { .. }));
        let between = (from + 1 - self.base) as usize..(to - self.base) as usize;
        !full_speed && self.hits.range(between).all(|hit| hit.thread != id)
    }

    pub fn marks(&self) -> impl DoubleEndedIterator<Item = &Mark> + '_ {
        self.marks.iter()
    }

    pub fn times(&self) -> impl DoubleEndedIterator<Item = &LineTime> + '_ {
        self.times.iter()
    }

    pub fn threads(&self) -> &[ThreadInfo] {
        &self.threads
    }

    /// The index of `key` in the thread table, if the lineline saw it.
    pub fn thread_index(&self, key: ThreadKey) -> Option<u16> {
        self.thread_ids.get(&key).copied()
    }

    /// The thread index of the hit at `at`, if it is kept: cheaper than [`Lineline::get`].
    pub fn thread_at(&self, at: Counter) -> Option<u16> {
        let index = at.checked_sub(self.base)? as usize;
        self.hits.get(index).map(|hit| hit.thread)
    }

    /// The thread at index `id` of the thread table.
    pub fn thread(&self, id: u16) -> Option<&ThreadInfo> {
        self.threads.get(id as usize)
    }

    pub fn file(&self, id: u16) -> Option<&Path> {
        self.files.get(id as usize).map(|path| &**path)
    }

    pub fn function(&self, id: u32) -> Option<&str> {
        self.functions.get(id as usize).map(String::as_str)
    }

    /// About how much memory the record takes.
    pub fn bytes(&self) -> usize {
        use std::mem::size_of;
        self.hits.capacity() * size_of::<Hit>()
            + self.reasons.capacity() * size_of::<(Counter, Why)>()
            + self.marks.capacity() * size_of::<Mark>()
            + self.times.capacity() * size_of::<LineTime>()
            + self
                .files
                .iter()
                .map(|path| 2 * path.as_os_str().len() + 48)
                .sum::<usize>()
            + self.threads.capacity() * size_of::<ThreadInfo>()
            + self
                .functions
                .iter()
                .map(|name| 2 * name.len() + 48)
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vortex_dap::registry::DebugAdapterId;

    fn keys(n: usize) -> Vec<ThreadKey> {
        let mut registry: slotmap::SlotMap<DebugAdapterId, ()> = slotmap::SlotMap::with_key();
        let session = registry.insert(());
        (1..=n)
            .map(|thread| ThreadKey {
                session,
                thread: serde_json::from_value(serde_json::json!(thread)).unwrap(),
            })
            .collect()
    }

    #[test]
    fn hits_of_every_thread_share_one_counter() {
        let threads = keys(2);
        let (a, b) = (threads[0], threads[1]);
        let main = Path::new("/w/main.rs");
        let mut record = Lineline::default();
        assert_eq!(record.hit(a, Some((main, 3)), Why::Line, Some(a)), 0);
        assert_eq!(record.hit(b, Some((main, 9)), Why::Line, Some(a)), 1);
        assert_eq!(record.hit(a, Some((main, 4)), Why::Breakpoint, Some(a)), 2);
        assert_eq!(record.hit(b, None, Why::Pause, Some(b)), 3);
        assert_eq!(record.counter(), 4);
        let hit = record.get(1).unwrap();
        assert_eq!(
            (hit.thread, hit.path, hit.line, hit.why),
            (b, Some(main), 9, Why::Line)
        );
        assert_eq!(record.get(2).unwrap().why, Why::Breakpoint);
        assert_eq!(record.get(3).unwrap().path, None);
        assert_eq!(record.where_at(a, 1).unwrap().line, 3);
        assert_eq!(record.where_at(a, 3).unwrap().line, 4);
        assert_eq!(record.main_thread, Some(a));
        let info = &record.threads()[1];
        assert_eq!((info.first, info.last), (Some(1), Some(3)));
        // the first spectated thread, the breakpoint, the switch to b and the pause
        let marks: Vec<_> = record
            .marks()
            .map(|mark| (mark.at, mark.kind.clone()))
            .collect();
        assert_eq!(
            marks,
            vec![
                (0, MarkKind::Switch { from: None, to: 0 }),
                (2, MarkKind::Breakpoint { thread: 0 }),
                (
                    3,
                    MarkKind::Switch {
                        from: Some(0),
                        to: 1
                    }
                ),
                (3, MarkKind::Pause { thread: 1 }),
            ]
        );
    }

    #[test]
    fn the_cap_drops_the_oldest_hits_and_what_was_recorded_against_them() {
        let threads = keys(1);
        let a = threads[0];
        let main = Path::new("/w/main.rs");
        let mut record = Lineline {
            cap: Some(3),
            ..Default::default()
        };
        record.hit(a, Some((main, 0)), Why::Breakpoint, None);
        record.line_time(0, a, main, 0, "main", 1, Duration::from_millis(4), None);
        for line in 1..5 {
            record.hit(a, Some((main, line)), Why::Line, None);
        }
        assert_eq!(record.counter(), 5);
        assert_eq!(record.len(), 3);
        assert_eq!(record.first_kept(), 2);
        assert!(record.get(1).is_none());
        assert_eq!(record.get(2).unwrap().line, 2);
        assert_eq!(record.marks().count(), 0);
        assert_eq!(record.times().count(), 0);
        record.set_cap(Some(1));
        assert_eq!(
            record.iter().map(|hit| hit.line).collect::<Vec<_>>(),
            vec![4]
        );
    }

    #[test]
    fn a_thread_is_adjacent_across_other_threads_hits_only() {
        let threads = keys(3);
        let (main, a, b) = (threads[0], threads[1], threads[2]);
        let mut record = Lineline::default();
        let path = Path::new("/src/main.rs");
        let first = record.hit(a, Some((path, 1)), Why::Line, Some(a));
        record.hit(b, Some((path, 5)), Why::Line, Some(a));
        let second = record.hit(a, Some((path, 2)), Why::Line, Some(a));
        assert!(
            record.adjacent(a, first, second),
            "b's hit in between doesn't count"
        );
        let third = record.hit(a, Some((path, 3)), Why::Line, Some(a));
        assert!(!record.adjacent(a, first, third), "a ran line 2 in between");
        // a full-speed run in between: its lines weren't counted
        record.mark(MarkKind::FullSpeed {
            duration: Duration::from_millis(5),
        });
        let fourth = record.hit(a, Some((path, 9)), Why::Breakpoint, Some(a));
        assert!(!record.adjacent(a, third, fourth));
        assert!(!record.adjacent(main, first, second), "a thread never seen");
    }

    #[test]
    fn thread_lifetimes_are_kept() {
        let threads = keys(2);
        let (a, b) = (threads[0], threads[1]);
        let main = Path::new("/w/main.rs");
        let mut record = Lineline::default();
        record.thread_started(a);
        record.hit(a, Some((main, 1)), Why::Line, None);
        record.thread_started(b);
        record.hit(b, Some((main, 2)), Why::Line, None);
        record.hit(a, Some((main, 3)), Why::Line, None);
        record.thread_exited(b);
        record.mark(MarkKind::FullSpeed {
            duration: Duration::from_millis(20),
        });
        let info = &record.threads()[1];
        assert_eq!(
            (info.started, info.first, info.last, info.exited),
            (1, Some(1), Some(1), Some(3))
        );
        assert_eq!(record.main_thread, Some(a));
        assert!(record
            .marks()
            .any(|mark| mark.at == 3 && matches!(mark.kind, MarkKind::FullSpeed { .. })));
    }

    #[test]
    fn a_hit_takes_8_bytes() {
        assert_eq!(std::mem::size_of::<Hit>(), 8);
        let threads = keys(4);
        let main = Path::new("/w/src/main.rs");
        let mut record = Lineline::default();
        for n in 0..100_000usize {
            record.hit(threads[n % 4], Some((main, n % 300)), Why::Line, None);
        }
        // 800 KB of hits, and the vector's growth on top
        let bytes = record.bytes();
        assert!((800_000..1_700_000).contains(&bytes), "{bytes}");
    }
}
