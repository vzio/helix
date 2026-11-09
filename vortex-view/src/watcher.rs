//! A debounced, gitignore-aware directory watcher.
//!
//! Watching a tree recursively would put a watch on every directory in `target/` or
//! `node_modules/`, which can exhaust the OS watch limit. Instead one non-recursive watch is
//! placed on each directory git doesn't ignore, and directories created later are added as they
//! appear. Events are filtered with the same ignore rules, then batched so that a burst of writes
//! (a build, a checkout) results in a single callback.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ignore::gitignore::{self, Gitignore, GitignoreBuilder};
use notify::{
    event::{EventKind, ModifyKind},
    Event, RecommendedWatcher, RecursiveMode, Watcher as _,
};

/// What to watch and how to batch the changes.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// The tree watched: every directory under it that git doesn't ignore (`.gitignore` files at
    /// any depth, `.git/info/exclude`, the global excludes file), minus the `.git` directory.
    pub root: PathBuf,
    /// Directories watched on their own, non-recursively, and never filtered: e.g. the git dir,
    /// for `HEAD` and `index`, and `<git dir>/logs`.
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

enum Msg {
    Event(notify::Result<Event>),
    Stop,
}

/// Watches until dropped.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts watching. `on_change` runs on a background thread with the changed paths of a batch
    /// (absolute, deduplicated, sorted), once per debounced batch.
    ///
    /// Fails only if the root can't be watched at all. Other errors, such as hitting the OS
    /// watch limit in a subdirectory, are logged and the rest of the tree stays watched.
    pub fn spawn(
        config: WatchConfig,
        on_change: impl Fn(Vec<PathBuf>) + Send + 'static,
    ) -> anyhow::Result<Watcher> {
        let (tx, rx) = mpsc::channel();
        let notify_tx = tx.clone();
        let watcher = notify::recommended_watcher(move |res| {
            // The receiver is gone once the thread has stopped.
            let _ = notify_tx.send(Msg::Event(res));
        })?;

        let root = absolute(&config.root);
        let mut state = State {
            matchers: Matchers::new(&root),
            root,
            extra: config.extra.iter().map(|p| absolute(p)).collect(),
            watcher,
            watched: HashSet::new(),
        };
        state
            .watcher
            .watch(&state.root, RecursiveMode::NonRecursive)?;
        state.watched.insert(state.root.clone());
        state.scan(&state.root.clone(), None);
        for dir in state.extra.clone() {
            if let Err(err) = state.watcher.watch(&dir, RecursiveMode::NonRecursive) {
                log::warn!("failed to watch {}: {err}", dir.display());
            }
        }

        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let (debounce, max_wait) = (config.debounce, config.max_wait);
            thread::Builder::new()
                .name("file-watcher".into())
                .spawn(move || run(state, rx, stop, debounce, max_wait, on_change))?
        };
        Ok(Watcher {
            stop,
            tx,
            thread: Some(thread),
        })
    }
}

impl Drop for Watcher {
    /// Stops the watcher and waits for its thread, so `on_change` doesn't run after this returns.
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

/// The debounce loop. Owns the notify watcher, so it is stopped when the loop ends.
fn run(
    mut state: State,
    rx: mpsc::Receiver<Msg>,
    stop: Arc<AtomicBool>,
    debounce: Duration,
    max_wait: Duration,
    on_change: impl Fn(Vec<PathBuf>),
) {
    let mut pending = BTreeSet::new();
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

struct State {
    root: PathBuf,
    extra: Vec<PathBuf>,
    watcher: RecommendedWatcher,
    /// Directories under `root` with a watch.
    watched: HashSet<PathBuf>,
    matchers: Matchers,
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
            if self.extra.iter().any(|dir| path.starts_with(dir)) {
                pending.insert(path);
                kept = true;
                continue;
            }
            if !path.starts_with(&self.root) {
                continue;
            }
            if path.file_name().is_some_and(|name| name == ".gitignore") {
                self.matchers
                    .invalidate(path.parent().unwrap_or(&self.root));
            }
            let exists = path.symlink_metadata();
            let is_dir = exists.as_ref().is_ok_and(|m| m.is_dir()) || self.watched.contains(&path);
            if self.matchers.is_ignored(&path, is_dir) {
                continue;
            }
            if exists.is_err() {
                self.forget(&path);
            } else if created && is_dir {
                // Files may have been written before the watch was in place.
                self.scan(&path, Some(pending));
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

    /// Watches `dir` and every non-ignored directory below it. The non-ignored files found are
    /// added to `report`. I do the walk by hand instead of with `ignore::WalkBuilder` so that it
    /// applies exactly the rules `Matchers` applies to events.
    fn scan(&mut self, dir: &Path, mut report: Option<&mut BTreeSet<PathBuf>>) {
        if dir != self.root {
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
                self.scan(&path, report.as_deref_mut());
            } else if let Some(report) = report.as_deref_mut() {
                report.insert(path);
            }
        }
    }
}

/// Git's ignore rules for the tree under a root, with per-directory `.gitignore` files cached.
struct Matchers {
    root: PathBuf,
    /// `.git/info/exclude` and the global excludes file, in order of increasing precedence.
    base: Vec<Gitignore>,
    /// The `.gitignore` of each directory seen, empty if it has none.
    dirs: HashMap<PathBuf, Gitignore>,
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
        }
    }

    /// Forgets the cached `.gitignore` of `dir`, which changed.
    fn invalidate(&mut self, dir: &Path) {
        self.dirs.remove(dir);
    }

    /// Forgets all cached `.gitignore` files in and below `dir`.
    fn forget_below(&mut self, dir: &Path) {
        self.dirs.retain(|d, _| !d.starts_with(dir));
    }

    fn dir_matcher(&mut self, dir: &Path) -> &Gitignore {
        self.dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| load(dir, &dir.join(".gitignore")))
    }

    /// Whether git would ignore `path`. A path is ignored when it or any directory above it
    /// (below the root) is, unless a closer rule re-includes it. `.git` is always ignored.
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
            let is_dir = i + 1 < count || is_dir;
            if self.ignored_in(&parent, &prefix, is_dir) {
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
        (watcher, rx)
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
}
