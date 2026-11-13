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
//! - **The vortex** behind it moves by [`DashboardBackground`]: it spins always (`on`), at the
//!   same speed while loading and then slowing down to a stop over [`SPIN_DOWN`] (`settle`), or
//!   never (`static`). Its phase is the integral of its speed, so slowing down never jumps.
//! - **Its reach**: `grow` and `grow-on` start with no vortex and grow it from the eye to the
//!   corners over [`REACH`]; `drain` starts full and empties from the corners into the eye. The
//!   reach is a radius ([`Timeline::reach`]) beyond which the vortex isn't drawn: a limit over
//!   the same flow, not another one. The motion starts at [`Timeline::motion_start`], when the
//!   title starts to open or after [`MOTION_DELAY`], whichever is first, so slow stats don't
//!   hold it back.

use std::time::{Duration, Instant};

use vortex_view::editor::DashboardBackground;

use super::stats::Stats;

/// The title never opens sooner than this after the dashboard did.
pub const MIN_LOADING: Duration = Duration::from_millis(1000);

/// How long a value takes to type itself in.
pub const TYPE: Duration = Duration::from_millis(260);

/// How long the title takes to open from `VX` to `VORTEX`.
pub const TITLE: Duration = Duration::from_millis(720);

/// How long the vortex takes to stop in the `settle` mode, once the title is open.
pub const SPIN_DOWN: Duration = Duration::from_millis(3200);

/// How long the vortex takes to grow from its eye to the corners (`grow`, `grow-on`), or to drain
/// from the corners into its eye (`drain`): the one constant for the speed of both, the same
/// time in any window.
pub const REACH: Duration = Duration::from_millis(1600);

/// The latest the growing or draining starts after the dashboard opened, however slow the
/// stats are.
pub const MOTION_DELAY: Duration = Duration::from_millis(1000);

/// The width of the soft edge of a growing or draining vortex, as a share of the distance from
/// the eye to the corners.
pub const EDGE: f64 = 0.08;

/// The time between two requests for a frame while something moves. The editor draws a frame
/// 33 ms after the first request that finds none pending, so asking twice as often keeps one
/// pending most of the time: about 20 frames a second.
pub const FRAME: Duration = Duration::from_millis(16);

/// The speed of the vortex in the `on` mode, and while loading in the `settle` mode: seconds of
/// spin per second. Slower than the time, so the flow is calm to look at.
const CRUISE: f64 = 2.0 / 3.0;

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

    /// When the vortex starts to grow or drain: when the title starts to open, or
    /// [`MOTION_DELAY`] after the dashboard opened, whichever is first. (The title never opens
    /// sooner than [`MIN_LOADING`], so with fast stats both are the same instant.)
    pub fn motion_start(&self, stats: &Stats) -> Instant {
        let latest = self.born + MOTION_DELAY;
        match self.ready(stats) {
            Some(ready) => ready.min(latest),
            None => latest,
        }
    }

    /// How far the vortex reaches at `now`, as a share of the distance from its eye to the
    /// corners: cells farther out aren't drawn (and the last [`EDGE`] of it fades). Infinite
    /// in the modes that don't grow or drain; 0 for an empty vortex.
    pub fn reach(&self, stats: &Stats, now: Instant) -> f64 {
        let full = 1.0 + EDGE;
        let x = || progress(self.motion_start(stats), REACH, now);
        match self.background {
            // out from the eye, fast first and gently arriving at the corners
            DashboardBackground::Grow | DashboardBackground::GrowOn => {
                full * (1.0 - (1.0 - x()).powi(2))
            }
            // the rim empties first and the edge falls in faster and faster, like the flow
            DashboardBackground::Drain => full * (1.0 - x() * x()),
            _ => f64::INFINITY,
        }
    }

    /// The phase of the vortex at `now`, in seconds of spin at cruise speed; `None` without
    /// a vortex.
    pub fn spin(&self, stats: &Stats, now: Instant) -> Option<f64> {
        let cruise = CRUISE * seconds(self.born, now);
        match self.background {
            DashboardBackground::Off => None,
            DashboardBackground::Static => Some(STILL),
            DashboardBackground::On | DashboardBackground::GrowOn | DashboardBackground::Drain => {
                Some(cruise)
            }
            DashboardBackground::Settle => {
                Some(self.spin_down(self.ready(stats).map(|ready| ready + TITLE), SPIN_DOWN, now))
            }
            // slows down as it grows, still once it reaches the corners
            DashboardBackground::Grow => {
                Some(self.spin_down(Some(self.motion_start(stats)), REACH, now))
            }
        }
    }

    /// The phase of a vortex at cruise speed until `from`, then slowing down to a stop over
    /// `length`: the speed falls as (1 - x)², x going from 0 to 1, whose integral is
    /// (1 - (1 - x)³) / 3.
    fn spin_down(&self, from: Option<Instant>, length: Duration, now: Instant) -> f64 {
        match from {
            Some(from) if now > from => {
                let x = progress(from, length, now);
                CRUISE * seconds(self.born, from)
                    + CRUISE * length.as_secs_f64() * (1.0 - (1.0 - x).powi(3)) / 3.0
            }
            _ => CRUISE * seconds(self.born, now),
        }
    }

    /// Whether nothing moves anymore at `now`, so no more frames are needed: everything in and
    /// typed, the title open, and the vortex still (or empty, or full and still). In the `on`
    /// and `grow-on` modes the vortex never stops.
    pub fn settled(&self, stats: &Stats, now: Instant) -> bool {
        let Some(ready) = self.ready(stats) else {
            return false;
        };
        // every value is typed in by ready + TYPE, and TYPE is shorter than TITLE
        let open = ready + TITLE;
        match self.background {
            DashboardBackground::On | DashboardBackground::GrowOn => false,
            DashboardBackground::Settle => now >= open + SPIN_DOWN,
            DashboardBackground::Grow | DashboardBackground::Drain => {
                now >= open.max(self.motion_start(stats) + REACH)
            }
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
    fn the_settle_mode_slows_down_without_jumping() {
        let born = Instant::now();
        let timeline = Timeline::new(born, DashboardBackground::Settle);
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
                DashboardBackground::Settle,
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

        let load = at(DashboardBackground::Settle);
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
        assert!(spin(DashboardBackground::Settle, born + ms(500)).unwrap() > 0.0);
        // while loading, `load` spins exactly like `on`
        for &t in times.iter().filter(|&&t| t <= open) {
            assert_eq!(
                spin(DashboardBackground::Settle, t),
                spin(DashboardBackground::On, t)
            );
        }
        let stopped = spin(DashboardBackground::Settle, open + SPIN_DOWN).unwrap();
        for extra in [1, 1000, 3_600_000] {
            assert_eq!(
                spin(DashboardBackground::Settle, open + SPIN_DOWN + ms(extra)),
                Some(stopped)
            );
        }
    }

    /// The instants from `from` to `to`, every `step`.
    fn instants(from: Instant, to: Instant, step: Duration) -> Vec<Instant> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            out.push(t);
            t += step;
        }
        out
    }

    #[test]
    fn drain_starts_full_empties_monotonically_and_settles_when_empty() {
        let born = Instant::now();
        let stats = Stats::default();
        let timeline = Timeline::new(born, DashboardBackground::Drain);
        let start = timeline.motion_start(&stats);
        let end = start + REACH;
        // full at born and until the motion starts
        for t in instants(born, start, ms(50)) {
            assert!(timeline.reach(&stats, t) >= 1.0 + EDGE, "{:?}", t - born);
        }
        // falls to 0
        let mut previous = f64::MAX;
        for t in instants(start, end, ms(16)) {
            let reach = timeline.reach(&stats, t);
            assert!(reach <= previous, "{reach} > {previous}");
            previous = reach;
        }
        assert_eq!(timeline.reach(&stats, end), 0.0);
        assert_eq!(timeline.reach(&stats, end + ms(3_600_000)), 0.0);
        // settled only once empty (and the title open)
        for t in instants(born, end - ms(1), ms(50)) {
            assert!(!timeline.settled(&stats, t), "{:?}", t - born);
        }
        assert!(timeline.settled(&stats, end));
        assert!(timeline.title(&stats, end) == 1.0);
        assert!(timeline.settled(&stats, end + ms(3_600_000)));
    }

    #[test]
    fn grow_is_empty_until_the_motion_starts_then_fills_and_stops_spinning() {
        let born = Instant::now();
        let stats = Stats::default();
        let timeline = Timeline::new(born, DashboardBackground::Grow);
        let start = timeline.motion_start(&stats);
        let end = start + REACH;
        for t in instants(born, start, ms(50)) {
            assert_eq!(timeline.reach(&stats, t), 0.0, "{:?}", t - born);
        }
        let mut previous = 0.0;
        for t in instants(start, end, ms(16)) {
            let reach = timeline.reach(&stats, t);
            assert!(reach >= previous, "{reach} < {previous}");
            previous = reach;
        }
        assert!(timeline.reach(&stats, end) >= 1.0 + EDGE);
        assert!(timeline.reach(&stats, end + ms(3_600_000)) >= 1.0 + EDGE);
        // the spin slows down to a stop at the end
        assert_eq!(
            timeline.spin(&stats, end),
            timeline.spin(&stats, end + Duration::from_secs(3600))
        );
        assert_ne!(
            timeline.spin(&stats, start + REACH / 2),
            timeline.spin(&stats, end)
        );
        assert!(!timeline.settled(&stats, end - ms(1)));
        assert!(timeline.settled(&stats, end));
    }

    #[test]
    fn grow_on_grows_like_grow_spins_like_on_and_never_settles() {
        let born = Instant::now();
        let stats = Stats::default();
        let grow = Timeline::new(born, DashboardBackground::Grow);
        let grow_on = Timeline::new(born, DashboardBackground::GrowOn);
        let on = Timeline::new(born, DashboardBackground::On);
        let hour = born + Duration::from_secs(3600);
        for t in instants(born, born + ms(5000), ms(37))
            .into_iter()
            .chain([hour])
        {
            assert_eq!(grow.reach(&stats, t), grow_on.reach(&stats, t));
            assert_eq!(grow_on.spin(&stats, t), on.spin(&stats, t));
            assert!(!grow_on.settled(&stats, t));
        }
    }

    #[test]
    fn growing_and_draining_start_at_the_delay_whatever_the_stats() {
        let born = Instant::now();
        let at = born + MOTION_DELAY;
        for background in [DashboardBackground::Grow, DashboardBackground::Drain] {
            let timeline = Timeline::new(born, background);
            // fast stats: the same instant as ready
            let fast = stats_in_git(born + ms(10), 6, born + ms(10), &[]);
            assert_eq!(timeline.motion_start(&fast), at);
            assert_eq!(timeline.ready(&fast), Some(at));
            // one stat missing: growing doesn't wait for it
            let missing = stats_in_git(born + ms(10), 0, born + ms(10), &[3]);
            assert_eq!(timeline.ready(&missing), None);
            assert_eq!(timeline.motion_start(&missing), at);
            // the last stat at 3 s
            let late = stats_in_git(born + ms(10), 6, born + Duration::from_secs(3), &[]);
            assert_eq!(timeline.motion_start(&late), at);
        }
        let grow = Timeline::new(born, DashboardBackground::Grow);
        let missing = stats_in_git(born + ms(10), 0, born + ms(10), &[3]);
        assert!(grow.reach(&missing, at + REACH / 2) > 0.0);
        assert_eq!(grow.reach(&missing, at), 0.0);
    }

    #[test]
    fn the_other_modes_have_an_infinite_reach() {
        let born = Instant::now();
        let stats = Stats::default();
        for background in [
            DashboardBackground::On,
            DashboardBackground::Settle,
            DashboardBackground::Static,
            DashboardBackground::Off,
        ] {
            let timeline = Timeline::new(born, background);
            for t in [0, 500, 1500, 3000, 3_600_000] {
                assert_eq!(timeline.reach(&stats, born + ms(t)), f64::INFINITY);
            }
        }
    }

    #[test]
    fn the_background_modes_parse_and_load_is_settle() {
        #[derive(serde::Deserialize)]
        struct W {
            b: DashboardBackground,
        }
        let parse = |name: &str| toml::from_str::<W>(&format!("b = \"{name}\"")).unwrap().b;
        assert_eq!(parse("load"), DashboardBackground::Settle);
        assert_eq!(parse("settle"), DashboardBackground::Settle);
        assert_eq!(parse("drain"), DashboardBackground::Drain);
        assert_eq!(parse("grow"), DashboardBackground::Grow);
        assert_eq!(parse("grow-on"), DashboardBackground::GrowOn);
        assert_eq!(parse("on"), DashboardBackground::On);
        assert_eq!(parse("static"), DashboardBackground::Static);
        assert_eq!(parse("off"), DashboardBackground::Off);
        assert!(toml::from_str::<W>("b = \"spin\"").is_err());
        assert_eq!(DashboardBackground::default(), DashboardBackground::GrowOn);
    }
}
