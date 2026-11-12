//! Drawing the dashboard: a game main menu, centered, with generous spacing that collapses in
//! small windows.
//!
//! From top to bottom: the logo and the credit line, the project and its languages (side by
//! side in wide windows, stacked otherwise; only in git repositories), and the actions. When the
//! window is too short, the layout gives up room step by step (`Layout::all`): smaller gaps, a
//! smaller logo, fewer rows of languages and worktrees, the actions on one row, and at last the
//! stats.

use std::path::Path;
use std::time::{Duration, Instant};

use vortex_core::unicode::width::{UnicodeWidthChar, UnicodeWidthStr};
use vortex_view::graphics::{Modifier, Rect, Style};
use vortex_view::Theme;

use tui::buffer::Buffer as Surface;

use super::languages::{self, display_name, thousands, Counts, OTHER};
use super::logo::{self, Logo};
use super::stats::{Slot, Stats, INTRO, REVEAL};

/// What the dashboard draws, apart from the time.
pub struct Model<'a> {
    pub stats: &'a Stats,
    /// When the dashboard opened: the start of the animations.
    pub born: Instant,
    /// The folder it opened in.
    pub dir: &'a Path,
    pub hints: &'a Hints,
}

/// The keys of the keymap that do what the actions do, so the dashboard teaches them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    pub files: Option<String>,
    pub tree: Option<String>,
    pub history: Option<String>,
    pub quit: Option<String>,
}

/// The styles of the dashboard, from the theme: the `ui.dashboard.*` scopes, else common scopes
/// every theme has.
pub struct Styles {
    logo: Style,
    shadow: Style,
    credit: Style,
    heading: Style,
    detail: Style,
    label: Style,
    value: Style,
    good: Style,
    warning: Style,
    key: Style,
    action: Style,
    hint: Style,
    spinner: Style,
    track: Style,
    /// The colors of the languages in the bar, in order.
    palette: Vec<Style>,
    other: Style,
}

/// The first of `scopes` the theme has (exactly), else the default style.
fn pick(theme: &Theme, scopes: &[&str]) -> Style {
    scopes
        .iter()
        .find_map(|scope| theme.try_get_exact(scope))
        .unwrap_or_default()
}

/// The scopes the bar takes its colors from, in order (their foreground only).
pub const PALETTE: [&str; 5] = ["function", "string", "keyword", "type", "constant"];

impl Styles {
    pub fn new(theme: &Theme) -> Self {
        let text = pick(theme, &["ui.text"]);
        let dim = pick(theme, &["ui.text.inactive", "comment", "ui.linenr"]);
        let logo = pick(theme, &["ui.dashboard.logo", "keyword", "ui.text.focus"]);
        let key = theme.try_get_exact("ui.dashboard.key").unwrap_or_else(|| {
            match theme.try_get_exact("ui.selection") {
                Some(selection) => selection
                    .patch(pick(theme, &["ui.text.focus", "ui.text"]))
                    .add_modifier(Modifier::BOLD),
                None => Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            }
        });
        let foreground = |style: Style| match style.fg {
            Some(fg) => Style::default().fg(fg),
            None => text,
        };
        let palette = PALETTE
            .iter()
            .filter_map(|scope| theme.try_get(scope).and_then(|style| style.fg))
            .map(|fg| Style::default().fg(fg))
            .collect();
        Self {
            logo,
            shadow: pick(
                theme,
                &["ui.dashboard.logo.shadow", "ui.text.inactive", "comment"],
            ),
            credit: pick(
                theme,
                &["ui.dashboard.credit", "ui.text.inactive", "comment"],
            ),
            heading: pick(theme, &["ui.dashboard.heading", "ui.text.focus", "ui.text"])
                .add_modifier(Modifier::BOLD),
            detail: dim,
            label: pick(
                theme,
                &["ui.dashboard.label", "ui.text.inactive", "comment"],
            ),
            value: pick(theme, &["ui.dashboard.value", "ui.text"]),
            good: foreground(pick(theme, &["diff.plus", "ui.text"])),
            warning: foreground(pick(theme, &["warning", "ui.text"])),
            key,
            action: pick(theme, &["ui.dashboard.action", "ui.text"]),
            hint: pick(theme, &["ui.dashboard.hint", "ui.text.inactive", "comment"]),
            spinner: theme.try_get_exact("ui.dashboard.spinner").unwrap_or(logo),
            track: dim,
            palette,
            other: foreground(dim),
        }
    }

    fn language(&self, index: usize, language: &str) -> Style {
        if language == OTHER || self.palette.is_empty() {
            self.other
        } else {
            self.palette[index % self.palette.len()]
        }
    }
}

/// A piece of text in one style.
type Span = (String, Style);

/// Drawing clipped to an area.
struct Canvas<'a> {
    area: Rect,
    surface: &'a mut Surface,
}

impl Canvas<'_> {
    /// Draws `spans` from column `x` of row `y`, in at most `width` columns, cutting the last
    /// visible one with `…` when they don't fit. Returns the column after the last cell drawn.
    fn spans(&mut self, x: u16, y: u16, spans: &[Span], width: u16) -> u16 {
        if y < self.area.top() || y >= self.area.bottom() {
            return x;
        }
        let right = x.saturating_add(width).min(self.area.right());
        let total: usize = spans.iter().map(|(text, _)| text.width()).sum();
        let fits = total <= right.saturating_sub(x) as usize;
        let mut col = x;
        for (text, style) in spans {
            for c in text.chars() {
                let w = c.width().unwrap_or(0) as u16;
                if w == 0 {
                    continue;
                }
                // room for the ellipsis when cutting
                let limit = if fits { right } else { right.saturating_sub(1) };
                if col + w > limit {
                    if !fits && col < right {
                        self.surface.set_string(col, y, "…", *style);
                        col += 1;
                    }
                    return col;
                }
                if col >= self.area.left() {
                    self.surface
                        .set_string(col, y, c.encode_utf8(&mut [0; 4]), *style);
                }
                col += w;
            }
        }
        col
    }

    fn text(&mut self, x: u16, y: u16, text: &str, style: Style, width: u16) -> u16 {
        self.spans(x, y, &[(text.to_string(), style)], width)
    }
}

fn spans_width(spans: &[Span]) -> u16 {
    spans.iter().map(|(text, _)| text.width() as u16).sum()
}

/// The first `columns` columns of `spans`: how a value types itself in.
fn cut(spans: &[Span], columns: usize) -> Vec<Span> {
    let mut left = columns;
    let mut out = Vec::new();
    for (text, style) in spans {
        if left == 0 {
            break;
        }
        let mut piece = String::new();
        for c in text.chars() {
            let w = c.width().unwrap_or(0);
            if w > left {
                left = 0;
                break;
            }
            left -= w;
            piece.push(c);
        }
        out.push((piece, *style));
    }
    out
}

/// How far an animation that started at `start` and lasts `length` is at `now`, from 0 to 1.
fn progress(start: Instant, length: Duration, now: Instant) -> f64 {
    let elapsed = now.saturating_duration_since(start).as_secs_f64();
    (elapsed / length.as_secs_f64()).clamp(0.0, 1.0)
}

const SPINNER: [&str; 4] = ["◜", "◝", "◞", "◟"];

fn spinner(born: Instant, now: Instant) -> &'static str {
    let frame = now.saturating_duration_since(born).as_millis() / 100;
    SPINNER[frame as usize % SPINNER.len()]
}

/// A row of a section: a label and a value, which shimmers while it loads and types itself in
/// when it arrives.
struct Row {
    label: String,
    label_style: Option<Style>,
    value: Value,
}

enum Value {
    Loading,
    Ready(Vec<Span>, Instant),
}

impl Row {
    fn new(label: impl Into<String>, value: Value) -> Self {
        Self {
            label: label.into(),
            label_style: None,
            value,
        }
    }
}

/// How many languages the legend lists in a row in `width` columns.
fn legend_columns(width: u16) -> u16 {
    match width {
        66.. => 3,
        40.. => 2,
        _ => 1,
    }
}

/// The rows of the legend: enough for the most segments there can be, at most `max`. Fixed
/// rather than by the languages counted so far, so the layout doesn't move while counting.
fn legend_rows(width: u16, max: u16) -> u16 {
    let segments = LANGUAGES as u16 + 1;
    segments.div_ceil(legend_columns(width)).min(max)
}

/// The languages with a segment of their own in the bar, at most.
const LANGUAGES: usize = 5;

/// The width of the labels of the project section.
const LABEL: u16 = 10;
/// How many worktrees are listed at most, before "and N more".
const WORKTREES: usize = 3;

/// The rows of the project section, from the stats read so far.
fn project_rows(stats: &Stats, s: &Styles, worktrees_max: usize) -> Vec<Row> {
    fn value<T: Clone>(slot: &Slot<T>, s: &Styles, f: impl FnOnce(&T) -> Vec<Span>) -> Value {
        match slot.get() {
            None => Value::Loading,
            Some((Ok(value), at)) => Value::Ready(f(&value), at),
            Some((Err(err), at)) => Value::Ready(vec![(err, s.label)], at),
        }
    }
    let mut rows = Vec::new();
    let head = stats.head.get();
    rows.push(Row::new(
        "branch",
        value(&stats.head, s, |head| match (&head.branch, &head.commit) {
            (Some(branch), _) => vec![(branch.clone(), s.value)],
            (None, Some(commit)) => vec![
                ("detached at ".into(), s.label),
                (commit.short_id.clone(), s.value),
            ],
            (None, None) => vec![("detached".into(), s.label)],
        }),
    ));
    rows.push(Row::new(
        "commit",
        value(&stats.head, s, |head| match &head.commit {
            Some(commit) => vec![
                (commit.short_id.clone(), s.detail),
                ("  ".into(), s.value),
                (commit.summary.clone(), s.value),
            ],
            None => vec![("no commits yet".into(), s.label)],
        }),
    ));
    let detached = matches!(&head, Some((Ok(head), _)) if head.branch.is_none());
    rows.push(Row::new(
        "upstream",
        value(&stats.upstream, s, |upstream| match upstream {
            Some(upstream) => {
                let mut spans = vec![(upstream.name.clone(), s.value), ("  ".into(), s.value)];
                match upstream.ahead {
                    0 => spans.push(("all pushed".into(), s.good)),
                    n => spans.push((format!("{n} unpushed"), s.warning)),
                }
                spans
            }
            None if detached => vec![("none while detached".into(), s.label)],
            None => vec![("no upstream".into(), s.label)],
        }),
    ));
    rows.push(Row::new(
        "remote",
        value(&stats.remote, s, |remote| match remote {
            Some(remote) => vec![
                (remote.name.clone(), s.value),
                ("  ".into(), s.value),
                (remote.url.clone(), s.detail),
            ],
            None => vec![("no remote".into(), s.label)],
        }),
    ));
    rows.push(Row::new(
        "changes",
        value(&stats.changes, s, |changes| {
            if changes.is_clean() {
                return vec![("clean".into(), s.good)];
            }
            let mut spans = Vec::new();
            for (n, what) in [
                (changes.staged, "staged"),
                (changes.unstaged, "unstaged"),
                (changes.untracked, "untracked"),
            ] {
                if n == 0 {
                    continue;
                }
                if !spans.is_empty() {
                    spans.push((" · ".into(), s.label));
                }
                spans.push((format!("{n} {what}"), s.value));
            }
            spans
        }),
    ));
    if let Some((Ok(Some(in_progress)), at)) = stats.in_progress.get() {
        let mut spans = vec![];
        if in_progress.operation != vortex_vcs::summary::Operation::Conflicts {
            spans.push(("in progress".to_string(), s.warning));
        }
        if in_progress.conflicted > 0 {
            if !spans.is_empty() {
                spans.push((" · ".into(), s.label));
            }
            let files = if in_progress.conflicted == 1 {
                "file"
            } else {
                "files"
            };
            spans.push((
                format!("{} conflicted {files}", in_progress.conflicted),
                s.warning,
            ));
        }
        rows.push(Row {
            label: in_progress.operation.name().to_string(),
            label_style: Some(s.warning),
            value: Value::Ready(spans, at),
        });
    }
    if let Some((Ok(worktrees), at)) = stats.worktrees.get() {
        let shown = if worktrees.len() > worktrees_max {
            worktrees_max.saturating_sub(1)
        } else {
            worktrees.len()
        };
        for (i, worktree) in worktrees.iter().take(shown).enumerate() {
            let path = vortex_stdx::path::fold_home_dir(&worktree.path);
            let mut spans = vec![
                (path.display().to_string(), s.value),
                ("  ".into(), s.value),
            ];
            match (&worktree.branch, &worktree.short_id) {
                (Some(branch), _) => spans.push((branch.clone(), s.detail)),
                (None, Some(id)) => spans.push((format!("detached at {id}"), s.label)),
                (None, None) => {}
            }
            let label = if i == 0 { "worktrees" } else { "" };
            rows.push(Row::new(label, Value::Ready(spans, at)));
        }
        if shown < worktrees.len() {
            let more = worktrees.len() - shown;
            let label = if shown == 0 { "worktrees" } else { "" };
            let text = if shown == 0 {
                format!("{more} more")
            } else {
                format!("and {more} more")
            };
            rows.push(Row::new(label, Value::Ready(vec![(text, s.label)], at)));
        }
    }
    rows
}

/// How many rows the project section takes, its heading included.
fn project_height(stats: &Stats, worktrees_max: usize) -> u16 {
    let mut rows = 1 + 5;
    if let Some((Ok(Some(_)), _)) = stats.in_progress.get() {
        rows += 1;
    }
    if let Some((Ok(worktrees), _)) = stats.worktrees.get() {
        rows += worktrees.len().min(worktrees_max);
    }
    rows as u16
}

/// A layout: how much room each part takes. [`Layout::all`] lists them from the roomiest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    logo: Logo,
    /// The empty rows between the parts.
    gap: u16,
    /// The rows of language names under the bar, at most.
    legend: u16,
    worktrees: usize,
    /// The actions as a column (a menu) rather than one row.
    menu: bool,
    stats: bool,
}

impl Layout {
    fn all() -> Vec<Layout> {
        let roomy = Layout {
            logo: logo::LARGE,
            gap: 2,
            legend: 3,
            worktrees: WORKTREES,
            menu: true,
            stats: true,
        };
        let steps: [fn(Layout) -> Layout; 10] = [
            |l| Layout { gap: 1, ..l },
            |l| Layout { legend: 2, ..l },
            |l| Layout { legend: 1, ..l },
            |l| Layout {
                logo: logo::MEDIUM,
                ..l
            },
            |l| Layout { worktrees: 1, ..l },
            |l| Layout { menu: false, ..l },
            |l| Layout {
                logo: logo::SMALL,
                ..l
            },
            |l| Layout { legend: 0, ..l },
            |l| Layout { gap: 0, ..l },
            |l| Layout { stats: false, ..l },
        ];
        let mut layouts = vec![roomy];
        for step in steps {
            layouts.push(step(*layouts.last().unwrap()));
        }
        // without the stats, the actions as a menu again if they fit that way
        let last = *layouts.last().unwrap();
        layouts.push(Layout {
            menu: true,
            gap: 1,
            ..last
        });
        layouts.push(Layout { menu: true, ..last });
        layouts.push(last);
        layouts
    }
}

/// The two stats sections side by side from this width on, each this wide.
const WIDE: u16 = 108;
const PANEL: u16 = 48;
const PANEL_GAP: u16 = 8;
/// The widest the sections get when stacked.
const STACKED: u16 = 72;

struct Action {
    key: &'static str,
    label: &'static str,
    /// The label when the actions are on one row.
    short: &'static str,
    hint: Option<String>,
}

fn actions(model: &Model) -> Vec<Action> {
    let mut actions = vec![
        Action {
            key: "i",
            short: "type",
            label: "Start typing",
            hint: None,
        },
        Action {
            key: "f",
            short: "files",
            label: "Find a file",
            hint: model.hints.files.clone(),
        },
        Action {
            key: "e",
            short: "tree",
            label: "File tree",
            hint: model.hints.tree.clone(),
        },
    ];
    if model.stats.shows_git() {
        actions.push(Action {
            key: "l",
            short: "history",
            label: "Git history",
            hint: model.hints.history.clone(),
        });
    }
    actions.push(Action {
        key: "q",
        short: "quit",
        label: "Quit",
        hint: model.hints.quit.clone(),
    });
    actions
}

/// Draws the dashboard of `model` in `area` as it is at `now`.
pub fn render(model: &Model, theme: &Theme, area: Rect, surface: &mut Surface, now: Instant) {
    let styles = Styles::new(theme);
    let s = &styles;
    surface.clear_with(area, theme.get("ui.background"));
    let mut canvas = Canvas { area, surface };
    let git = model.stats.shows_git();
    let wide = area.width >= WIDE;
    let panel = if wide {
        PANEL
    } else {
        STACKED.min(area.width.saturating_sub(4)).max(1)
    };
    let actions = actions(model);

    let height = |layout: &Layout| -> u16 {
        let logo = layout.logo.height() + if layout.logo.height() > 1 { 2 } else { 1 };
        let menu = if layout.menu { actions.len() as u16 } else { 1 };
        let mut total = logo + layout.gap + 1 + menu;
        if git && layout.stats {
            let project = project_height(model.stats, layout.worktrees);
            let languages = 2 + legend_rows(panel, layout.legend);
            let stats = if wide {
                project.max(languages)
            } else {
                project + layout.gap.max(1) + languages
            };
            total += stats + layout.gap + 1;
        }
        total
    };
    let layouts = Layout::all();
    let row_width = actions_row_width(&actions);
    let fits = |layout: &&Layout| {
        height(layout) <= area.height
            && layout.logo.width() <= area.width
            && (layout.menu || row_width <= area.width)
    };
    let layout = layouts
        .iter()
        .find(fits)
        .or_else(|| layouts.iter().rfind(|l| l.logo.width() <= area.width))
        .copied()
        .unwrap_or(*layouts.last().unwrap());

    let total = height(&layout);
    let mut y = area.y + area.height.saturating_sub(total) / 2;

    // the logo, drawing itself in
    draw_logo(&mut canvas, layout.logo, y, model.born, now, s);
    y += layout.logo.height();
    if layout.logo.height() > 1 {
        y += 1;
    }
    let credit = logo::CREDIT;
    let x = center(area, credit.width() as u16);
    canvas.text(x, y, credit, s.credit, area.width);
    y += 1 + layout.gap + 1;

    if git && layout.stats {
        if wide {
            let left = center(area, PANEL * 2 + PANEL_GAP);
            let project_rows = draw_project(&mut canvas, model, left, y, PANEL, layout, now, s);
            let languages_rows = draw_languages(
                &mut canvas,
                model,
                left + PANEL + PANEL_GAP,
                y,
                PANEL,
                layout,
                now,
                s,
            );
            y += project_rows.max(languages_rows);
        } else {
            let left = center(area, panel);
            y += draw_project(&mut canvas, model, left, y, panel, layout, now, s);
            y += layout.gap.max(1);
            y += draw_languages(&mut canvas, model, left, y, panel, layout, now, s);
        }
        y += layout.gap + 1;
    }

    draw_actions(&mut canvas, &actions, y, layout.menu, s);
}

fn center(area: Rect, width: u16) -> u16 {
    area.x + area.width.saturating_sub(width) / 2
}

fn draw_logo(canvas: &mut Canvas, logo: Logo, y: u16, born: Instant, now: Instant, s: &Styles) {
    let area = canvas.area;
    let x = center(area, logo.width());
    let intro = progress(born, INTRO, now);
    // the spiral from its center outwards, over the first two thirds of the intro
    let order = logo.reveal_order();
    let spiral_shown = if order.is_empty() {
        usize::MAX
    } else {
        ((order.len() as f64) * (intro * 1.5).min(1.0)).ceil() as usize
    };
    let spiral_y = y + logo.height().saturating_sub(logo.spiral.len() as u16) / 2;
    for (row, line) in logo.spiral.iter().enumerate() {
        for (col, c) in line.chars().enumerate() {
            if c == ' ' {
                continue;
            }
            let index = order
                .iter()
                .position(|&cell| cell == (row as u16, col as u16))
                .unwrap_or(0);
            if index < spiral_shown {
                canvas.text(
                    x + col as u16,
                    spiral_y + row as u16,
                    &c.to_string(),
                    s.logo,
                    1,
                );
            }
        }
    }
    // the word sweeps in from the left, a little behind the spiral
    let word_x = x + logo.spiral_width() + logo.gap;
    let word_width = logo.word[0].chars().count();
    let word_shown =
        ((word_width as f64) * ((intro - 0.15) / 0.85).clamp(0.0, 1.0)).ceil() as usize;
    let word_y = y + logo.height().saturating_sub(logo.word.len() as u16) / 2;
    for (row, line) in logo.word.iter().enumerate() {
        for (col, c) in line.chars().enumerate().take(word_shown) {
            if c == ' ' {
                continue;
            }
            let style = if Logo::is_letter(c) { s.logo } else { s.shadow };
            canvas.text(
                word_x + col as u16,
                word_y + row as u16,
                &c.to_string(),
                style,
                1,
            );
        }
    }
}

/// A heading: a word, and details after it (dimmed) or a spinner while its section loads.
fn draw_heading(
    canvas: &mut Canvas,
    x: u16,
    y: u16,
    width: u16,
    title: &str,
    detail: Vec<Span>,
    s: &Styles,
) {
    let end = canvas.text(x, y, title, s.heading, width);
    let room = width.saturating_sub(end - x + 2);
    let detail_width = spans_width(&detail).min(room);
    // details on the right
    canvas.spans(x + width - detail_width, y, &detail, detail_width);
}

/// A value at `now`: the shimmer while it loads, then typing itself in.
fn value_spans(value: &Value, width: u16, born: Instant, now: Instant, s: &Styles) -> Vec<Span> {
    match value {
        Value::Loading => shimmer(width.min(14), born, now, s),
        Value::Ready(spans, at) => {
            let full = spans_width(spans) as f64;
            let shown = (full * progress(*at, REVEAL, now)).ceil() as usize;
            cut(spans, shown)
        }
    }
}

/// A placeholder with a light running along it.
fn shimmer(width: u16, born: Instant, now: Instant, s: &Styles) -> Vec<Span> {
    let width = width as i64;
    let band = 4;
    let step = (now.saturating_duration_since(born).as_millis() / 45) as i64;
    let at = step % (width + band * 2) - band;
    (0..width)
        .map(|i| {
            let lit = i >= at && i < at + band;
            ("─".to_string(), if lit { s.value } else { s.track })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn draw_project(
    canvas: &mut Canvas,
    model: &Model,
    x: u16,
    y: u16,
    width: u16,
    layout: Layout,
    now: Instant,
    s: &Styles,
) -> u16 {
    let rows = project_rows(model.stats, s, layout.worktrees);
    let loading = rows.iter().any(|row| matches!(row.value, Value::Loading));
    let dir = match model.stats.work_dir.get() {
        Some((Ok(dir), _)) => dir,
        _ => model.dir.to_path_buf(),
    };
    let mut detail = vec![(
        vortex_stdx::path::fold_home_dir(&dir).display().to_string(),
        s.detail,
    )];
    if loading {
        detail.insert(0, (format!("{} ", spinner(model.born, now)), s.spinner));
    }
    draw_heading(canvas, x, y, width, "PROJECT", detail, s);
    for (i, row) in rows.iter().enumerate() {
        let row_y = y + 1 + i as u16;
        canvas.text(
            x,
            row_y,
            &row.label,
            row.label_style.unwrap_or(s.label),
            LABEL,
        );
        let value_width = width.saturating_sub(LABEL);
        let spans = value_spans(&row.value, value_width, model.born, now, s);
        canvas.spans(x + LABEL, row_y, &spans, value_width);
    }
    1 + rows.len() as u16
}

#[allow(clippy::too_many_arguments)]
fn draw_languages(
    canvas: &mut Canvas,
    model: &Model,
    x: u16,
    y: u16,
    width: u16,
    layout: Layout,
    now: Instant,
    s: &Styles,
) -> u16 {
    let counts: Counts = model.stats.languages.lock().unwrap().clone();
    let done = model.stats.languages_done.get();
    let total = counts.total();

    // the heading: the total once counted, the files so far while counting
    let detail = match &done {
        Some((Err(err), _)) => vec![(err.clone(), s.label)],
        Some((Ok(()), at)) => {
            let mut spans = vec![(format!("{} lines", thousands(total)), s.detail)];
            if counts.unreadable > 0 {
                spans.push((
                    format!(" · {} unreadable", thousands(counts.unreadable as u64)),
                    s.label,
                ));
            }
            let full = spans_width(&spans) as usize;
            let shown = (full as f64 * progress(*at, REVEAL, now)).ceil() as usize;
            let mut spans = cut(&spans, shown);
            // right-aligned as it types in
            let pad = full.saturating_sub(spans_width(&spans) as usize);
            spans.insert(0, (" ".repeat(pad), s.detail));
            spans
        }
        None => {
            let mut spans = vec![(format!("{} ", spinner(model.born, now)), s.spinner)];
            match counts.files {
                Some(files) => spans.push((
                    format!(
                        "{} of {} files",
                        thousands(counts.done as u64),
                        thousands(files as u64)
                    ),
                    s.detail,
                )),
                None => spans.push(("listing files".into(), s.detail)),
            }
            spans
        }
    };
    draw_heading(canvas, x, y, width, "LANGUAGES", detail, s);

    // the bar: the share of each language, over the part of the files read so far
    let shares = languages::shares(&counts.lines, LANGUAGES);
    let filled = (width as f64 * counts.progress()).round() as u16;
    let widths = languages::widths(&shares, filled);
    let mut col = x;
    for (i, (share, cells)) in shares.iter().zip(&widths).enumerate() {
        let style = s.language(i, &share.language);
        canvas.text(col, y + 1, &"━".repeat(*cells as usize), style, *cells);
        col += cells;
    }
    if col < x + width {
        let rest = x + width - col;
        let track = if done.is_some() {
            vec![("─".repeat(rest as usize), s.track)]
        } else {
            shimmer(rest, model.born, now, s)
        };
        canvas.spans(col, y + 1, &track, rest);
    }

    // the names under it, in a grid, as many rows as the layout has
    let columns = legend_columns(width);
    let gap = 3;
    let cell = (width + gap) / columns - gap;
    for (i, share) in shares.iter().enumerate() {
        let (row, column) = (i as u16 / columns, i as u16 % columns);
        if row >= layout.legend {
            break;
        }
        let name = if share.language == OTHER {
            OTHER.to_string()
        } else {
            display_name(&share.language)
        };
        let lines = thousands(share.lines);
        let cell_x = x + column * (cell + gap);
        canvas.text(cell_x, y + 2 + row, "■ ", s.language(i, &share.language), 2);
        let name_width = cell.saturating_sub(lines.width() as u16 + 3);
        canvas.text(cell_x + 2, y + 2 + row, &name, s.value, name_width);
        let lines_width = (lines.width() as u16).min(cell);
        canvas.text(
            cell_x + cell - lines_width,
            y + 2 + row,
            &lines,
            s.detail,
            lines_width,
        );
    }
    2 + legend_rows(width, layout.legend)
}

/// The width of the actions on one row.
fn actions_row_width(actions: &[Action]) -> u16 {
    actions
        .iter()
        .map(|action| 3 + 1 + action.short.width() as u16)
        .sum::<u16>()
        + 3 * actions.len().saturating_sub(1) as u16
}

fn draw_actions(canvas: &mut Canvas, actions: &[Action], y: u16, menu: bool, s: &Styles) {
    let area = canvas.area;
    let badge = |key: &str| format!(" {key} ");
    if menu {
        // a column: the key, the action, and the keymap's key for it on the right
        const LABEL: u16 = 16;
        let hint_width = actions
            .iter()
            .filter_map(|action| action.hint.as_ref())
            .map(|hint| hint.width() as u16)
            .max()
            .unwrap_or(0);
        let width = 3 + 3 + LABEL + hint_width;
        let x = center(area, width);
        for (i, action) in actions.iter().enumerate() {
            let row = y + i as u16;
            canvas.text(x, row, &badge(action.key), s.key, 3);
            canvas.text(x + 6, row, action.label, s.action, LABEL);
            if let Some(hint) = &action.hint {
                let w = hint.width() as u16;
                canvas.text(x + width - w, row, hint, s.hint, w);
            }
        }
    } else {
        // one row: the keys and the actions
        let mut spans: Vec<Span> = Vec::new();
        for (i, action) in actions.iter().enumerate() {
            if i > 0 {
                spans.push(("   ".into(), s.action));
            }
            spans.push((badge(action.key), s.key));
            spans.push((format!(" {}", action.short), s.action));
        }
        let width = spans_width(&spans).min(area.width);
        canvas.spans(center(area, width), y, &spans, width);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use vortex_vcs::summary::{
        Changes, Commit, Head, InProgress, Operation, Remote, Upstream, Worktree,
    };

    /// The text of a buffer, one line per row, without trailing spaces.
    pub fn text(buffer: &Surface) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            let mut line = String::new();
            for x in area.left()..area.right() {
                line.push_str(buffer[(x, y)].symbol.as_str());
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    pub fn hints() -> Hints {
        Hints {
            files: Some("space f".into()),
            tree: Some("space e".into()),
            history: Some("space l".into()),
            quit: Some(":q".into()),
        }
    }

    /// The stats of a repository, all loaded at `at`.
    pub fn loaded(at: Instant) -> Stats {
        let stats = Stats {
            git: true,
            ..Stats::default()
        };
        stats.work_dir.set_at(Ok(PathBuf::from("/src/vortex")), at);
        stats.head.set_at(
            Ok(Head {
                branch: Some("dashboard".into()),
                commit: Some(Commit {
                    short_id: "3768ea4".into(),
                    summary: "feat: remove microslop support and fix CI (#27)".into(),
                }),
            }),
            at,
        );
        stats.remote.set_at(
            Ok(Some(Remote {
                name: "origin".into(),
                url: "https://github.com/vzio/vortex".into(),
            })),
            at,
        );
        stats.upstream.set_at(
            Ok(Some(Upstream {
                name: "origin/main".into(),
                ahead: 3,
            })),
            at,
        );
        stats.changes.set_at(
            Ok(Changes {
                staged: 2,
                unstaged: 5,
                untracked: 1,
            }),
            at,
        );
        stats.in_progress.set_at(
            Ok(Some(InProgress {
                operation: Operation::Rebase,
                conflicted: 2,
            })),
            at,
        );
        stats.worktrees.set_at(
            Ok(vec![Worktree {
                path: PathBuf::from("/src/vortex-review"),
                branch: Some("review".into()),
                short_id: Some("8b83cad".into()),
            }]),
            at,
        );
        {
            let mut counts = stats.languages.lock().unwrap();
            counts.files = Some(1200);
            counts.done = 1200;
            counts.finished = true;
            for (language, lines) in [
                ("rust", 48210),
                ("markdown", 3377),
                ("toml", 1904),
                ("tsq", 12650),
                ("other", 2210),
            ] {
                counts.lines.insert(language.into(), lines);
            }
        }
        stats.languages_done.set_at(Ok(()), at);
        stats
    }

    pub fn draw(stats: &Stats, width: u16, height: u16, born: Instant, now: Instant) -> String {
        let theme = vortex_view::theme::Loader::new(&[]).default();
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Surface::empty(area);
        let hints = hints();
        let model = Model {
            stats,
            born,
            dir: Path::new("/src/vortex"),
            hints: &hints,
        };
        render(&model, &theme, area, &mut buffer, now);
        text(&buffer)
    }

    /// Compares the drawing with `snapshots/<name>.txt`, or writes it there with
    /// `UPDATE_SNAPSHOTS=1`.
    fn snapshot(name: &str, drawn: &str) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/dashboard/snapshots")
            .join(format!("{name}.txt"));
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, drawn).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            expected == drawn,
            "{name} differs from {}, run with UPDATE_SNAPSHOTS=1 to update it:\n{drawn}",
            path.display()
        );
    }

    /// Long after everything arrived: nothing moves.
    fn settled(born: Instant) -> Instant {
        born + Duration::from_secs(5)
    }

    /// The intro done, the stats still loading (the shimmer frozen at one frame).
    fn loading_time(born: Instant) -> Instant {
        born + INTRO + Duration::from_millis(130)
    }

    #[test]
    fn git_repository_loaded() {
        let born = Instant::now();
        let stats = loaded(born);
        snapshot("git-160", &draw(&stats, 160, 44, born, settled(born)));
        snapshot("git-100", &draw(&stats, 100, 44, born, settled(born)));
    }

    #[test]
    fn git_repository_loading() {
        let born = Instant::now();
        let stats = Stats {
            git: true,
            ..Stats::default()
        };
        snapshot(
            "loading-160",
            &draw(&stats, 160, 44, born, loading_time(born)),
        );
        // the language bar filling in: half of the files read
        {
            let mut counts = stats.languages.lock().unwrap();
            counts.files = Some(1000);
            counts.done = 500;
            counts.lines.insert("rust".into(), 20000);
            counts.lines.insert("markdown".into(), 4000);
        }
        stats.head.set_at(
            Ok(Head {
                branch: None,
                commit: Some(Commit {
                    short_id: "8b83cad".into(),
                    summary: "feat(blame): .git-blame-ignore-revs".into(),
                }),
            }),
            born,
        );
        snapshot(
            "loading-100",
            &draw(&stats, 100, 44, born, loading_time(born)),
        );
    }

    #[test]
    fn folder_outside_git() {
        let born = Instant::now();
        let stats = Stats::default();
        snapshot("folder-160", &draw(&stats, 160, 44, born, settled(born)));
        snapshot("folder-100", &draw(&stats, 100, 44, born, settled(born)));
    }

    #[test]
    fn narrow_and_short_windows() {
        let born = Instant::now();
        let stats = loaded(born);
        snapshot("narrow-60x24", &draw(&stats, 60, 24, born, settled(born)));
        snapshot("narrow-40x12", &draw(&stats, 40, 12, born, settled(born)));
    }

    #[test]
    fn a_repository_that_cant_be_read_shows_no_stats() {
        let born = Instant::now();
        let stats = Stats {
            git: true,
            ..Stats::default()
        };
        stats.gone.store(true, Ordering::Relaxed);
        let drawn = draw(&stats, 100, 44, born, settled(born));
        assert!(!drawn.contains("PROJECT"), "{drawn}");
        assert!(!drawn.contains("Git history"), "{drawn}");
        assert!(drawn.contains("File tree"), "{drawn}");
    }

    #[test]
    fn calm_errors_in_place() {
        let born = Instant::now();
        let stats = loaded(born);
        stats.upstream.set_at(Ok(None), born);
        stats.remote.set_at(Ok(None), born);
        stats.changes.set_at(Err("couldn't read".into()), born);
        stats.head.set_at(
            Ok(Head {
                branch: None,
                commit: Some(Commit {
                    short_id: "8b83cad".into(),
                    summary: "x".into(),
                }),
            }),
            born,
        );
        let drawn = draw(&stats, 100, 44, born, settled(born));
        for text in [
            "detached at 8b83cad",
            "none while detached",
            "no remote",
            "couldn't read",
        ] {
            assert!(drawn.contains(text), "{text}:\n{drawn}");
        }
        stats.head.set_at(
            Ok(Head {
                branch: Some("main".into()),
                commit: None,
            }),
            born,
        );
        let drawn = draw(&stats, 100, 44, born, settled(born));
        assert!(
            drawn.contains("no upstream") && drawn.contains("no commits yet"),
            "{drawn}"
        );
    }

    #[test]
    fn the_logo_draws_itself_in() {
        let born = Instant::now();
        let stats = Stats::default();
        let first = draw(&stats, 100, 30, born, born);
        let middle = draw(&stats, 100, 30, born, born + INTRO / 2);
        let done = draw(&stats, 100, 30, born, born + INTRO);
        let cells = |s: &str| s.chars().filter(|c| "█╗║╔═╝╚╭╮╰╯│─╶".contains(*c)).count();
        assert!(cells(&first) < cells(&middle) && cells(&middle) < cells(&done));
        assert_eq!(done, draw(&stats, 100, 30, born, born + INTRO * 4));
    }
}
