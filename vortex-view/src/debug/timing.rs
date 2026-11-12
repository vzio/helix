//! Time per line: how long each playback step took, minus the cost of stepping itself, kept per
//! line of the session so the code reads as a rough flame graph.
//!
//! Each request of a step is timed from when it is sent to when the event of its stop arrives
//! (not what the editor loads after it). That includes the round trip to the adapter and the
//! debugger's own work, which is about the same for every step of a kind: the overhead of a step
//! in is the median of the faster half of the last plain steps (one request, the same function
//! before and after), the overhead of a step out the same over the last step outs (most leave a
//! cheap library call). A step's time is the time of its requests minus their overheads. Steps
//! under the threshold, or under the step's own overhead (the overhead varies about as much as it
//! is), count as free, so the noise of the correction doesn't pile up on cheap lines.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Plain steps kept for the overhead.
const SAMPLES: usize = 32;
/// Plain steps needed before anything is labelled.
const MIN_SAMPLES: usize = 5;

/// The time spent on one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineTime {
    /// The corrected time of the steps that left it above the threshold.
    pub total: Duration,
    /// Their CPU time, when it could be read.
    pub cpu: Option<Duration>,
    /// Steps counted.
    pub hits: u32,
}

#[derive(Debug, Clone)]
pub struct Timing {
    pub lines: HashMap<PathBuf, BTreeMap<usize, LineTime>>,
    /// The time of recent plain steps.
    samples: VecDeque<Duration>,
    /// The time of recent step outs.
    out_samples: VecDeque<Duration>,
    /// Steps faster than this (after correction) aren't labelled.
    pub threshold: Duration,
    pub enabled: bool,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            lines: HashMap::new(),
            samples: VecDeque::new(),
            out_samples: VecDeque::new(),
            threshold: Duration::from_millis(1),
            enabled: true,
        }
    }
}

impl Timing {
    /// The measured cost of a step in; `None` until enough plain steps were seen.
    pub fn overhead(&self) -> Option<Duration> {
        faster_median(&self.samples)
    }

    /// The measured cost of a step out; the step in's until enough step outs were seen.
    pub fn out_overhead(&self) -> Option<Duration> {
        faster_median(&self.out_samples).or_else(|| self.overhead())
    }

    /// A playback step ended: `step` the time of its step in, `outs` the times of the step outs
    /// that brought it back to the code playback follows; `plain` when it stayed in the same
    /// function with one request. Credits the line it left with its corrected time when that is
    /// above the threshold, and returns that time.
    pub fn record(
        &mut self,
        from: Option<(PathBuf, usize)>,
        step: Duration,
        outs: &[Duration],
        plain: bool,
        cpu: Option<Duration>,
    ) -> Option<Duration> {
        if plain && outs.is_empty() {
            push(&mut self.samples, step);
        }
        for out in outs {
            push(&mut self.out_samples, *out);
        }
        let overhead = self.overhead()?;
        let out_overhead = self.out_overhead()?;
        let wall = step + outs.iter().sum::<Duration>();
        let corrected = step.saturating_sub(overhead)
            + outs
                .iter()
                .map(|out| out.saturating_sub(out_overhead))
                .sum::<Duration>();
        // the overhead varies about as much as it is: a step counts once it took at least as long
        // again as stepping itself costs
        let cost = overhead + out_overhead * outs.len() as u32;
        if !self.enabled || corrected < self.threshold.max(cost) {
            return None;
        }
        let (path, line) = from?;
        let entry = self.lines.entry(path).or_default().entry(line).or_default();
        entry.total += corrected;
        entry.hits += 1;
        if let Some(cpu) = cpu {
            // the CPU can't have run longer than the step took
            *entry.cpu.get_or_insert(Duration::ZERO) += cpu.min(wall);
        }
        Some(corrected)
    }

    /// The label of a line: `12 ms`, `12 ms (3 cpu)`, `1.4 s ×12`.
    pub fn label(&self, path: &Path, line: usize) -> Option<String> {
        let time = self.lines.get(path)?.get(&line)?;
        Some(format_line_time(time))
    }

    /// The labelled lines of a file.
    pub fn file(&self, path: &Path) -> Option<&BTreeMap<usize, LineTime>> {
        self.lines.get(path)
    }

    /// Forgets the labels (and keeps the calibration).
    pub fn clear_labels(&mut self) {
        self.lines.clear();
    }

    /// Forgets everything, for a new session.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.samples.clear();
        self.out_samples.clear();
    }
}

fn push(samples: &mut VecDeque<Duration>, sample: Duration) {
    samples.push_back(sample);
    while samples.len() > SAMPLES {
        samples.pop_front();
    }
}

/// The median of the faster half of `samples`: most steps are cheap, so this is what stepping
/// itself costs, whatever a few slow lines take. `None` with fewer than [`MIN_SAMPLES`].
fn faster_median(samples: &VecDeque<Duration>) -> Option<Duration> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let mut sorted: Vec<Duration> = samples.iter().copied().collect();
    sorted.sort();
    let faster = &sorted[..sorted.len().div_ceil(2)];
    Some(faster[faster.len() / 2])
}

/// `850 µs`, `3.2 ms`, `120 ms`, `1.4 s`, `75 s`.
pub fn format_duration(duration: Duration) -> String {
    let micros = duration.as_micros();
    if micros < 1000 {
        format!("{micros} µs")
    } else if micros < 10_000 {
        format!("{:.1} ms", micros as f64 / 1000.0)
    } else if micros < 1_000_000 {
        format!("{} ms", micros / 1000)
    } else if micros < 10_000_000 {
        format!("{:.1} s", micros as f64 / 1_000_000.0)
    } else {
        format!("{} s", micros / 1_000_000)
    }
}

/// A line's label: `12 ms`, `12 ms (3 cpu)`, `1.4 s ×12`.
pub fn format_line_time(time: &LineTime) -> String {
    let mut label = format_duration(time.total);
    if let Some(cpu) = time.cpu {
        // the CPU part in the unit of the total, without it: "12 ms (3 cpu)"
        let micros = time.total.as_micros();
        let value = if micros < 1000 {
            format!("{}", cpu.as_micros())
        } else if micros < 10_000 {
            format!("{:.1}", cpu.as_secs_f64() * 1000.0)
        } else if micros < 1_000_000 {
            format!("{}", (cpu.as_secs_f64() * 1000.0).round() as u64)
        } else {
            format!("{:.1}", cpu.as_secs_f64())
        };
        label.push_str(&format!(" ({value} cpu)"));
    }
    if time.hits > 1 {
        label.push_str(&format!(" ×{}", time.hits));
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn at(line: usize) -> Option<(PathBuf, usize)> {
        Some((PathBuf::from("/w/main.rs"), line))
    }

    #[test]
    fn nothing_is_labelled_until_calibrated() {
        let mut timing = Timing::default();
        for _ in 0..MIN_SAMPLES - 1 {
            assert_eq!(timing.record(at(1), ms(50), &[], true, None), None);
        }
        assert!(timing.overhead().is_none());
        assert!(timing.lines.is_empty());
    }

    #[test]
    fn the_overhead_is_taken_off_and_cheap_lines_stay_clean() {
        let mut timing = Timing::default();
        for _ in 0..8 {
            timing.record(at(1), ms(4), &[], true, None);
        }
        assert_eq!(timing.overhead(), Some(ms(4)));
        // a cheap line: under the threshold after correction
        assert_eq!(
            timing.record(at(2), Duration::from_micros(4500), &[], true, None),
            None
        );
        assert_eq!(timing.label(Path::new("/w/main.rs"), 2), None);
        // a slow line: 30 ms - 4 ms
        assert_eq!(timing.record(at(3), ms(30), &[], true, None), Some(ms(26)));
        assert_eq!(timing.label(Path::new("/w/main.rs"), 3).unwrap(), "26 ms");
        // with a step out: until step outs are calibrated, they cost what a step in costs
        assert_eq!(
            timing.record(at(4), ms(10), &[ms(10)], false, None),
            Some(ms(12))
        );
    }

    #[test]
    fn slow_lines_dont_move_the_calibration_much() {
        let mut timing = Timing::default();
        for i in 0..32 {
            // a third of the plain steps are slow
            let wall = if i % 3 == 0 { ms(80) } else { ms(2) };
            timing.record(at(1), wall, &[], true, None);
        }
        assert_eq!(timing.overhead(), Some(ms(2)));
    }

    #[test]
    fn labels_add_up_visits_and_show_the_cpu() {
        let mut timing = Timing::default();
        for _ in 0..8 {
            timing.record(at(1), ms(1), &[], true, None);
        }
        timing.record(at(5), ms(13), &[], false, Some(ms(3)));
        assert_eq!(
            timing.label(Path::new("/w/main.rs"), 5).unwrap(),
            "12 ms (3 cpu)"
        );
        timing.record(at(5), ms(13), &[], false, Some(ms(3)));
        assert_eq!(
            timing.label(Path::new("/w/main.rs"), 5).unwrap(),
            "24 ms (6 cpu) ×2"
        );
        timing.clear_labels();
        assert_eq!(timing.label(Path::new("/w/main.rs"), 5), None);
        assert!(timing.overhead().is_some());
    }

    #[test]
    fn step_outs_have_their_own_overhead() {
        let mut timing = Timing::default();
        for _ in 0..8 {
            timing.record(at(1), ms(3), &[], true, None);
        }
        // step outs of cheap library calls cost 40 ms each in this debugger
        for _ in 0..8 {
            timing.record(at(2), ms(3), &[ms(40)], false, None);
        }
        assert_eq!(timing.out_overhead(), Some(ms(40)));
        // a library call that works for 100 ms
        assert_eq!(
            timing.record(at(3), ms(3), &[ms(140)], false, None),
            Some(ms(100))
        );
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(format_duration(Duration::from_micros(850)), "850 µs");
        assert_eq!(format_duration(Duration::from_micros(3200)), "3.2 ms");
        assert_eq!(format_duration(ms(120)), "120 ms");
        assert_eq!(format_duration(ms(1400)), "1.4 s");
        assert_eq!(format_duration(ms(75_000)), "75 s");
    }
}
