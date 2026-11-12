//! The dashboard's clock: when each thing moves, as pure functions of the time it opened, the
//! times the stats arrived and the time of the frame. Nothing here waits: keys work from the
//! first frame, and the animations only decide what a frame looks like.
//!
//! - **Loading.** The title reads `VX`. Rows shimmer until their stat is in, then type
//!   themselves in, in a cascade from the top that lasts at least [`MIN_LOADING`]: stats that
//!   arrive at once still take about a second to appear, stats that arrive late appear when
//!   they do.
//! - **Ready**, once every stat is in and [`MIN_LOADING`] has passed: the title opens from `VX`
//!   to `VORTEX` over [`TITLE`].
//! - **The vortex** behind it spins by [`DashboardBackground`]: always (`on`), fast while
//!   loading and then slowing down to a stop over [`SPIN_DOWN`] (`load`), or never (`static`).
//!   Its phase is the integral of its speed, so slowing down never jumps.

use std::time::{Duration, Instant};

use vortex_view::editor::DashboardBackground;

use super::stats::Stats;

/// The title never opens sooner than this after the dashboard did.
pub const MIN_LOADING: Duration = Duration::from_millis(1000);

/// How long a value takes to type itself in.
pub const TYPE: Duration = Duration::from_millis(260);

/// How long the title takes to open from `VX` to `VORTEX`.
pub const TITLE: Duration = Duration::from_millis(720);

/// How long the vortex takes to stop in the `load` mode, once the title is open.
pub const SPIN_DOWN: Duration = Duration::from_millis(3200);

/// The time between two requests for a frame while something moves. The editor draws a frame
/// 33 ms after the first request that finds none pending, so asking twice as often keeps one
/// pending most of the time: about 20 frames a second.
pub const FRAME: Duration = Duration::from_millis(16);

/// The speed of the vortex in the `on` mode, and while loading in the `load` mode: seconds of
/// spin per second.
const CRUISE: f64 = 1.0;
const FAST: f64 = 2.6;

/// The phase of the one frame the `static` mode draws, picked for its looks.
const STILL: f64 = 41.7;

/// When the dashboard opened and how its background moves.
#[derive(Debug, Clone, Copy)]
pub struct Timeline {
    pub born: Instant,
    pub background: DashboardBackground,
}

fn seconds(from: Instant, to: Instant) -> f64 {
    to.saturating_duration_since(from).as_secs_f64()
}

/// How far an animation that started at `start` and lasts `length` is at `now`, from 0 to 1.
pub fn progress(start: Instant, length: Duration, now: Instant) -> f64 {
    (seconds(start, now) / length.as_secs_f64()).clamp(0.0, 1.0)
}

impl Timeline {
    pub fn new(born: Instant, background: DashboardBackground) -> Self {
        Self { born, background }
    }

    /// When the value at `place` in the cascade (0 the first row, 1 the last) starts typing
    /// in, at the earliest: the cascade spreads the rows over [`MIN_LOADING`].
    fn cascade(&self, place: f64) -> Instant {
        self.born + (MIN_LOADING - TYPE).mul_f64(place.clamp(0.0, 1.0))
    }

    /// How much of a value that arrived at `at`, at `place` in the cascade, is typed in at
    /// `now`, from 0 to 1.
    pub fn typed(&self, at: Instant, place: f64, now: Instant) -> f64 {
        progress(at.max(self.cascade(place)), TYPE, now)
    }

    /// The share of a progress bar shown at `now` when `real` of it is done: never ahead of a
    /// sweep over [`MIN_LOADING`], so a fast count still fills it in rather than at once.
    pub fn filled(&self, real: f64, now: Instant) -> f64 {
        let sweep = progress(self.born, MIN_LOADING, now);
        let eased = 1.0 - (1.0 - sweep).powi(2);
        real.min(eased)
    }

    /// When everything is in and the title may open: the last stat's arrival, and never
    /// sooner than [`MIN_LOADING`] after the dashboard opened. `None` while loading.
    pub fn ready(&self, stats: &Stats) -> Option<Instant> {
        let earliest = self.born + MIN_LOADING;
        if !stats.shows_git() {
            return Some(earliest);
        }
        stats.all_in().map(|last| last.max(earliest))
    }

    /// How far the title is from `VX` (0) to `VORTEX` (1) at `now`.
    pub fn title(&self, stats: &Stats, now: Instant) -> f64 {
        match self.ready(stats) {
            Some(ready) => progress(ready, TITLE, now),
            None => 0.0,
        }
    }

    /// The phase of the vortex at `now`, in seconds of spin at cruise speed; `None` without
    /// a vortex.
    pub fn spin(&self, stats: &Stats, now: Instant) -> Option<f64> {
        match self.background {
            DashboardBackground::Off => None,
            DashboardBackground::Static => Some(STILL),
            DashboardBackground::On => Some(CRUISE * seconds(self.born, now)),
            DashboardBackground::Load => {
                // fast until the title is open, then the speed falls as (1 - x)², x going
                // from 0 to 1 over SPIN_DOWN: its integral is (1 - (1 - x)³) / 3
                let open = self.ready(stats).map(|ready| ready + TITLE);
                match open {
                    Some(open) if now > open => {
                        let length = SPIN_DOWN.as_secs_f64();
                        let x = progress(open, SPIN_DOWN, now);
                        Some(
                            FAST * seconds(self.born, open)
                                + FAST * length * (1.0 - (1.0 - x).powi(3)) / 3.0,
                        )
                    }
                    _ => Some(FAST * seconds(self.born, now)),
                }
            }
        }
    }

    /// Whether nothing moves anymore at `now`, so no more frames are needed: everything in and
    /// typed, the title open, and the vortex still. In the `on` mode the vortex never stops.
    pub fn settled(&self, stats: &Stats, now: Instant) -> bool {
        let Some(ready) = self.ready(stats) else {
            return false;
        };
        // every value is typed in by ready + TYPE, and TYPE is shorter than TITLE
        let open = ready + TITLE;
        match self.background {
            DashboardBackground::On => false,
            DashboardBackground::Load => now >= open + SPIN_DOWN,
            DashboardBackground::Static | DashboardBackground::Off => now >= open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_type_in_no_sooner_than_their_place_in_the_cascade() {
        let born = Instant::now();
        let timeline = Timeline::new(born, DashboardBackground::Off);
        // arrived at once: the first row types in right away, the last one at the end
        assert!(timeline.typed(born, 0.0, born + TYPE) == 1.0);
        assert!(timeline.typed(born, 1.0, born + TYPE) == 0.0);
        assert!(timeline.typed(born, 1.0, born + MIN_LOADING) == 1.0);
        // arrived late: when it arrives
        let late = born + Duration::from_secs(3);
        assert!(timeline.typed(late, 0.0, late) == 0.0);
        assert!(timeline.typed(late, 0.0, late + TYPE) == 1.0);
    }

    #[test]
    fn the_load_mode_slows_down_without_jumping() {
        let born = Instant::now();
        let timeline = Timeline::new(born, DashboardBackground::Load);
        let stats = Stats::default(); // outside git: ready at MIN_LOADING
        let open = born + MIN_LOADING + TITLE;
        let mut previous = 0.0;
        let mut previous_step = f64::MAX;
        let mut t = open - Duration::from_millis(500);
        while t < open + SPIN_DOWN + Duration::from_secs(1) {
            let spin = timeline.spin(&stats, t).unwrap();
            let step = spin - previous;
            assert!(step >= 0.0);
            if t > open {
                // slowing down
                assert!(step <= previous_step + 1e-9, "{step} > {previous_step}");
            }
            previous = spin;
            previous_step = step;
            t += FRAME;
        }
        // stopped
        let stopped = timeline.spin(&stats, open + SPIN_DOWN).unwrap();
        assert_eq!(
            stopped,
            timeline.spin(&stats, open + SPIN_DOWN * 3).unwrap()
        );
    }

    use vortex_vcs::summary::{Changes, Head};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Stats of a repository where every stat but those in `missing` (0 to 6) arrived at `at`,
    /// and the last one, `last`, at `last_at`.
    fn stats_in_git(at: Instant, last: usize, last_at: Instant, missing: &[usize]) -> Stats {
        let stats = Stats {
            git: true,
            ..Stats::default()
        };
        for which in (0..7).filter(|which| !missing.contains(which)) {
            let at = if which == last { last_at } else { at };
            match which {
                0 => stats.head.set_at(
                    Ok(Head {
                        branch: None,
                        commit: None,
                    }),
                    at,
                ),
                1 => stats.remote.set_at(Ok(None), at),
                2 => stats.upstream.set_at(Ok(None), at),
                3 => stats.changes.set_at(Ok(Changes::default()), at),
                4 => stats.in_progress.set_at(Ok(None), at),
                5 => stats.worktrees.set_at(Ok(vec![]), at),
                _ => stats.languages_done.set_at(Ok(()), at),
            }
        }
        stats
    }

    #[test]
    fn fast_stats_still_wait_for_the_minimum() {
        let born = Instant::now();
        let stats = stats_in_git(born + ms(10), 6, born + ms(10), &[]);
        let timeline = Timeline::new(born, DashboardBackground::Off);
        assert_eq!(timeline.ready(&stats), Some(born + MIN_LOADING));
        for t in [0, 10, 500, 999, 1000] {
            assert_eq!(timeline.title(&stats, born + ms(t)), 0.0, "at {t}ms");
        }
        let half = timeline.title(&stats, born + MIN_LOADING + TITLE / 2);
        assert!(half > 0.0 && half < 1.0, "{half}");
        assert_eq!(timeline.title(&stats, born + MIN_LOADING + TITLE), 1.0);
        assert_eq!(timeline.title(&stats, born + ms(60_000)), 1.0);
    }

    #[test]
    fn slow_stats_keep_vx_until_they_arrive() {
        let born = Instant::now();
        let last = born + Duration::from_secs(3);
        let stats = stats_in_git(born + ms(10), 3, last, &[]);
        let timeline = Timeline::new(born, DashboardBackground::Off);
        assert_eq!(timeline.ready(&stats), Some(last));
        for t in [10, 1000, 1500, 2999, 3000] {
            assert_eq!(timeline.title(&stats, born + ms(t)), 0.0, "at {t}ms");
        }
        assert!(timeline.title(&stats, last + TITLE / 2) > 0.0);
        assert_eq!(timeline.title(&stats, last + TITLE), 1.0);
    }

    #[test]
    fn one_missing_stat_keeps_vx_forever() {
        let born = Instant::now();
        for missing in 0..7 {
            let stats = stats_in_git(born + ms(10), 0, born + ms(10), &[missing]);
            for background in [
                DashboardBackground::On,
                DashboardBackground::Load,
                DashboardBackground::Static,
                DashboardBackground::Off,
            ] {
                let timeline = Timeline::new(born, background);
                assert_eq!(timeline.ready(&stats), None, "slot {missing}");
                let hour = born + Duration::from_secs(3600);
                assert_eq!(timeline.title(&stats, hour), 0.0, "slot {missing}");
                assert!(!timeline.settled(&stats, hour), "{background:?} {missing}");
            }
        }
    }

    #[test]
    fn a_folder_outside_git_is_ready_at_the_minimum() {
        let born = Instant::now();
        let timeline = Timeline::new(born, DashboardBackground::Off);
        let stats = Stats::default();
        assert_eq!(timeline.ready(&stats), Some(born + MIN_LOADING));
        assert_eq!(timeline.title(&stats, born + MIN_LOADING - ms(1)), 0.0);
        assert_eq!(timeline.title(&stats, born + MIN_LOADING + TITLE), 1.0);
    }

    #[test]
    fn each_mode_settles_at_its_own_instant() {
        let born = Instant::now();
        let stats = Stats::default();
        let open = born + MIN_LOADING + TITLE;
        let hour = born + Duration::from_secs(3600);
        let at = |background| Timeline::new(born, background);

        let on = at(DashboardBackground::On);
        assert!(!on.settled(&stats, open + SPIN_DOWN));
        assert!(!on.settled(&stats, hour));

        let load = at(DashboardBackground::Load);
        assert!(!load.settled(&stats, born));
        assert!(!load.settled(&stats, open));
        assert!(!load.settled(&stats, open + SPIN_DOWN - ms(1)));
        assert!(load.settled(&stats, open + SPIN_DOWN));
        assert!(load.settled(&stats, hour));

        for background in [DashboardBackground::Static, DashboardBackground::Off] {
            let timeline = at(background);
            assert!(!timeline.settled(&stats, born), "{background:?}");
            assert!(!timeline.settled(&stats, open - ms(1)), "{background:?}");
            assert!(timeline.settled(&stats, open), "{background:?}");
            assert!(timeline.settled(&stats, hour), "{background:?}");
        }
    }

    #[test]
    fn late_stats_delay_settling() {
        let born = Instant::now();
        let last = born + Duration::from_secs(3);
        let stats = stats_in_git(born + ms(10), 6, last, &[]);
        let timeline = Timeline::new(born, DashboardBackground::Static);
        assert!(!timeline.settled(&stats, born + MIN_LOADING + TITLE));
        assert!(!timeline.settled(&stats, last + TITLE - ms(1)));
        assert!(timeline.settled(&stats, last + TITLE));
    }

    #[test]
    fn the_vortex_spins_by_mode() {
        let born = Instant::now();
        let stats = Stats::default();
        let times: Vec<Instant> = (0..80).map(|i| born + ms(i * 150)).collect();
        let spin = |background, now| Timeline::new(born, background).spin(&stats, now);

        assert!(times
            .iter()
            .all(|&t| spin(DashboardBackground::Off, t).is_none()));

        let still = spin(DashboardBackground::Static, born).unwrap();
        assert!(times
            .iter()
            .all(|&t| spin(DashboardBackground::Static, t) == Some(still)));

        let mut previous = f64::MIN;
        for &t in &times {
            let now = spin(DashboardBackground::On, t).unwrap();
            assert!(now > previous || t == born, "{now} <= {previous}");
            previous = now;
        }
        let far = born + Duration::from_secs(3600);
        assert!(spin(DashboardBackground::On, far).unwrap() > previous);

        let open = born + MIN_LOADING + TITLE;
        assert!(spin(DashboardBackground::Load, born + ms(500)).unwrap() > 0.0);
        let stopped = spin(DashboardBackground::Load, open + SPIN_DOWN).unwrap();
        for extra in [1, 1000, 3_600_000] {
            assert_eq!(
                spin(DashboardBackground::Load, open + SPIN_DOWN + ms(extra)),
                Some(stopped)
            );
        }
    }
}
