//! Views that update while the working tree changes: one watch on the repository, shared by the
//! views that show it, which stops when the last of them closes.
//!
//! The watch only says *that* something changed: each view keeps a [`Live`] and, when it renders
//! and sees a change, rebuilds what it shows, in the background with [`Pending`] when that is
//! slow (`git status`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use vortex_view::watcher::{WatchConfig, Watcher};
use vortex_view::Editor;

use crate::job;

/// Quiet time after the last file event before the views refresh.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Sees a batch of changed paths on the watcher's thread; what it returns runs on the main
/// thread, see [`RepoWatch::start_tapped`].
pub type Tap = Box<dyn Fn(&[PathBuf]) -> Box<dyn FnOnce(&mut Editor) + Send> + Send>;

/// A watch on the working tree of a repository (what git doesn't ignore, nested repositories left
/// out), on its `HEAD` and on its index. Shared with [`Arc`]: it stops when the last view holding
/// it is dropped.
///
/// Starting it never walks the tree on the caller's thread (see [`Watcher::spawn`]): a change
/// made before the whole tree is watched comes in the first batch, so the views don't wait for
/// the watch to be ready.
pub struct RepoWatch {
    /// Counts the batches of changes. Bumped on the main thread, after the open buffers of the
    /// changed files were checked, so a view that refreshes sees reloaded buffers.
    generation: Arc<AtomicU64>,
    _watcher: Watcher,
}

impl RepoWatch {
    /// Watches the repository `cwd` is in. `None` when it isn't in one, or it can't be watched
    /// (logged): the views then just don't update.
    pub fn start(editor: &Editor, cwd: &Path) -> Option<Arc<RepoWatch>> {
        Self::start_tapped(editor, cwd, None)
    }

    /// Like [`Self::start`], with a `tap` that sees each batch of changed paths first, on the
    /// watcher's thread, and returns what to do on the main thread once the open buffers of the
    /// changed files were checked (watch mode reads the changed files there).
    pub fn start_tapped(editor: &Editor, cwd: &Path, tap: Option<Tap>) -> Option<Arc<RepoWatch>> {
        let trust_full = editor
            .workspace_trust
            .query(
                &vortex_loader::find_workspace_in(cwd).0,
                vortex_loader::workspace_trust::TrustQuery::Git,
            )
            .is_trusted();
        let state = editor
            .diff_providers
            .repo_state(cwd, trust_full)
            .map_err(|err| log::debug!("no repository to watch: {err}"))
            .ok()?;
        // canonical, like the paths of the documents the changed files are compared with
        let mut config = WatchConfig::new(vortex_stdx::path::canonicalize(state.work_dir));
        config.debounce = DEBOUNCE;
        // `HEAD` and `index` are in the git dir; `logs/HEAD` changes on every commit, even when
        // only the branch ref moves
        let git_dir = vortex_stdx::path::canonicalize(state.git_dir);
        config.extra = [git_dir.clone(), git_dir.join("logs")]
            .into_iter()
            .filter(|dir| dir.is_dir())
            .collect();

        let generation = Arc::new(AtomicU64::new(0));
        let bump = generation.clone();
        // the job queue belongs to the runtime (one per test in integration tests), which the
        // watcher's thread isn't part of
        let runtime = tokio::runtime::Handle::current();
        let watcher = Watcher::spawn(config, move |paths| {
            let bump = bump.clone();
            let then = tap.as_ref().map(|tap| tap(&paths));
            let _runtime = runtime.enter();
            job::dispatch_blocking(move |editor, _| {
                // the same rule as when switching buffers: reload the unmodified ones, warn
                // about the others
                crate::handlers::disk::check_paths(editor, &paths);
                // a merge, rebase or cherry-pick may have started or ended
                crate::handlers::conflicts::refresh(editor);
                if let Some(then) = then {
                    then(editor);
                }
                bump.fetch_add(1, Ordering::Relaxed);
                vortex_event::request_redraw();
            });
        })
        .map_err(|err| log::warn!("can't watch {}: {err}", cwd.display()))
        .ok()?;
        Some(Arc::new(RepoWatch {
            generation,
            _watcher: watcher,
        }))
    }
}

/// A view's handle on a [`RepoWatch`]: tells whether the tree changed since the view last
/// asked. Cloning it gives another view its own handle on the same watch.
#[derive(Clone)]
pub struct Live {
    watch: Arc<RepoWatch>,
    seen: u64,
}

impl Live {
    pub fn new(watch: Arc<RepoWatch>) -> Self {
        let seen = watch.generation.load(Ordering::Relaxed);
        Self { watch, seen }
    }

    /// Whether the tree changed since the last call (or since this handle was made).
    pub fn changed(&mut self) -> bool {
        let generation = self.watch.generation.load(Ordering::Relaxed);
        std::mem::replace(&mut self.seen, generation) != generation
    }
}

/// The result of the latest job run in the background for a view, for it to take when it
/// renders. A job that finishes after a newer one is dropped.
pub struct Pending<T> {
    jobs: Arc<Mutex<Jobs<T>>>,
}

struct Jobs<T> {
    /// The number of the latest job started.
    started: u64,
    /// The number of the latest job whose result was kept.
    done: u64,
    result: Option<T>,
}

impl<T: Send + 'static> Pending<T> {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(Jobs {
                started: 0,
                done: 0,
                result: None,
            })),
        }
    }

    /// Runs `job` on a blocking thread, then redraws so that the view takes the result.
    pub fn run(&self, job: impl FnOnce() -> T + Send + 'static) {
        let jobs = self.jobs.clone();
        let number = {
            let mut jobs = jobs.lock();
            jobs.started += 1;
            jobs.started
        };
        tokio::task::spawn_blocking(move || {
            let result = job();
            let mut jobs = jobs.lock();
            if number > jobs.done {
                jobs.done = number;
                jobs.result = Some(result);
            }
            drop(jobs);
            vortex_event::request_redraw();
        });
    }

    /// The result of the latest job that finished, once.
    pub fn take(&self) -> Option<T> {
        self.jobs.lock().result.take()
    }
}
