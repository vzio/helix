//! Debug mode's mode bar (this fork): three rows on the shared [mode bar](crate::ui::modebar).
//!
//! 1. The transport bar: the state on the left (`DEBUG ▶ playing`, `❚❚ paused`, `◀◀ rewind
//!    128/2,048`, `○ no session`, the engine's loading animation while it installs), the
//!    transport's badges in the middle (`|◀ ◀◀ ▶ ▶▶ ▶|`, `❚❚` instead of `▶` while playing: each a
//!    command, its key and a click), the speed and what playback follows on the right.
//! 2. The lineline: the record of lines executed (`vortex_view::debug::lineline`), scaled onto
//!    the bar's width: how densely the spectated thread ran in each cell, the playhead, the marks,
//!    the stretches before the thread started and after it exited, the full-speed gaps. A click
//!    seeks to the recorded stop nearest that point.
//! 3. The keys: badges for the rest, collapsing on a narrow window; on the right, what the last
//!    key did (or why it waited), else where the spectated thread is.

use std::time::Instant;

use vortex_view::{
    debug::{
        lineline::{Counter, Lineline, MarkKind},
        spectator::Mode,
        Status, ThreadKey,
    },
    graphics::{Modifier, Style},
    theme::DashboardCharset,
    Editor, Theme,
};

use crate::keymap::ReverseKeymap;
use crate::ui::modebar::{Bar, Cell, Line, Part, Row, Segment};

/// The rows debug mode takes on the mode bar.
pub const BAR_ROWS: u16 = 3;

/// The spot of the lineline's last column: the present.
const PRESENT: u64 = 0;

/// What a mark on the lineline shows, from the least important: when marks share a cell, the most
/// important shows (an exception, then a breakpoint, then a thread switch, then the rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Glyph {
    /// The selected variable read here (debug mode's variables: not recorded yet).
    Read,
    /// The selected variable written here (not recorded yet).
    Write,
    /// Every covered file had its hidden breakpoints from here.
    Covered,
    Pause,
    /// The program ended.
    End,
    /// Playback started following another thread.
    Switch,
    /// One of your breakpoints was hit (paused or not: a breakpoint of another type is recorded
    /// and continued).
    Breakpoint,
    Exception,
}

impl Glyph {
    pub const ALL: [Glyph; 8] = [
        Glyph::Read,
        Glyph::Write,
        Glyph::Covered,
        Glyph::Pause,
        Glyph::End,
        Glyph::Switch,
        Glyph::Breakpoint,
        Glyph::Exception,
    ];

    /// The glyph of a mark, if it is drawn as one (a full-speed run is a gap, a thread starting or
    /// exiting shows as the stretch where it doesn't exist).
    pub fn of(kind: &MarkKind) -> Option<Glyph> {
        Some(match kind {
            MarkKind::Breakpoint { .. } => Glyph::Breakpoint,
            MarkKind::Exception { .. } => Glyph::Exception,
            MarkKind::Pause { .. } => Glyph::Pause,
            // the first thread followed isn't a switch
            MarkKind::Switch { from: None, .. } => return None,
            MarkKind::Switch { .. } => Glyph::Switch,
            MarkKind::End => Glyph::End,
            MarkKind::Covered => Glyph::Covered,
            MarkKind::Read { .. } => Glyph::Read,
            MarkKind::Write { .. } => Glyph::Write,
            MarkKind::FullSpeed { .. }
            | MarkKind::ThreadStarted { .. }
            | MarkKind::ThreadExited { .. } => return None,
        })
    }

    /// One cell, in the theme's charset.
    pub fn symbol(self, ascii: bool) -> char {
        match (self, ascii) {
            (Glyph::Breakpoint, false) => '●',
            (Glyph::Breakpoint, true) => '*',
            (Glyph::Exception, false) => '✕',
            (Glyph::Exception, true) => '!',
            (Glyph::Pause, false) => '‖',
            (Glyph::Pause, true) => '"',
            (Glyph::Switch, false) => '⇄',
            (Glyph::Switch, true) => '/',
            (Glyph::End, false) => '■',
            (Glyph::End, true) => '$',
            (Glyph::Covered, false) => '✓',
            (Glyph::Covered, true) => '+',
            (Glyph::Read, false) => '▿',
            (Glyph::Read, true) => 'r',
            (Glyph::Write, false) => '▾',
            (Glyph::Write, true) => 'w',
        }
    }

    pub fn scope(self) -> &'static str {
        match self {
            Glyph::Breakpoint => "ui.modebar.lineline.breakpoint",
            Glyph::Exception => "ui.modebar.lineline.exception",
            Glyph::Pause => "ui.modebar.lineline.pause",
            Glyph::Switch => "ui.modebar.lineline.switch",
            Glyph::End => "ui.modebar.lineline.end",
            Glyph::Covered => "ui.modebar.lineline.covered",
            Glyph::Read => "ui.modebar.lineline.read",
            Glyph::Write => "ui.modebar.lineline.write",
        }
    }

    /// The scopes tried after its own.
    fn fallbacks(self) -> &'static [&'static str] {
        match self {
            Glyph::Breakpoint => &["ui.debug.breakpoint", "error"],
            Glyph::Exception => &["error"],
            Glyph::Pause => &["ui.debug.active", "ui.text.focus"],
            Glyph::Switch => &["info"],
            Glyph::End => &["ui.text"],
            Glyph::Covered => &["hint", "ui.text.inactive"],
            Glyph::Read => &["info"],
            Glyph::Write => &["warning"],
        }
    }
}

/// What a column of the lineline shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    /// A span of the counter: `[from, to)`. `density` is how densely the spectated thread ran in
    /// it, from 0 (not at all) to 1 (the densest cell); `dead` when the thread didn't exist yet,
    /// or anymore, for all of it.
    Span {
        from: Counter,
        to: Counter,
        density: f32,
        dead: bool,
        mark: Option<Glyph>,
        playhead: bool,
    },
    /// A column of a full-speed gap: the run at `at`, and the gap's `index`-th column of `width`.
    Gap { at: Counter, index: u16, width: u16 },
    /// The last column: the present (live, the playhead is here; in rewind, the end of the
    /// record; the program's end once it ended).
    Now { playhead: bool, mark: Option<Glyph> },
}

/// The label of a gap wide enough for it.
const GAP_LABEL: &str = "full speed";

/// How many hits of each span are looked at for its density: cheap however long the record.
const SAMPLES: u64 = 24;

/// The lineline laid out on `width` columns: the counter range from the oldest hit kept to now,
/// `spectated` the thread whose density is drawn (every thread's when `None`), `position` the
/// rewind position (`None`: live), `ended` once the program ended.
pub fn layout(
    record: &Lineline,
    spectated: Option<ThreadKey>,
    position: Option<Counter>,
    ended: bool,
    width: u16,
) -> Vec<Slot> {
    if width == 0 {
        return Vec::new();
    }
    let first = record.first_kept();
    let now = record.counter();
    let span = (now - first).max(1);
    let marks: Vec<_> = record.marks().filter(|mark| mark.at >= first).collect();

    // the full-speed runs, as gaps: wide enough for their label when there's room
    let gaps: Vec<Counter> = marks
        .iter()
        .filter(|mark| matches!(mark.kind, MarkKind::FullSpeed { .. }))
        .map(|mark| mark.at)
        .collect();
    let room = (width as usize).saturating_sub(1) / 3;
    let label = GAP_LABEL.chars().count() + 2;
    let gap_width = if gaps.len() * label <= room {
        label
    } else if gaps.len() * 3 <= room {
        3
    } else {
        1
    };
    // what's left for the counter, after the last column (now)
    let mut cells = (width as usize).saturating_sub(1);
    let mut kept_gaps = Vec::new();
    let mut gap_cells = 0;
    for &at in &gaps {
        if gap_cells + gap_width > room {
            break;
        }
        kept_gaps.push(at);
        gap_cells += gap_width;
    }
    cells = cells.saturating_sub(gap_cells).max(1);
    let cells = cells as u64;

    // the cell of a counter, and where each cell starts: cell k holds the counters whose
    // `(c - first) * cells / span` is k
    let cell_of =
        |at: Counter| (((at.saturating_sub(first)) * cells / span).min(cells - 1)) as usize;
    let start_of = |k: u64| first + (k * span).div_ceil(cells);

    let thread = spectated.and_then(|key| record.thread_index(key));
    let info = thread.and_then(|id| record.thread(id));
    let mut densities = Vec::with_capacity(cells as usize);
    for k in 0..cells {
        let (from, to) = (start_of(k), start_of(k + 1).min(now));
        let n = to.saturating_sub(from);
        // fewer hits than cells: each hit is drawn over the cells it spans
        let (from, n) = match n {
            0 if now > first => ((first + k * span / cells).min(now - 1), 1),
            n => (from, n),
        };
        let density = match (n, thread) {
            (0, _) => 0.0,
            (_, None) => 1.0,
            (n, Some(id)) => {
                let samples = n.min(SAMPLES);
                let hits = (0..samples)
                    .filter(|i| record.thread_at(from + i * n / samples) == Some(id))
                    .count();
                hits as f32 / samples as f32
            }
        };
        densities.push(density);
    }
    let densest = densities.iter().copied().fold(0.0, f32::max);

    // the marks in each cell, the most important kept; the ones at now go in the last column
    let mut cell_marks: Vec<Option<Glyph>> = vec![None; cells as usize];
    let mut now_mark: Option<Glyph> = None;
    for mark in &marks {
        let Some(glyph) = Glyph::of(&mark.kind) else {
            continue;
        };
        let slot = if mark.at >= now {
            &mut now_mark
        } else {
            &mut cell_marks[cell_of(mark.at)]
        };
        *slot = (*slot).max(Some(glyph));
    }
    let playhead_cell = position.map(&cell_of);

    let mut slots = Vec::with_capacity(width as usize);
    let mut next_gap = kept_gaps.iter().peekable();
    for k in 0..cells {
        while let Some(&&at) = next_gap.peek() {
            if cell_of(at) as u64 > k {
                break;
            }
            next_gap.next();
            for index in 0..gap_width as u16 {
                slots.push(Slot::Gap {
                    at,
                    index,
                    width: gap_width as u16,
                });
            }
        }
        let (from, to) = (start_of(k), start_of(k + 1));
        let dead = info.is_some_and(|info| {
            to <= info.started || info.exited.is_some_and(|exited| from >= exited)
        });
        let density = if densest > 0.0 {
            densities[k as usize] / densest
        } else {
            0.0
        };
        slots.push(Slot::Span {
            from,
            to,
            density,
            dead,
            mark: cell_marks[k as usize],
            playhead: playhead_cell == Some(k as usize),
        });
    }
    // gaps at the very end (a full-speed run that just ended)
    for &at in next_gap {
        for index in 0..gap_width as u16 {
            slots.push(Slot::Gap {
                at,
                index,
                width: gap_width as u16,
            });
        }
    }
    slots.truncate(width as usize - 1);
    slots.push(Slot::Now {
        playhead: position.is_none() && !ended,
        mark: if ended {
            Some(Glyph::End.max(now_mark.unwrap_or(Glyph::End)))
        } else {
            now_mark
        },
    });
    slots
}

/// The cell of a density: nothing for none, else the charset's ramp, faintest to densest.
fn density_symbol(density: f32, charset: DashboardCharset) -> char {
    if density <= 0.0 {
        return ' ';
    }
    let (ramp, _) = crate::dashboard::charset::ramp(charset);
    let index = ((density * ramp.len() as f32).ceil() as usize).clamp(1, ramp.len()) - 1;
    ramp[index]
}

fn first(theme: &Theme, scopes: &[&str]) -> Option<Style> {
    scopes.iter().find_map(|scope| theme.try_get_exact(scope))
}

/// The lineline's styles.
struct LinelineStyles {
    base: Style,
    playhead: Style,
    dead: Style,
    gap: Style,
    now: Style,
}

impl LinelineStyles {
    fn new(theme: &Theme) -> Self {
        let base = first(theme, &["ui.modebar.lineline", "ui.modebar.info"]).unwrap_or_default();
        let playhead = first(theme, &["ui.modebar.lineline.playhead", "ui.debug.active"])
            .unwrap_or_default()
            .add_modifier(Modifier::BOLD);
        let dead = first(theme, &["ui.modebar.lineline.dead", "error"]).unwrap_or_default();
        let gap =
            first(theme, &["ui.modebar.lineline.gap", "ui.text.inactive"]).unwrap_or_default();
        let now =
            first(theme, &["ui.modebar.lineline.now", "ui.text.inactive"]).unwrap_or_default();
        Self {
            base,
            playhead,
            dead,
            gap,
            now,
        }
    }

    fn mark(&self, theme: &Theme, glyph: Glyph) -> Style {
        let mut scopes = vec![glyph.scope()];
        scopes.extend(glyph.fallbacks());
        self.base.patch(first(theme, &scopes).unwrap_or_default())
    }
}

/// The lineline's cells for `slots`.
pub fn cells(slots: &[Slot], theme: &Theme, charset: DashboardCharset) -> Line {
    let ascii = charset == DashboardCharset::Ascii;
    let styles = LinelineStyles::new(theme);
    let playhead_symbol = if ascii { '|' } else { '┃' };
    let cells = slots
        .iter()
        .map(|slot| match *slot {
            Slot::Span {
                from,
                to,
                density,
                dead,
                mark,
                playhead,
            } => {
                let (symbol, style) = if playhead {
                    (playhead_symbol, styles.base.patch(styles.playhead))
                } else if let Some(glyph) = mark {
                    (glyph.symbol(ascii), styles.mark(theme, glyph))
                } else if dead {
                    (
                        if ascii { '_' } else { '─' },
                        styles.base.patch(styles.dead),
                    )
                } else {
                    (density_symbol(density, charset), styles.base)
                };
                Cell {
                    symbol,
                    style,
                    spot: Some(from + to.saturating_sub(from) / 2 + 1),
                }
            }
            Slot::Gap { at, index, width } => {
                let label: Vec<char> = GAP_LABEL.chars().collect();
                let fill = if ascii { '~' } else { '┄' };
                let symbol = if width as usize == label.len() + 2 && index >= 1 {
                    label.get(index as usize - 1).copied().unwrap_or(fill)
                } else {
                    fill
                };
                Cell {
                    symbol,
                    style: styles.base.patch(styles.gap),
                    spot: Some(at + 1),
                }
            }
            Slot::Now { playhead, mark } => {
                let (symbol, style) = if playhead {
                    (playhead_symbol, styles.base.patch(styles.playhead))
                } else if let Some(glyph) = mark {
                    (glyph.symbol(ascii), styles.mark(theme, glyph))
                } else {
                    (if ascii { ']' } else { '▕' }, styles.base.patch(styles.now))
                };
                Cell {
                    symbol,
                    style,
                    spot: Some(PRESENT),
                }
            }
        })
        .collect();
    Line { cells }
}

/// A click on the lineline: the present at its right end, else the recorded stop of the
/// spectated thread nearest that point.
pub fn click(editor: &mut Editor, spot: u64) {
    if spot == PRESENT {
        super::mode::present(editor);
    } else {
        super::mode::seek(editor, spot - 1);
    }
}

/// The thread the lineline shows: the spectated one, or in rewind the one of the stop shown.
fn shown_thread(editor: &Editor) -> Option<ThreadKey> {
    let spectator = &editor.debug.spectator;
    spectator
        .history
        .current()
        .map(|snapshot| snapshot.key)
        .or(spectator.spectated)
}

/// Whether a session is on: launched or attached, not ended.
pub fn has_session(editor: &Editor) -> bool {
    editor.debug_adapters.iter().next().is_some() && editor.debug.status.is_active()
}

/// The state on the left of the transport bar.
fn state(editor: &Editor) -> Segment {
    let debug = &editor.debug;
    let spectator = &debug.spectator;
    if debug.engine.task.is_some() {
        let mut spans = vec![("DEBUG ".to_string(), Style::default())];
        spans.extend(super::render::loading_spans(editor, 60));
        return Segment::spans(Part::State, spans).scope("ui.modebar.state.loading");
    }
    if let Some(snapshot) = spectator.history.current() {
        return Segment::state(format!(
            "DEBUG ◀◀ rewind {}/{}",
            vortex_view::debug::engine::thousands(snapshot.step),
            vortex_view::debug::engine::thousands(spectator.history.last_step)
        ))
        .scope("ui.modebar.state.rewind")
        .scope("ui.debug.rewind");
    }
    let (text, scope) = match &debug.status {
        Status::Idle if !has_session(editor) => ("DEBUG ○ no session".to_string(), "idle"),
        Status::Ended { code } => (
            match code {
                Some(code) => format!("DEBUG ○ exited {code}"),
                None => "DEBUG ○ ended".to_string(),
            },
            "idle",
        ),
        Status::Idle | Status::Starting => ("DEBUG ◌ starting".to_string(), "idle"),
        _ => match spectator.blocked(Instant::now()) {
            Some(_) => (
                format!(
                    "DEBUG ▶ {}",
                    spectator.label(Instant::now()).unwrap_or_default()
                ),
                "playing",
            ),
            None => match (spectator.mode, &debug.status) {
                (Mode::Playing, _) => ("DEBUG ▶ playing".to_string(), "playing"),
                (Mode::FastForward, _) => (
                    format!("DEBUG ▶▶ fast forward · {}", spectator.steps),
                    "playing",
                ),
                (Mode::Paused, Status::Running) => {
                    ("DEBUG ▶ running at full speed".to_string(), "playing")
                }
                (Mode::Paused, Status::Stopped { reason, .. }) => {
                    let reason = reason.trim();
                    // playback's own stops are lines: say only why it paused otherwise
                    if reason.is_empty()
                        || reason == "step"
                        || reason == vortex_view::debug::engine::LINE_REASON
                    {
                        ("DEBUG ❚❚ paused".to_string(), "paused")
                    } else {
                        (format!("DEBUG ❚❚ paused · {reason}"), "paused")
                    }
                }
                (Mode::Paused, _) => ("DEBUG ❚❚ paused".to_string(), "paused"),
            },
        },
    };
    let scope = match scope {
        "idle" => "ui.modebar.state.idle",
        "playing" => "ui.modebar.state.playing",
        _ => "ui.modebar.state.paused",
    };
    Segment::state(text).scope(scope)
}

/// The transport's badges: previous breakpoint, previous line, play or pause, next line, next
/// breakpoint, and the present in rewind.
fn transport(editor: &Editor, keys: &ReverseKeymap) -> Vec<Segment> {
    let playing = editor.debug.spectator.is_playing();
    let mut badges = vec![
        Segment::key("debug_breakpoint_back", "|◀", keys),
        Segment::key("debug_line_back", "◀◀", keys),
        Segment::key("debug_play_pause", if playing { "❚❚" } else { "▶" }, keys),
        Segment::key("debug_line_forward", "▶▶", keys),
        Segment::key("debug_breakpoint_forward", "▶|", keys),
    ];
    if editor.debug.is_rewinding() {
        badges.push(Segment::key("dap_live", "present", keys));
    }
    badges
}

/// `8 lines/s · own` on the right of the transport bar.
fn speed(editor: &Editor) -> String {
    let spectator = &editor.debug.spectator;
    let lines = spectator.lines_per_second();
    let lines = if lines.fract() == 0.0 {
        format!("{lines:.0}")
    } else {
        format!("{lines}")
    };
    let noun = if lines == "1" { "line" } else { "lines" };
    format!("{lines} {noun}/s · {}", spectator.follow.label())
}

/// What the keys row shows on its right: the note, else where the spectated thread is.
fn note(editor: &Editor) -> String {
    if let Some(note) = editor
        .debug
        .mode
        .as_ref()
        .and_then(|mode| mode.note.clone())
    {
        return note;
    }
    let Some(key) = shown_thread(editor) else {
        return String::new();
    };
    let name = editor.debug.thread_name(key);
    let frame = editor
        .debug
        .spectator
        .history
        .current()
        .and_then(|snapshot| snapshot.frames.first().cloned())
        .or_else(|| editor.debug_adapters.current_stack_frame().cloned())
        .or_else(|| editor.debug.spectator.shown_frame.clone());
    let place = frame.and_then(|frame| {
        let path = frame.source.as_ref()?.path.clone()?;
        let cwd = vortex_stdx::env::current_working_dir();
        let path = vortex_view::debug::spectator::short_path(&path, &cwd).to_path_buf();
        Some(format!("{}:{}", path.display(), frame.line))
    });
    match place {
        Some(place) => format!("{name} · {place}"),
        None => name,
    }
}

/// The keys row's badges.
fn keys_row(editor: &Editor, keys: &ReverseKeymap) -> Vec<Segment> {
    let viewing = editor.debug.viewing();
    let session = has_session(editor);
    let follow = editor.debug.spectator.follow.label();
    let mut badges = Vec::new();
    if !session {
        badges.push(Segment::key("dap_launch", "start", keys));
    }
    badges.push(Segment::key(
        "debug_view",
        if viewing { "follow" } else { "view" },
        keys,
    ));
    badges.push(Segment::key("dap_toggle_breakpoint", "breakpoint", keys));
    if viewing {
        badges.push(Segment::key("debug_breakpoint_kind", "type", keys));
    }
    if session {
        badges.extend([
            Segment::key("dap_follow_mode", follow, keys),
            Segment::key("dap_fast_forward", "change", keys),
            Segment::key("dap_run_to_cursor", "cursor", keys),
            Segment::key("dap_next_thread", "thread", keys),
            Segment::key("dap_terminate", "stop", keys),
        ]);
    }
    badges.extend([
        Segment::key("debug_panel_focus", "panel", keys),
        Segment::key("debug_help", "keys", keys),
        Segment::key("debug_leave", "leave", keys),
    ]);
    badges
}

/// Debug mode's bar, for a lineline `width` columns wide.
pub fn bar(editor: &Editor, keys: &ReverseKeymap, width: u16) -> Bar {
    let debug = &editor.debug;
    let position = debug
        .spectator
        .history
        .current()
        .and_then(|snapshot| snapshot.counter);
    let ended = matches!(debug.status, Status::Ended { .. });
    let slots = layout(
        &debug.engine.record,
        shown_thread(editor),
        position,
        ended,
        width,
    );
    let charset = editor.theme.effects().dashboard_charset;
    let line = cells(&slots, &editor.theme, charset);
    Bar {
        rows: vec![
            Row {
                left: vec![state(editor)],
                middle: transport(editor, keys),
                // the speed between its keys: `j −  8 lines/s · own  k +`
                right: vec![
                    Segment::key("dap_slower", "−", keys),
                    Segment::info(speed(editor)),
                    Segment::key("dap_faster", "+", keys),
                ],
                line: None,
            },
            Row {
                line: Some(line),
                ..Row::default()
            },
            Row {
                left: keys_row(editor, keys),
                right: vec![Segment::note(note(editor))],
                ..Row::default()
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;
    use vortex_dap::registry::DebugAdapterId;
    use vortex_view::debug::lineline::Why;

    fn key(n: usize) -> ThreadKey {
        ThreadKey {
            session: DebugAdapterId::default(),
            thread: serde_json::from_value(serde_json::json!(n)).unwrap(),
        }
    }

    fn symbols(slots: &[Slot], charset: DashboardCharset) -> String {
        cells(slots, &Theme::default(), charset)
            .cells
            .iter()
            .map(|cell| cell.symbol)
            .collect()
    }

    /// `hits` hits alternating between `main` and `worker` (the worker every `every` hits).
    fn record(main: ThreadKey, worker: ThreadKey, hits: usize, every: usize) -> Lineline {
        let mut record = Lineline::default();
        record.thread_started(main);
        for i in 0..hits {
            let thread = if i % every == 0 { worker } else { main };
            record.hit(
                thread,
                Some((Path::new("/a.rs"), i % 10)),
                Why::Line,
                Some(main),
            );
        }
        record
    }

    #[test]
    fn scale_and_live_playhead() {
        let main = key(1);
        let record = record(main, key(2), 100, 1000);
        let slots = layout(&record, Some(main), None, false, 21);
        assert_eq!(slots.len(), 21);
        // 20 cells of 5 hits each, the playhead in the last column
        let spans: Vec<_> = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Span { from, to, .. } => Some((*from, *to)),
                _ => None,
            })
            .collect();
        assert_eq!(spans.len(), 20);
        assert_eq!(spans[0], (0, 5));
        assert_eq!(spans[19], (95, 100));
        assert_eq!(
            slots.last(),
            Some(&Slot::Now {
                playhead: true,
                mark: None
            })
        );
        let text = symbols(&slots, DashboardCharset::Blocks);
        assert!(text.ends_with('┃'), "{text}");
    }

    #[test]
    fn playhead_in_rewind() {
        let main = key(1);
        let record = record(main, key(2), 100, 1000);
        let slots = layout(&record, Some(main), Some(52), false, 21);
        let at = slots
            .iter()
            .position(|slot| matches!(slot, Slot::Span { playhead: true, .. }))
            .unwrap();
        // 52 is in the cell of 50..55
        assert_eq!(at, 10);
        assert!(matches!(
            slots.last(),
            Some(Slot::Now {
                playhead: false,
                ..
            })
        ));
    }

    #[test]
    fn density_of_the_spectated_thread() {
        let (main, worker) = (key(1), key(2));
        let mut record = Lineline::default();
        record.thread_started(main);
        // the first half: only the worker; the second: only main
        for i in 0..200 {
            let thread = if i < 100 { worker } else { main };
            record.hit(thread, Some((Path::new("/a.rs"), 1)), Why::Line, Some(main));
        }
        let slots = layout(&record, Some(main), None, false, 11);
        let densities: Vec<f32> = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Span { density, .. } => Some(*density),
                _ => None,
            })
            .collect();
        assert!(densities[..5].iter().all(|d| *d == 0.0), "{densities:?}");
        assert!(densities[5..].iter().all(|d| *d == 1.0), "{densities:?}");
        let text = symbols(&slots, DashboardCharset::Blocks);
        assert_eq!(text.chars().take(5).collect::<String>(), "     ");
        assert!(text.contains('█'), "{text}");
        // in ASCII
        let text = symbols(&slots, DashboardCharset::Ascii);
        assert!(text.contains('@') && text.is_ascii(), "{text}");
    }

    #[test]
    fn marks_and_their_priority() {
        let (main, worker) = (key(1), key(2));
        let mut record = Lineline::default();
        record.thread_started(main);
        for i in 0..100 {
            let why = match i {
                10 => Why::Breakpoint,
                30 => Why::Pause,
                50 | 51 => Why::Exception,
                _ => Why::Line,
            };
            record.hit(main, Some((Path::new("/a.rs"), 1)), why, Some(main));
            if i == 50 {
                // a breakpoint in the same cell as the exception
                record.hit(
                    main,
                    Some((Path::new("/a.rs"), 2)),
                    Why::Breakpoint,
                    Some(main),
                );
            }
            if i == 70 {
                record.hit(
                    worker,
                    Some((Path::new("/a.rs"), 2)),
                    Why::Line,
                    Some(worker),
                );
            }
        }
        record.mark(MarkKind::Covered);
        let slots = layout(&record, Some(main), None, false, 101);
        let marks: Vec<Glyph> = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Span { mark, .. } => *mark,
                _ => None,
            })
            .collect();
        assert!(marks.contains(&Glyph::Breakpoint));
        assert!(marks.contains(&Glyph::Pause));
        assert!(marks.contains(&Glyph::Switch));
        // the exception wins its cell over the breakpoint in it
        let cell = slots
            .iter()
            .find(|slot| matches!(slot, Slot::Span { from, to, .. } if (*from..*to).contains(&51)))
            .unwrap();
        assert!(matches!(
            cell,
            Slot::Span {
                mark: Some(Glyph::Exception),
                ..
            }
        ));
        // covered at now: the last column
        assert_eq!(
            slots.last(),
            Some(&Slot::Now {
                playhead: true,
                mark: Some(Glyph::Covered)
            })
        );
        // priorities
        assert!(Glyph::Exception > Glyph::Breakpoint);
        assert!(Glyph::Breakpoint > Glyph::Switch);
        assert!(Glyph::Switch > Glyph::End && Glyph::Switch > Glyph::Pause);
        // every glyph is one column, plain (no emoji presentation), ASCII in the ASCII charset
        for glyph in Glyph::ALL {
            for ascii in [false, true] {
                let symbol = glyph.symbol(ascii);
                assert_eq!(
                    vortex_core::unicode::width::UnicodeWidthChar::width(symbol),
                    Some(1)
                );
                assert!(!ascii || symbol.is_ascii());
            }
        }
    }

    #[test]
    fn dead_before_start_and_after_exit() {
        let (main, worker) = (key(1), key(2));
        let mut record = Lineline::default();
        record.thread_started(main);
        for i in 0..300 {
            if i == 100 {
                record.thread_started(worker);
            }
            if i == 200 {
                record.thread_exited(worker);
            }
            let thread = if (100..200).contains(&i) && i % 2 == 0 {
                worker
            } else {
                main
            };
            record.hit(thread, Some((Path::new("/a.rs"), 1)), Why::Line, Some(main));
        }
        let slots = layout(&record, Some(worker), None, false, 31);
        let dead: Vec<bool> = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Span { dead, .. } => Some(*dead),
                _ => None,
            })
            .collect();
        assert!(dead[..10].iter().all(|d| *d), "{dead:?}");
        assert!(dead[10..20].iter().all(|d| !*d), "{dead:?}");
        assert!(dead[20..].iter().all(|d| *d), "{dead:?}");
        let text = symbols(&slots, DashboardCharset::Blocks);
        assert!(text.starts_with("──────────"), "{text}");
        // the main thread lived all along
        let slots = layout(&record, Some(main), None, false, 31);
        assert!(!slots
            .iter()
            .any(|slot| matches!(slot, Slot::Span { dead: true, .. })));
    }

    #[test]
    fn full_speed_gap() {
        let main = key(1);
        let mut record = record(main, key(2), 50, 1000);
        record.mark(MarkKind::FullSpeed {
            duration: Duration::from_millis(300),
        });
        for i in 0..50 {
            record.hit(main, Some((Path::new("/a.rs"), i)), Why::Line, Some(main));
        }
        let slots = layout(&record, Some(main), None, false, 81);
        assert_eq!(slots.len(), 81);
        let text = symbols(&slots, DashboardCharset::Blocks);
        assert!(text.contains("┄full speed┄"), "{text}");
        // the gap sits where the counter was, halfway
        let gap = text.chars().position(|c| c == '┄').unwrap();
        assert!((30..45).contains(&gap), "{gap}: {text}");
        // narrow: a short gap, no label
        let slots = layout(&record, Some(main), None, false, 21);
        let text = symbols(&slots, DashboardCharset::Blocks);
        assert!(text.contains('┄') && !text.contains("full"), "{text}");
        let text = symbols(&slots, DashboardCharset::Ascii);
        assert!(text.contains('~') && text.is_ascii(), "{text}");
    }

    #[test]
    fn spots_seek_to_their_span_and_the_end_is_the_present() {
        let main = key(1);
        let record = record(main, key(2), 100, 1000);
        let slots = layout(&record, Some(main), None, false, 21);
        let line = cells(&slots, &Theme::default(), DashboardCharset::Blocks);
        assert_eq!(line.cells[0].spot, Some(2 + 1));
        assert_eq!(line.cells[10].spot, Some(52 + 1));
        assert_eq!(line.cells.last().unwrap().spot, Some(PRESENT));
    }

    #[test]
    fn program_end() {
        let main = key(1);
        let mut record = record(main, key(2), 100, 1000);
        record.mark(MarkKind::End);
        let slots = layout(&record, Some(main), None, true, 21);
        assert_eq!(
            slots.last(),
            Some(&Slot::Now {
                playhead: false,
                mark: Some(Glyph::End)
            })
        );
        assert!(symbols(&slots, DashboardCharset::Blocks).ends_with('■'));
    }

    #[test]
    fn empty_record() {
        let slots = layout(&Lineline::default(), None, None, false, 10);
        assert_eq!(slots.len(), 10);
        assert!(layout(&Lineline::default(), None, None, false, 0).is_empty());
    }
}
