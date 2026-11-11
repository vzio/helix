//! The stats of the dashboard, read in the background: each one lands in its own [`Slot`] when
//! it is ready, with the time it arrived so the dashboard can animate it in. Nothing waits for
//! them: the first frame draws at once with every stat loading. Only local git is read, never
//! the network, and nothing is written anywhere.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vortex_vcs::summary::{Changes, Head, InProgress, Remote, Repo, Upstream, Worktree};

use super::languages::{self, Counts};

/// A stat, once read: its value (or why it couldn't be read) and when it arrived.
#[derive(Debug)]
pub struct Slot<T> {
    inner: Mutex<Option<(Result<T, String>, Instant)>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }
}

impl<T: Clone> Slot<T> {
    pub fn set(&self, value: Result<T, String>) {
        self.set_at(value, Instant::now());
    }

    pub fn set_at(&self, value: Result<T, String>, at: Instant) {
        *self.inner.lock().unwrap() = Some((value, at));
        vortex_event::request_redraw();
    }

    /// The value and when it arrived, `None` while it loads.
    pub fn get(&self) -> Option<(Result<T, String>, Instant)> {
        self.inner.lock().unwrap().clone()
    }

    fn arrived(&self) -> Option<Instant> {
        self.inner.lock().unwrap().as_ref().map(|(_, at)| *at)
    }
}

/// Everything the dashboard shows about the folder it opened in.
#[derive(Debug, Default)]
pub struct Stats {
    /// Whether the folder is in a git repository: decided at once, by looking for a `.git` in it
    /// and its parents, so the layout doesn't jump when the repository is read.
    pub git: bool,
    /// The root of the working tree, once the repository is open; when it turns out not to be
    /// one after all (a bare repository, an unreadable `.git`), [`Stats::gone`] is set.
    pub work_dir: Slot<PathBuf>,
    pub gone: AtomicBool,
    pub head: Slot<Head>,
    pub remote: Slot<Option<Remote>>,
    pub upstream: Slot<Option<Upstream>>,
    pub changes: Slot<Changes>,
    pub in_progress: Slot<Option<InProgress>>,
    pub worktrees: Slot<Vec<Worktree>>,
    pub languages: Mutex<Counts>,
    /// When the language counts finished.
    pub languages_done: Slot<()>,
    /// Set when the dashboard closes, so the background work stops asking for frames.
    pub closed: AtomicBool,
}

/// How long a stat takes to appear once it arrived.
pub const REVEAL: Duration = Duration::from_millis(220);

/// How long the logo takes to draw itself.
pub const INTRO: Duration = Duration::from_millis(700);

/// The time between frames while something moves.
const FRAME: Duration = Duration::from_millis(50);

/// Whether `dir` is in a git repository, by looking for `.git` in it and its parents (a few
/// `stat`s: fast enough for the first frame).
pub fn in_git_repo(dir: &Path) -> bool {
    dir.ancestors().any(|dir| dir.join(".git").exists())
}

impl Stats {
    /// Whether the git stats are shown.
    pub fn shows_git(&self) -> bool {
        self.git && !self.gone.load(Ordering::Relaxed)
    }

    /// Whether everything is loaded and drawn in, the logo too: nothing moves anymore, so no more
    /// frames are needed.
    pub fn settled(&self, born: Instant, now: Instant) -> bool {
        if now < born + INTRO {
            return false;
        }
        if !self.shows_git() {
            return true;
        }
        let arrivals = [
            self.head.arrived(),
            self.remote.arrived(),
            self.upstream.arrived(),
            self.changes.arrived(),
            self.in_progress.arrived(),
            self.worktrees.arrived(),
            self.languages_done.arrived(),
        ];
        arrivals
            .iter()
            .all(|arrived| arrived.is_some_and(|at| now >= at + REVEAL))
    }
}

fn read<T>(result: anyhow::Result<T>) -> Result<T, String> {
    result.map_err(|err| {
        log::debug!("dashboard: {err:#}");
        "couldn't read".to_string()
    })
}

/// Starts reading the stats of `dir` in the background, and asks for frames while something
/// moves. Outside a tokio runtime (unit tests) nothing is read.
pub fn load(
    dir: PathBuf,
    trust_full: bool,
    loader: Arc<vortex_core::syntax::Loader>,
    lockfiles: vortex_view::editor::ChangesConfig,
) -> Arc<Stats> {
    let stats = Arc::new(Stats {
        git: in_git_repo(&dir),
        ..Stats::default()
    });
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return stats;
    };
    let born = Instant::now();

    // frames for the animations, until everything settled or the dashboard closed
    let ticking = stats.clone();
    handle.spawn(async move {
        loop {
            tokio::time::sleep(FRAME).await;
            if ticking.closed.load(Ordering::Relaxed) {
                break;
            }
            vortex_event::request_redraw();
            if ticking.settled(born, Instant::now()) {
                break;
            }
        }
    });

    if !stats.git {
        return stats;
    }
    let opening = stats.clone();
    let spawner = handle.clone();
    handle.spawn_blocking(move || {
        let stats = opening;
        let repo = match Repo::open(&dir, trust_full) {
            Ok(Some(repo)) => repo,
            result => {
                if let Err(err) = result {
                    log::debug!("dashboard: {err:#}");
                }
                stats.gone.store(true, Ordering::Relaxed);
                vortex_event::request_redraw();
                return;
            }
        };
        stats.work_dir.set(Ok(repo.work_dir().to_path_buf()));

        // each stat on its own, so a slow one (the status of a large tree) doesn't hold the others
        macro_rules! stat {
            ($slot:ident, $method:ident) => {{
                let stats = stats.clone();
                let repo = repo.clone();
                spawner.spawn_blocking(move || {
                    if !stats.closed.load(Ordering::Relaxed) {
                        stats.$slot.set(read(repo.$method()));
                    }
                });
            }};
        }
        stat!(head, head);
        stat!(remote, remote);
        stat!(upstream, upstream);
        stat!(in_progress, in_progress);
        stat!(worktrees, worktrees);
        stat!(changes, changes);

        let files = match repo.tracked_files() {
            Ok(files) => files,
            Err(err) => {
                log::debug!("dashboard: {err:#}");
                let mut counts = stats.languages.lock().unwrap();
                counts.files = Some(0);
                counts.finished = true;
                drop(counts);
                stats.languages_done.set(Err("couldn't read".into()));
                return;
            }
        };
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        languages::count(
            files,
            &loader,
            |path| lockfiles.is_lockfile(path),
            threads,
            &stats.languages,
            || stats.closed.load(Ordering::Relaxed),
        );
        stats.languages_done.set(Ok(()));
    });
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use vortex_vcs::summary::Changes;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Fills the slot named by `which` (0 to 6) as if it arrived at `at`.
    fn arrive(stats: &Stats, which: usize, at: Instant) {
        match which {
            0 => stats.head.set_at(
                Ok(Head {
                    branch: Some("main".into()),
                    commit: None,
                }),
                at,
            ),
            1 => stats.remote.set_at(Ok(None), at),
            2 => stats.upstream.set_at(Ok(None), at),
            3 => stats.changes.set_at(Ok(Changes::default()), at),
            4 => stats.in_progress.set_at(Ok(None), at),
            5 => stats.worktrees.set_at(Ok(vec![]), at),
            6 => stats.languages_done.set_at(Ok(()), at),
            _ => unreachable!(),
        }
    }

    #[test]
    fn nothing_is_settled_before_the_logo_is_drawn() {
        let born = Instant::now();
        let stats = Stats::default();
        assert!(!stats.settled(born, born));
        assert!(!stats.settled(born, born + INTRO - ms(1)));
        // outside of a repository there is nothing else to wait for
        assert!(stats.settled(born, born + INTRO));
        assert!(stats.settled(born, born + INTRO + Duration::from_secs(60)));
    }

    #[test]
    fn a_repository_settles_when_every_stat_is_in_and_revealed() {
        let born = Instant::now();
        let stats = Stats {
            git: true,
            ..Stats::default()
        };
        assert!(stats.shows_git());
        let late = born + INTRO + Duration::from_secs(60);
        // not before the logo, and not while nothing has arrived
        assert!(!stats.settled(born, born));
        assert!(!stats.settled(born, late));

        // all but the last, each long ago: still waiting for it
        for which in 0..6 {
            arrive(&stats, which, born + ms(100));
            assert!(!stats.settled(born, late), "slot {which}");
        }
        arrive(&stats, 6, born + ms(800));
        // it arrived, but isn't drawn in until REVEAL after
        let revealed = born + ms(800) + REVEAL;
        assert!(revealed > born + INTRO);
        assert!(!stats.settled(born, born + INTRO));
        assert!(!stats.settled(born, revealed - ms(1)));
        assert!(stats.settled(born, revealed));
        assert!(stats.settled(born, late));
    }

    #[test]
    fn each_stat_is_needed() {
        let born = Instant::now();
        let late = born + INTRO + Duration::from_secs(60);
        for missing in 0..7 {
            let stats = Stats {
                git: true,
                ..Stats::default()
            };
            for which in (0..7).filter(|&which| which != missing) {
                arrive(&stats, which, born);
            }
            assert!(!stats.settled(born, late), "settled without slot {missing}");
            arrive(&stats, missing, born);
            assert!(stats.settled(born, late));
        }
    }

    #[test]
    fn a_folder_that_turns_out_not_to_be_a_repository_settles_without_stats() {
        let born = Instant::now();
        let stats = Stats {
            git: true,
            ..Stats::default()
        };
        assert!(!stats.settled(born, born + INTRO));
        stats.gone.store(true, Ordering::Relaxed);
        assert!(!stats.shows_git());
        assert!(stats.settled(born, born + INTRO));
    }

    #[test]
    fn a_folder_without_git_has_no_git_stats() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!in_git_repo(dir.path()));
        let stats = Stats {
            git: in_git_repo(dir.path()),
            ..Stats::default()
        };
        assert!(!stats.shows_git());
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(in_git_repo(dir.path()));
        // a folder inside of it counts too
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(in_git_repo(&dir.path().join("sub")));
    }
}
