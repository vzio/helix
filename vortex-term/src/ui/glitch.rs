//! The glitch (this fork, see FORK.md, "Theme effects"): a reusable effect that makes cells of
//! the screen show a random character for a short time, then go back to what they were. The
//! dashboard's title and background and the ghost text of error diagnostics use it, each with
//! the rate its theme sets (`[effects]`).
//!
//! - **Rate**: a chance per cell per second. Glitches are a Poisson process over the cells on
//!   screen: with `n` cells at rate `r`, the time to the next glitch is drawn from an
//!   exponential distribution of rate `n × r`, and the glitch lands on one of the cells at
//!   random. The process is memoryless, so when the number of cells changes the time is simply
//!   drawn again from the frame.
//! - **Characters**: ASCII only ([`CHARS`]), one column each.
//! - **No constant redraws**: the owner draws a frame, then [`Glitch::schedule`] asks for one
//!   more frame at the next change, a new glitch or one ending, and nothing else. With no cell on
//!   screen (or a rate of 0) nothing is asked for and the editor stays idle.
//!
//! The random source is seeded once per run; nothing is saved.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, Instant};

use tui::buffer::Buffer as Surface;

/// The characters a glitched cell shows: letters, digits and symbols, ASCII only, for every
/// terminal and font.
pub const CHARS: &[u8] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz@#$%&*+=-:;/\\|<>?!";

/// How long a glitch shows, by default.
pub const DURATION: Duration = Duration::from_millis(100);

/// A small random source (splitmix64): fast, good enough for looks, seeded per run.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in (0, 1].
    fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) + 1) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        ((self.next_u64() >> 32) * n as u64 >> 32) as usize
    }
}

/// A seed that differs from run to run, without a dependency: std's random hasher keys.
fn run_seed() -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(0x5eed);
    hasher.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Active {
    /// The cell, as an index into the owner's cells of the frame.
    cell: usize,
    c: char,
    until: Instant,
}

/// A frame asked for at `at`.
#[derive(Debug)]
struct Wake {
    at: Instant,
    task: Option<tokio::task::AbortHandle>,
}

/// The glitches of one set of cells (a title, a background, the ghost text of diagnostics).
#[derive(Debug)]
pub struct Glitch {
    rate: f64,
    duration: Duration,
    rng: Rng,
    /// The time of the next glitch, drawn for `cells` cells.
    next: Option<Instant>,
    cells: usize,
    active: Vec<Active>,
    wake: Option<Wake>,
    /// The glitches started so far, for the tests' statistics.
    started: u64,
}

impl Glitch {
    /// Glitches at `rate`, a chance per cell per second, seeded for this run.
    pub fn new(rate: f64) -> Self {
        Self::seeded(rate, run_seed())
    }

    /// Glitches with a given seed: the same seed and frames give the same glitches.
    pub fn seeded(rate: f64, seed: u64) -> Self {
        Self {
            rate: rate.max(0.0),
            duration: DURATION,
            rng: Rng(seed),
            next: None,
            cells: 0,
            active: Vec::new(),
            wake: None,
            started: 0,
        }
    }

    /// How long a glitch shows.
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The time to the next glitch among `cells` cells.
    fn gap(&mut self, cells: usize) -> Duration {
        let per_second = self.rate * cells as f64;
        Duration::from_secs_f64((-self.rng.unit().ln() / per_second).min(3600.0))
    }

    /// Brings the glitches to `now`, for a frame showing `cells` cells (visible ones only):
    /// glitches that ended are gone, those due since the last frame start.
    pub fn advance(&mut self, now: Instant, cells: usize) {
        if cells != self.cells {
            // memoryless: the time to the next glitch is drawn again for the new count
            self.cells = cells;
            self.next = None;
        }
        self.active
            .retain(|glitch| glitch.until > now && glitch.cell < cells);
        if cells == 0 || self.rate <= 0.0 {
            self.next = None;
            return;
        }
        let mut next = match self.next {
            Some(next) => next,
            None => now + self.gap(cells),
        };
        // after a long pause (no frame drawn), the glitches that would already be over don't
        // matter: start again from the oldest time one could still show
        if let Some(oldest) = now.checked_sub(self.duration) {
            if next < oldest {
                next = oldest + self.gap(cells);
            }
        }
        while next <= now {
            let until = next + self.duration;
            if until > now {
                let cell = self.rng.below(cells);
                let c = CHARS[self.rng.below(CHARS.len())] as char;
                self.active.retain(|glitch| glitch.cell != cell);
                self.active.push(Active { cell, c, until });
                self.started += 1;
            }
            next += self.gap(cells);
        }
        self.next = Some(next);
    }

    /// The glitched cells of the frame: their index among the frame's cells and the character
    /// they show.
    pub fn glitched(&self) -> impl Iterator<Item = (usize, char)> + '_ {
        self.active.iter().map(|glitch| (glitch.cell, glitch.c))
    }

    /// When the glitches change next: a new one, or one ending. `None` when nothing will.
    pub fn next_change(&self) -> Option<Instant> {
        self.active
            .iter()
            .map(|glitch| glitch.until)
            .chain(self.next)
            .min()
    }

    /// Asks the editor for a frame at the next change, unless a frame is already asked for at
    /// or before it. Outside a tokio runtime (unit tests) the wake-up is only remembered.
    pub fn schedule(&mut self, now: Instant) {
        let Some(at) = self.next_change() else {
            self.cancel();
            return;
        };
        if let Some(wake) = &self.wake {
            let pending =
                wake.at > now && wake.task.as_ref().is_none_or(|task| !task.is_finished());
            if pending && wake.at <= at {
                return;
            }
        }
        self.cancel();
        let task = tokio::runtime::Handle::try_current().ok().map(|runtime| {
            runtime
                .spawn(async move {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
                    vortex_event::request_redraw();
                })
                .abort_handle()
        });
        self.wake = Some(Wake { at, task });
    }

    /// The frame asked for, if any.
    pub fn wake_at(&self) -> Option<Instant> {
        self.wake.as_ref().map(|wake| wake.at)
    }

    fn cancel(&mut self) {
        if let Some(Wake {
            task: Some(task), ..
        }) = self.wake.take()
        {
            task.abort();
        }
    }

    /// The glitches started so far.
    pub fn started(&self) -> u64 {
        self.started
    }

    /// One frame over the cells at `cells` of `surface` (visible ones only, in a stable order):
    /// brings the glitches to `now`, draws the glitched cells' characters in their own style,
    /// and asks for a frame at the next change.
    pub fn draw(&mut self, now: Instant, surface: &mut Surface, cells: &[(u16, u16)]) {
        self.advance(now, cells.len());
        for (index, c) in self.glitched() {
            let (x, y) = cells[index];
            if let Some(cell) = surface.get_mut(x, y) {
                cell.set_char(c);
            }
        }
        self.schedule(now);
    }
}

impl Drop for Glitch {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A glitch at the rate a theme sets, made when the rate is set and dropped when it is 0, made
/// again when it changes (another theme): what owners keep across frames.
#[derive(Debug, Default)]
pub struct Slot(Option<Glitch>);

impl Slot {
    /// The glitch at `rate`, or `None` when the rate is 0.
    pub fn at(&mut self, rate: f64) -> Option<&mut Glitch> {
        if rate <= 0.0 {
            self.0 = None;
            return None;
        }
        if self.0.as_ref().is_none_or(|glitch| glitch.rate() != rate) {
            self.0 = Some(Glitch::new(rate));
        }
        self.0.as_mut()
    }

    /// The glitch, if there is one, without changing it.
    pub fn get(&self) -> Option<&Glitch> {
        self.0.as_ref()
    }
}
