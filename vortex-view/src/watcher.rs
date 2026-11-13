//! A debounced, gitignore-aware directory watcher.
//!
//! How the tree is watched depends on what the backend can do (see [`mode`]):
//!
//! - **One directory at a time** (inotify on Linux, and every backend but FSEvents): watching a
//!   tree recursively would put a watch on every directory in `target/` or `node_modules/`, which
//!   can exhaust the OS watch limit. Instead one non-recursive watch is placed on each directory
//!   git doesn't ignore, and directories created later are added as they appear. The walk that
//!   places them runs on the watcher's own thread: [`Watcher::spawn`] returns at once, and
//!   [`Watcher::is_ready`] tells when the whole tree is watched.
//! - **The whole tree at once** (FSEvents on macOS): the backend watches a tree natively, for the
//!   cost of one watch, and every extra watch would restart its event stream. So the root gets a
//!   single recursive watch and there is no walk.
//!
//! Either way events are filtered with the same rules (git's ignore rules, `.git`, and nested
//! repositories, whose files aren't this repository's), then batched so that a burst of writes
//! (a build, a checkout) results in a single callback.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use ignore::gitignore::{self, Gitignore, GitignoreBuilder};
use notify::{
    event::{EventKind, ModifyKind},
    Event, RecommendedWatcher, RecursiveMode, Watcher as _, WatcherKind,
};

/// What to watch and how to batch the changes.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// The tree watched: every directory under it that git doesn't ignore (`.gitignore` files at
    /// any depth, `.git/info/exclude`, the global excludes file), minus the `.git` directory and
    /// nested repositories (a directory below the root holding a `.git` file or directory: a
    /// submodule, or a repository cloned inside this one).
    pub root: PathBuf,
    /// Directories watched on their own, non-recursively, and never filtered: e.g. the git dir,
    /// for `HEAD` and `index`, and `<git dir>/logs`. Only their direct entries are reported.
    pub extra: Vec<PathBuf>,
    /// Quiet time after the last event before `on_change` runs.
    pub debounce: Duration,
    /// Longest time `on_change` waits during a stream of events that never goes quiet.
    pub max_wait: Duration,
}

impl WatchConfig {
    /// A config for `root` with a 200ms debounce, a 1s maximum wait and no extra directories.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            extra: Vec::new(),
            debounce: Duration::from_millis(200),
            max_wait: Duration::from_secs(1),
        }
    }
}

/// How a tree is watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One recursive watch on the root.
    Recursive,
    /// One non-recursive watch on each directory that isn't ignored, placed by a walk of the
    /// tree and, for directories created later, as they appear.
    PerDirectory,
}

/// How a tree is watched with a backend of `kind`: by its capability, not by the OS.
///
/// FSEvents watches a whole tree natively, for the cost of one watch, and notify's backend
/// restarts its event stream on every `watch` call, registering again every path watched so far:
/// a watch per directory costs a quadratic number of registrations (about 10^8 for 16,000
/// directories), on top of the walk. So it gets one recursive watch, and filtering the events of
/// ignored directories is cheap next to that.
///
/// Every other backend gets a watch per directory. inotify's recursive mode is notify walking
/// the tree and adding a watch per directory anyway, ignored ones included (`target/`,
/// `node_modules/`), each taking a slot of the per-user limit; kqueue holds a file descriptor
/// per watched file; polling reads every file it watches. Walking only what git doesn't ignore
/// keeps that small.
pub fn mode(kind: WatcherKind) -> Mode {
    match kind {
        WatcherKind::Fsevent => Mode::Recursive,
        _ => Mode::PerDirectory,
    }
}

/// When a file found by the first walk counts as changed since [`Watcher::spawn`]: modified at
/// most this long before it. File times come from a coarser clock than [`SystemTime::now`]
/// (up to a scheduler tick behind), so a file written just after `spawn` may look a little older.
const CLOCK_SLACK: Duration = Duration::from_millis(100);

enum Msg {
    Event(notify::Result<Event>),
    Stop,
}

/// Whether the whole tree is watched yet.
#[derive(Default)]
struct Ready {
    done: Mutex<bool>,
    signal: Condvar,
}

impl Ready {
    fn set(&self) {
        *self.done.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.signal.notify_all();
    }

    fn get(&self) -> bool {
        *self.done.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait(&self, timeout: Duration) -> bool {
        let done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        let (done, _) = self
            .signal
            .wait_timeout_while(done, timeout, |done| !*done)
            .unwrap_or_else(|e| e.into_inner());
        *done
    }
}

/// Watches until dropped.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Msg>,
    ready: Arc<Ready>,
    thread: Option<JoinHandle<()>>,
    /// The directories with a watch of their own once ready (the root's tree only).
    #[cfg(test)]
    watched: Arc<Mutex<Vec<PathBuf>>>,
}

impl Watcher {
    /// Starts watching and returns at once. `on_change` runs on a background thread with the
    /// changed paths of a batch (absolute, deduplicated, sorted), once per debounced batch.
    ///
    /// The root and the extra directories are watched before this returns, which takes one
    /// watch each. With a watch per directory ([`Mode::PerDirectory`]), the rest of the tree is
    /// walked and watched on the watcher's thread; until [`Watcher::is_ready`], a change in a
    /// directory the walk hasn't reached has no event, but it isn't lost: the walk reports the
    /// files and directories modified since this was called, in the first batch. Events that
    /// arrive during the walk are handled after it.
    ///
    /// Fails only if the root can't be watched at all. Other errors, such as hitting the OS
    /// watch limit in a subdirectory, are logged and the rest of the tree stays watched.
    pub fn spawn(
        config: WatchConfig,
        on_change: impl Fn(Vec<PathBuf>) + Send + 'static,
    ) -> anyhow::Result<Watcher> {
        let since = SystemTime::now();
        let (tx, rx) = mpsc::channel();
        let notify_tx = tx.clone();
        let watcher = notify::recommended_watcher(move |res| {
            // The receiver is gone once the thread has stopped.
            let _ = notify_tx.send(Msg::Event(res));
        })?;

        let root = absolute(&config.root);
        let mode = mode(RecommendedWatcher::kind());
        let stop = Arc::new(AtomicBool::new(false));
        let mut state = State {
            matchers: Matchers::new(&root),
            mode,
            extra: config.extra.iter().map(|p| absolute(p)).collect(),
            root,
            watcher,
            watched: HashSet::new(),
            stop: stop.clone(),
        };
        let recursive = match mode {
            Mode::Recursive => RecursiveMode::Recursive,
            Mode::PerDirectory => RecursiveMode::NonRecursive,
        };
        state.watcher.watch(&state.root, recursive)?;
        for dir in state.extra.clone() {
            // a recursive watch on the root already sees the git dir when it is inside
            if mode == Mode::Recursive && dir.starts_with(&state.root) {
                continue;
            }
            if let Err(err) = state.watcher.watch(&dir, RecursiveMode::NonRecursive) {
                log::warn!("failed to watch {}: {err}", dir.display());
            }
        }

        let ready = Arc::new(Ready::default());
        #[cfg(test)]
        let watched = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let ready = ready.clone();
            #[cfg(test)]
            let watched = watched.clone();
            let (debounce, max_wait) = (config.debounce, config.max_wait);
            let stop = stop.clone();
            thread::Builder::new()
                .name("file-watcher".into())
                .spawn(move || {
                    let mut pending = BTreeSet::new();
                    if state.mode == Mode::PerDirectory {
                        let started = Instant::now();
                        let since = since.checked_sub(CLOCK_SLACK).unwrap_or(since);
                        let root = state.root.clone();
                        state.scan(&root, Report::ChangedSince(since, &mut pending));
                        log::debug!(
                            "watching {} directories under {} after {:?}",
                            state.watched.len() + 1,
                            state.root.display(),
                            started.elapsed()
                        );
                    }
                    #[cfg(test)]
                    {
                        *watched.lock().unwrap() = state.watched.iter().cloned().collect();
                    }
                    ready.set();
                    run(state, rx, pending, stop, debounce, max_wait, on_change)
                })?
        };
        Ok(Watcher {
            stop,
            tx,
            ready,
            thread: Some(thread),
            #[cfg(test)]
            watched,
        })
    }

    /// Whether the whole tree is watched: the walk placing a watch on each directory has ended
    /// (at once with a recursive watch).
    pub fn is_ready(&self) -> bool {
        self.ready.get()
    }

    /// Waits until [`Self::is_ready`], at most `timeout`. Returns whether it is.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        self.ready.wait(timeout)
    }
}

impl Drop for Watcher {
    /// Stops the watcher and waits for its thread, so `on_change` doesn't run after this returns.
    /// A walk still running stops at the next directory.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            // If `on_change` itself drops the watcher, joining would deadlock; the stop flag
            // still prevents any further callback.
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The debounce loop. Owns the notify watcher, so it is stopped when the loop ends. `pending`
/// holds what the first walk found changed.
fn run(
    mut state: State,
    rx: mpsc::Receiver<Msg>,
    mut pending: BTreeSet<PathBuf>,
    stop: Arc<AtomicBool>,
    debounce: Duration,
    max_wait: Duration,
    on_change: impl Fn(Vec<PathBuf>),
) {
    // When the current batch started and when its last event arrived.
    let mut first = Instant::now();
    let mut last = first;

    loop {
        let msg = if pending.is_empty() {
            match rx.recv() {
                Ok(msg) => Some(msg),
                Err(_) => return,
            }
        } else {
            let deadline = (last + debounce).min(first + max_wait);
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(msg) => Some(msg),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };

        match msg {
            Some(Msg::Stop) => return,
            Some(Msg::Event(res)) => {
                match res {
                    Ok(event) => {
                        if pending.is_empty() {
                            first = Instant::now();
                        }
                        if state.handle(event, &mut pending) {
                            // Only events we keep restart the quiet period.
                            last = Instant::now();
                        }
                    }
                    Err(err) => log::warn!("file watcher error: {err}"),
                }
                if pending.is_empty() || Instant::now() < first + max_wait {
                    continue;
                }
            }
            None => {}
        }

        if stop.load(Ordering::SeqCst) {
            return;
        }
        on_change(std::mem::take(&mut pending).into_iter().collect());
    }
}

/// What a walk adds to the batch.
enum Report<'a> {
    /// Every file found: a new directory, whose files may have been written before its watch.
    Everything(&'a mut BTreeSet<PathBuf>),
    /// The files and directories modified since then: the first walk, which places the watches
    /// while the tree may already be changing.
    ChangedSince(SystemTime, &'a mut BTreeSet<PathBuf>),
}

impl Report<'_> {
    fn reborrow(&mut self) -> Report<'_> {
        match self {
            Report::Everything(paths) => Report::Everything(paths),
            Report::ChangedSince(since, paths) => Report::ChangedSince(*since, paths),
        }
    }

    fn add(&mut self, path: &Path) {
        match self {
            Report::Everything(paths) => {
                paths.insert(path.to_path_buf());
            }
            Report::ChangedSince(since, paths) => {
                let modified = path.symlink_metadata().and_then(|m| m.modified());
                if modified.is_ok_and(|modified| modified >= *since) {
                    paths.insert(path.to_path_buf());
                }
            }
        }
    }
}

struct State {
    root: PathBuf,
    mode: Mode,
    extra: Vec<PathBuf>,
    watcher: RecommendedWatcher,
    /// Directories under `root` with a watch of their own (none with a recursive watch).
    watched: HashSet<PathBuf>,
    matchers: Matchers,
    /// Set when the [`Watcher`] is dropped: a walk still running stops.
    stop: Arc<AtomicBool>,
}

impl State {
    /// Filters one notify event into `pending` and keeps the set of watched directories current.
    /// Returns whether any path was kept.
    fn handle(&mut self, event: Event, pending: &mut BTreeSet<PathBuf>) -> bool {
        let mut kept = false;
        let created = match event.kind {
            EventKind::Access(_) => return false,
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_)) => true,
            _ => false,
        };
        for path in event.paths {
            // only the direct entries: with a recursive watch on the root, the git dir's objects
            // and refs would be reported too
            let parent = path.parent();
            if self
                .extra
                .iter()
                .any(|dir| path == *dir || parent == Some(dir.as_path()))
            {
                pending.insert(path);
                kept = true;
                continue;
            }
            if !path.starts_with(&self.root) {
                continue;
            }
            let name = path.file_name();
            if name.is_some_and(|name| name == ".gitignore") {
                self.matchers.invalidate(parent.unwrap_or(&self.root));
            }
            if name.is_some_and(|name| name == ".git") {
                if let Some(dir) = parent.filter(|dir| *dir != self.root) {
                    // a repository was created or cloned in `dir`, or is gone from it
                    let dir = dir.to_path_buf();
                    self.matchers.invalidate_nested(&dir);
                    if self.matchers.is_nested(&dir) {
                        self.forget(&dir);
                    }
                }
            }
            let exists = path.symlink_metadata();
            let is_dir = exists.as_ref().is_ok_and(|m| m.is_dir()) || self.watched.contains(&path);
            if self.matchers.is_ignored(&path, is_dir) {
                continue;
            }
            if exists.is_err() {
                self.forget(&path);
            } else if created && is_dir && self.mode == Mode::PerDirectory {
                // Files may have been written before the watch was in place.
                self.scan(&path, Report::Everything(pending));
            }
            pending.insert(path);
            kept = true;
        }
        kept
    }

    /// Drops the watches of `dir` and everything below it.
    fn forget(&mut self, dir: &Path) {
        let gone: Vec<_> = self
            .watched
            .iter()
            .filter(|w| w.starts_with(dir))
            .cloned()
            .collect();
        for w in gone {
            // The OS usually removed the watch with the directory already.
            let _ = self.watcher.unwatch(&w);
            self.watched.remove(&w);
        }
        self.matchers.forget_below(dir);
    }

    /// Watches `dir` and every directory below it that isn't ignored, and adds the files found
    /// to `report`. A nested repository is neither watched nor walked: the watch of the
    /// directory holding it still sees its own entry change. I do the walk by hand instead of
    /// with `ignore::WalkBuilder` so that it applies exactly the rules `Matchers` applies to
    /// events.
    fn scan(&mut self, dir: &Path, mut report: Report) {
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        if dir != self.root {
            if self.matchers.is_nested(dir) {
                return;
            }
            if let Err(err) = self.watcher.watch(dir, RecursiveMode::NonRecursive) {
                log::warn!("failed to watch {}: {err}", dir.display());
                return;
            }
            self.watched.insert(dir.to_path_buf());
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                log::warn!("failed to read {}: {err}", dir.display());
                return;
            }
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let is_dir = file_type.is_dir();
            if self.matchers.is_ignored(&path, is_dir) {
                continue;
            }
            if is_dir {
                // a file deleted from a directory not watched yet only shows in its time
                if let Report::ChangedSince(..) = report {
                    report.add(&path);
                }
                self.scan(&path, report.reborrow());
            } else {
                report.add(&path);
            }
        }
    }
}

/// Git's ignore rules for the tree under a root, with per-directory `.gitignore` files cached,
/// and which directories below it are nested repositories.
struct Matchers {
    root: PathBuf,
    /// `.git/info/exclude` and the global excludes file, in order of increasing precedence.
    base: Vec<Gitignore>,
    /// The `.gitignore` of each directory seen, empty if it has none.
    dirs: HashMap<PathBuf, Gitignore>,
    /// Whether each directory seen holds a `.git`.
    nested: HashMap<PathBuf, bool>,
}

impl Matchers {
    fn new(root: &Path) -> Self {
        let mut base = Vec::new();
        if let Some(global) = gitignore::gitconfig_excludes_path() {
            base.push(load(root, &global));
        }
        if let Some(exclude) = git_dir(root).map(|dir| dir.join("info").join("exclude")) {
            base.push(load(root, &exclude));
        }
        Self {
            root: root.to_path_buf(),
            base,
            dirs: HashMap::new(),
            nested: HashMap::new(),
        }
    }

    /// Forgets the cached `.gitignore` of `dir`, which changed.
    fn invalidate(&mut self, dir: &Path) {
        self.dirs.remove(dir);
    }

    /// Forgets all cached `.gitignore` files in and below `dir`, and whether they are nested
    /// repositories.
    fn forget_below(&mut self, dir: &Path) {
        self.dirs.retain(|d, _| !d.starts_with(dir));
        self.nested.retain(|d, _| !d.starts_with(dir));
    }

    /// Forgets whether `dir` is a nested repository: its `.git` was created or removed.
    fn invalidate_nested(&mut self, dir: &Path) {
        self.nested.remove(dir);
    }

    /// Whether `dir`, below the root, is a nested repository: it holds a `.git` (a directory, or
    /// the file of a submodule or a linked worktree). git doesn't count the files in it as this
    /// repository's: a submodule is one entry of the index, any other nested repository one
    /// untracked directory.
    fn is_nested(&mut self, dir: &Path) -> bool {
        if dir == self.root || !dir.starts_with(&self.root) {
            return false;
        }
        *self
            .nested
            .entry(dir.to_path_buf())
            .or_insert_with(|| dir.join(".git").symlink_metadata().is_ok())
    }

    fn dir_matcher(&mut self, dir: &Path) -> &Gitignore {
        self.dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| load(dir, &dir.join(".gitignore")))
    }

    /// Whether git would ignore `path`. A path is ignored when it or any directory above it
    /// (below the root) is, unless a closer rule re-includes it. `.git` is always ignored, and so
    /// is everything inside a nested repository (but not the nested repository's own entry).
    fn is_ignored(&mut self, path: &Path, is_dir: bool) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };
        let count = rel.components().count();
        let mut prefix = self.root.clone();
        for (i, component) in rel.components().enumerate() {
            let parent = prefix.clone();
            prefix.push(component);
            if component.as_os_str() == ".git" {
                return true;
            }
            let inside = i + 1 < count;
            if self.ignored_in(&parent, &prefix, inside || is_dir) {
                return true;
            }
            if inside && self.is_nested(&prefix) {
                return true;
            }
        }
        false
    }

    /// Checks one path against the rules that apply in `parent`, the closest `.gitignore` first.
    fn ignored_in(&mut self, parent: &Path, path: &Path, is_dir: bool) -> bool {
        let mut dir = parent.to_path_buf();
        loop {
            let m = self.dir_matcher(&dir).matched(path, is_dir);
            if !m.is_none() {
                return m.is_ignore();
            }
            if dir == self.root || !dir.pop() {
                break;
            }
        }
        for matcher in self.base.iter().rev() {
            let m = matcher.matched(path, is_dir);
            if !m.is_none() {
                return m.is_ignore();
            }
        }
        false
    }
}

/// Loads the ignore file `file`, whose patterns are relative to `base`. Missing or broken files
/// give an empty (or partial) matcher.
fn load(base: &Path, file: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(base);
    if file.is_file() {
        if let Some(err) = builder.add(file) {
            log::debug!("error in {}: {err}", file.display());
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// The git directory of the repository at `root`, following a `.git` file in linked worktrees.
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let dir = Path::new(text.strip_prefix("gitdir:")?.trim());
    let dir = root.join(dir);
    // `info/exclude` is shared by all worktrees and lives in the common dir.
    match fs::read_to_string(dir.join("commondir")) {
        Ok(common) => Some(dir.join(common.trim())),
        Err(_) => Some(dir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    const WAIT: Duration = Duration::from_secs(5);
    const QUIET: Duration = Duration::from_millis(600);

    fn config(root: &Path) -> WatchConfig {
        let mut config = WatchConfig::new(root.to_path_buf());
        config.debounce = Duration::from_millis(100);
        config.max_wait = Duration::from_millis(500);
        config
    }

    fn start(config: WatchConfig) -> (Watcher, Receiver<Vec<PathBuf>>) {
        let (tx, rx) = mpsc::channel();
        let watcher = Watcher::spawn(config, move |paths| {
            let _ = tx.send(paths);
        })
        .unwrap();
        assert!(watcher.wait_ready(WAIT), "the walk didn't end in {WAIT:?}");
        (watcher, rx)
    }

    /// Creates `dirs` directories fanned out as `d{i/100:03}/e{i%100:02}`: the `dirs` count the
    /// leaves only, so 5000 of them are 50 top directories of 100 each (and 5050 in all).
    fn tree(root: &Path, dirs: usize) {
        for i in 0..dirs {
            if i % 100 == 0 {
                fs::create_dir(root.join(format!("d{:03}", i / 100))).unwrap();
            }
            fs::create_dir(root.join(format!("d{:03}/e{:02}", i / 100, i % 100))).unwrap();
        }
    }

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        (dir, path)
    }

    /// Waits until a batch containing `path` arrives.
    fn expect(rx: &Receiver<Vec<PathBuf>>, path: &Path) {
        let end = Instant::now() + WAIT;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Ok(paths) = rx.recv_timeout(left) {
                if paths.contains(&path.to_path_buf()) {
                    return;
                }
            }
        }
        panic!("no change reported for {}", path.display());
    }

    /// Waits until the watcher has been quiet for a while: on macOS FSEvents also delivers what
    /// happened in the tree just before the watch started (the tempdir, the setup's files), so a
    /// test that expects silence settles first.
    fn settle(rx: &Receiver<Vec<PathBuf>>) {
        while rx.recv_timeout(QUIET).is_ok() {}
    }

    fn expect_quiet(rx: &Receiver<Vec<PathBuf>>) {
        if let Ok(paths) = rx.recv_timeout(QUIET) {
            panic!("unexpected change: {paths:?}");
        }
    }

    #[test]
    fn reports_write() {
        let (_guard, root) = root();
        let (_w, rx) = start(config(&root));
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let paths = rx.recv_timeout(WAIT).unwrap();
        assert!(paths.contains(&file), "{paths:?}");
    }

    #[test]
    fn reports_hidden_file() {
        let (_guard, root) = root();
        let (_w, rx) = start(config(&root));
        let file = root.join(".env");
        fs::write(&file, "x").unwrap();
        expect(&rx, &file);
    }

    #[test]
    fn ignores_ignored_files() {
        let (_guard, root) = root();
        fs::write(root.join(".gitignore"), "*.log\ntarget/\n").unwrap();
        fs::create_dir_all(root.join("sub/inner")).unwrap();
        fs::write(root.join("sub/.gitignore"), "inner/\n").unwrap();
        let (_w, rx) = start(config(&root));
        settle(&rx);

        fs::write(root.join("a.log"), "x").unwrap();
        expect_quiet(&rx);
        fs::write(root.join("sub/inner/f"), "x").unwrap();
        expect_quiet(&rx);
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/out"), "x").unwrap();
        expect_quiet(&rx);

        // Others are still reported.
        let file = root.join("sub/ok");
        fs::write(&file, "x").unwrap();
        expect(&rx, &file);
    }

    #[test]
    fn reports_new_directory_contents() {
        let (_guard, root) = root();
        let (_w, rx) = start(config(&root));
        fs::create_dir(root.join("new")).unwrap();
        let file = root.join("new/f");
        fs::write(&file, "x").unwrap();
        expect(&rx, &file);
        // The directory is watched from now on.
        while rx.recv_timeout(QUIET).is_ok() {}
        fs::write(&file, "y").unwrap();
        expect(&rx, &file);
    }

    #[test]
    fn debounces_bursts() {
        let (_guard, root) = root();
        let (_w, rx) = start(config(&root));
        let file = root.join("a.txt");
        for i in 0..20 {
            fs::write(&file, i.to_string()).unwrap();
        }
        rx.recv_timeout(WAIT).unwrap();
        let mut batches = 1;
        while rx.recv_timeout(QUIET).is_ok() {
            batches += 1;
        }
        assert!((1..=2).contains(&batches), "{batches} batches");
    }

    #[test]
    fn max_wait_during_endless_stream() {
        let (_guard, root) = root();
        let (_w, rx) = start(config(&root));
        let file = root.join("a.txt");
        let start = Instant::now();
        let mut first_batch = None;
        let mut batches = 0;
        let mut i = 0;
        while start.elapsed() < Duration::from_secs(2) {
            fs::write(&file, i.to_string()).unwrap();
            i += 1;
            thread::sleep(Duration::from_millis(50));
            while rx.try_recv().is_ok() {
                batches += 1;
                first_batch.get_or_insert(start.elapsed());
            }
        }
        assert!(batches >= 2, "{batches} batches");
        assert!(first_batch.unwrap() < Duration::from_millis(1500));
    }

    #[test]
    fn extra_is_unfiltered() {
        let (_guard, root) = root();
        fs::write(root.join(".gitignore"), "gitdir/\n").unwrap();
        let extra = root.join("gitdir");
        fs::create_dir(&extra).unwrap();
        let mut config = config(&root);
        config.extra.push(extra.clone());
        let (_w, rx) = start(config);
        let file = extra.join("HEAD");
        fs::write(&file, "x").unwrap();
        expect(&rx, &file);
    }

    #[test]
    fn silent_after_drop() {
        let (_guard, root) = root();
        let (w, rx) = start(config(&root));
        drop(w);
        fs::write(root.join("a.txt"), "x").unwrap();
        expect_quiet(&rx);
    }

    #[test]
    fn mode_follows_the_backend_capability() {
        assert_eq!(mode(WatcherKind::Fsevent), Mode::Recursive);
        for kind in [
            WatcherKind::Inotify,
            WatcherKind::Kqueue,
            WatcherKind::PollWatcher,
            WatcherKind::ReadDirectoryChangesWatcher,
            WatcherKind::NullWatcher,
        ] {
            assert_eq!(mode(kind), Mode::PerDirectory, "{kind:?}");
        }
    }

    #[test]
    fn spawn_returns_before_walking_and_nothing_is_lost() {
        let (_guard, root) = root();
        tree(&root, 5000);
        let (tx, rx) = mpsc::channel();
        let begin = Instant::now();
        let w = Watcher::spawn(config(&root), move |paths| {
            let _ = tx.send(paths);
        })
        .unwrap();
        let spawned = begin.elapsed();
        // Changes while the walk may still be running.
        let files: Vec<PathBuf> = (0..5000)
            .step_by(125)
            .map(|i| root.join(format!("d{:03}/e{:02}/f", i / 100, i % 100)))
            .collect();
        assert_eq!(files.len(), 40);
        for file in &files {
            fs::write(file, "x").unwrap();
        }
        let mut missing: HashSet<&PathBuf> = files.iter().collect();
        let end = Instant::now() + Duration::from_secs(20);
        while !missing.is_empty() {
            let left = end
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("not reported in 20s: {missing:?}"));
            if let Ok(paths) = rx.recv_timeout(left) {
                for path in &paths {
                    missing.remove(path);
                }
            }
        }
        assert!(w.wait_ready(Duration::from_secs(20)));
        println!(
            "spawn took {spawned:?}, ready (and all reported) after {:?}",
            begin.elapsed()
        );
        assert!(
            spawned < Duration::from_millis(100),
            "spawn took {spawned:?}"
        );
        settle(&rx);
        let deep = root.join("d049/e99/late");
        fs::write(&deep, "x").unwrap();
        expect(&rx, &deep);
    }

    #[test]
    fn dropping_during_the_walk_stops_it() {
        let (_guard, root) = root();
        tree(&root, 5000);
        let (tx, rx) = mpsc::channel();
        let w = Watcher::spawn(config(&root), move |paths| {
            let _ = tx.send(paths);
        })
        .unwrap();
        let begin = Instant::now();
        drop(w);
        let dropped = begin.elapsed();
        println!("drop took {dropped:?}");
        assert!(dropped < Duration::from_secs(1), "drop took {dropped:?}");
        fs::write(root.join("d010/e10/f"), "x").unwrap();
        expect_quiet(&rx);
    }

    #[test]
    fn nested_repositories_are_neither_watched_nor_walked() {
        let (_guard, root) = root();
        fs::create_dir_all(root.join("sub-file/inner")).unwrap();
        fs::write(
            root.join("sub-file/.git"),
            "gitdir: ../.git/modules/sub-file\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("sub-dir/inner")).unwrap();
        fs::create_dir(root.join("sub-dir/.git")).unwrap();
        fs::create_dir_all(root.join("plain/inner")).unwrap();
        let (w, rx) = start(config(&root));
        settle(&rx);

        let watched: Vec<PathBuf> = w.watched.lock().unwrap().clone();
        if mode(RecommendedWatcher::kind()) == Mode::PerDirectory {
            assert!(watched.contains(&root.join("plain")), "{watched:?}");
            assert!(watched.contains(&root.join("plain/inner")), "{watched:?}");
            assert!(
                !watched
                    .iter()
                    .any(|d| d.starts_with(root.join("sub-file"))
                        || d.starts_with(root.join("sub-dir"))),
                "{watched:?}"
            );
        } else {
            assert!(watched.is_empty(), "{watched:?}");
        }

        fs::write(root.join("sub-file/inner/x"), "x").unwrap();
        fs::write(root.join("sub-dir/x"), "x").unwrap();
        fs::write(root.join("sub-dir/inner/y"), "x").unwrap();
        expect_quiet(&rx);

        let next = root.join("next.txt");
        let z = root.join("plain/inner/z");
        fs::write(&next, "x").unwrap();
        fs::write(&z, "x").unwrap();
        let mut seen = HashSet::new();
        let end = Instant::now() + WAIT;
        while !(seen.contains(&next) && seen.contains(&z)) {
            let left = end
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("not reported: {seen:?}"));
            if let Ok(paths) = rx.recv_timeout(left) {
                for path in &paths {
                    assert!(
                        !path.starts_with(root.join("sub-file"))
                            && !path.starts_with(root.join("sub-dir")),
                        "{paths:?}"
                    );
                }
                seen.extend(paths);
            }
        }
    }

    #[test]
    fn a_directory_becoming_a_repository_is_dropped() {
        let (_guard, root) = root();
        let (_w, rx) = start(config(&root));
        settle(&rx);

        fs::create_dir(root.join("newrepo")).unwrap();
        let a = root.join("newrepo/a");
        fs::write(&a, "x").unwrap();
        expect(&rx, &a);
        settle(&rx);

        fs::create_dir(root.join("newrepo/.git")).unwrap();
        settle(&rx);

        fs::write(root.join("newrepo/b"), "x").unwrap();
        expect_quiet(&rx);
    }

    #[test]
    fn events_inside_nested_repositories_are_ignored() {
        let (_guard, root) = root();
        fs::create_dir_all(root.join("sub/deep")).unwrap();
        fs::write(root.join("sub/.git"), "gitdir: ../.git/modules/sub\n").unwrap();
        fs::create_dir(root.join("other")).unwrap();
        let mut m = Matchers::new(&root);
        assert!(!m.is_ignored(&root.join("sub"), true));
        assert!(m.is_ignored(&root.join("sub/a.txt"), false));
        assert!(m.is_ignored(&root.join("sub/deep/x"), false));
        assert!(!m.is_ignored(&root.join("other/x"), false));
        assert!(!m.is_ignored(&root.join("a.txt"), false));

        // The root being a repository itself doesn't ignore its files.
        fs::create_dir(root.join(".git")).unwrap();
        let mut m = Matchers::new(&root);
        assert!(!m.is_ignored(&root.join("a.txt"), false));
        assert!(!m.is_ignored(&root.join("other/x"), false));
        assert!(m.is_ignored(&root.join(".git/HEAD"), false));
    }

    #[test]
    fn extra_reports_only_its_direct_entries() {
        let (_guard, root) = root();
        fs::write(root.join(".gitignore"), "gitdir/\n").unwrap();
        let extra = root.join("gitdir");
        fs::create_dir_all(extra.join("objects/ab")).unwrap();
        let mut config = config(&root);
        config.extra.push(extra.clone());
        let (_w, rx) = start(config);
        settle(&rx);

        fs::write(extra.join("objects/ab/x"), "x").unwrap();
        expect_quiet(&rx);
        let head = extra.join("HEAD");
        fs::write(&head, "x").unwrap();
        expect(&rx, &head);
    }
}
