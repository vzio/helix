//! Drawing the dashboard: a game main menu, centered, with generous spacing that collapses in
//! small windows, over a vortex.
//!
//! From top to bottom: the title and the credit line, the project and its languages (side by
//! side in wide windows, stacked otherwise; only in git repositories), and the actions. Each of
//! the three sits on a panel of the theme's background, cut out of the vortex ([`vortex`]) with a
//! margin around its text. When the window is too short, the layout gives up room step by step
//! (`Layout::all`): smaller gaps, long values back on one row, fewer rows of languages, a
//! smaller title, fewer worktrees, the actions on one row, the smallest title, and at last the
//! stats.

use std::cell::RefCell;
use std::path::Path;
use std::time::Instant;

use vortex_core::unicode::width::{UnicodeWidthChar, UnicodeWidthStr};
use vortex_view::graphics::{Modifier, Rect, Style};
use vortex_view::Theme;

use tui::buffer::Buffer as Surface;

use vortex_view::graphics::Color;

use super::languages::{self, display_name, thousands, Counts, OTHER};
use super::stats::{Slot, Stats};
use super::timeline::Timeline;
use super::title::{self, Title};
use super::vortex;
use super::wrap::{self, Breaks, Span};
use crate::ui::glitch;

/// What the dashboard draws, apart from the time.
pub struct Model<'a> {
    pub stats: &'a Stats,
    /// When the dashboard opened and how its background moves.
    pub timeline: Timeline,
    /// The folder it opened in.
    pub dir: &'a Path,
    pub hints: &'a Hints,
    /// Whether the title and its credit line are shown (`dashboard-title`).
    pub title: bool,
    /// Only the vortex and the logo, none of the other blocks (`:vortex`).
    pub vortex_only: bool,
    /// The glitches of the title and of the background, kept from frame to frame; `None`
    /// draws none, whatever the theme says (the tests' frames are functions of the time).
    pub glitches: Option<&'a RefCell<Glitches>>,
}

/// The glitches of the dashboard, at the rates of the theme's `[effects]`.
#[derive(Debug, Default)]
pub struct Glitches {
    pub logo: glitch::Slot,
    pub background: glitch::Slot,
}

/// The keys of the keymap that do what the actions do, so the dashboard teaches them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    pub files: Option<String>,
    pub tree: Option<String>,
    pub history: Option<String>,
    /// `:worktrees`, which has no key by default (`:wt`).
    pub worktrees: Option<String>,
    /// `:watch`, which has no key by default.
    pub watch: Option<String>,
    pub quit: Option<String>,
}

/// The styles of the dashboard, from the theme: the `ui.dashboard.*` scopes, else common scopes
/// every theme has.
pub struct Styles {
    logo: Style,
    shadow: Style,
    credit: Style,
    heading: Style,
    pub(crate) detail: Style,
    pub(crate) label: Style,
    pub(crate) value: Style,
    pub(crate) good: Style,
    pub(crate) warning: Style,
    key: Style,
    action: Style,
    hint: Style,
    spinner: Style,
    track: Style,
    /// The colors of the languages in the bar, in order.
    palette: Vec<Style>,
    other: Style,
    /// What the panels under the text are filled with.
    panel: Style,
    vortex: vortex::Colors,
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
        let palette_colors: Vec<Color> = PALETTE
            .iter()
            .filter_map(|scope| theme.try_get(scope).and_then(|style| style.fg))
            .collect();
        let palette = palette_colors
            .iter()
            .map(|&fg| Style::default().fg(fg))
            .collect();
        let background = theme.get("ui.background");
        let panel = theme
            .try_get_exact("ui.dashboard.panel")
            .unwrap_or(background);
        // the vortex, in two tones: the color of the title at the eye (or
        // `ui.dashboard.vortex`), fading into the color of the text at the rim; or the stops
        // `ui.dashboard.vortex.1`, `.2`, ... from the eye out
        let numbered: Vec<Color> = (1..)
            .map_while(|n| theme.try_get_exact(&format!("ui.dashboard.vortex.{n}")))
            .filter_map(|style| style.fg)
            .collect();
        let gradient = if !numbered.is_empty() {
            numbered
        } else {
            let eye = theme
                .try_get_exact("ui.dashboard.vortex")
                .and_then(|style| style.fg)
                .or(logo.fg)
                .or(palette_colors.first().copied());
            eye.into_iter().chain(text.fg).collect()
        };
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
            panel,
            vortex: vortex::Colors {
                gradient,
                background: background.bg,
            },
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
    /// Where the value may break onto a second row.
    breaks: Breaks,
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
            breaks: Breaks::Words,
        }
    }

    fn url(self) -> Self {
        Self {
            breaks: Breaks::Url,
            ..self
        }
    }

    /// The rows of the value in `width` columns: two at most when `wrap`, else one.
    fn lines(&self, width: u16, wrap: bool) -> Vec<Vec<Span>> {
        match &self.value {
            Value::Loading => vec![Vec::new()],
            Value::Ready(spans, _) => {
                wrap::wrap(spans, width as usize, if wrap { 2 } else { 1 }, self.breaks)
            }
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
    rows.push(
        Row::new(
            "remote",
            value(&stats.remote, s, |remote| match remote {
                Some(remote) => vec![
                    (remote.name.clone(), s.value),
                    ("  ".into(), s.value),
                    (remote.url.clone(), s.detail),
                ],
                None => vec![("no remote".into(), s.label)],
            }),
        )
        .url(),
    );
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
            breaks: Breaks::Words,
        });
    }
    if let Some((Ok(worktrees), at)) = stats.worktrees.get() {
        let shown = if worktrees.len() > worktrees_max {
            worktrees_max.saturating_sub(1)
        } else {
            worktrees.len()
        };
        let now = std::time::SystemTime::now();
        for (i, dated) in worktrees.iter().take(shown).enumerate() {
            let worktree = &dated.worktree;
            let path = vortex_stdx::path::fold_home_dir(&worktree.path);
            let mut spans = vec![
                (path.display().to_string(), s.value),
                ("  ".into(), s.value),
            ];
            if worktree.missing {
                spans.push(("missing".into(), s.label));
            } else {
                match (&worktree.branch, &worktree.short_id) {
                    (Some(branch), _) => spans.push((branch.clone(), s.detail)),
                    (None, Some(id)) => spans.push((format!("detached at {id}"), s.label)),
                    (None, None) => {}
                }
                if let Some(modified) = dated.modified {
                    spans.push(("  ".into(), s.value));
                    spans.push((vortex_stdx::time::format_ago(modified, now), s.label));
                }
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

/// How many rows the project section takes in `width` columns, its heading included.
fn project_height(rows: &[Row], width: u16, wrap: bool) -> u16 {
    let value_width = width.saturating_sub(LABEL);
    1 + rows
        .iter()
        .map(|row| row.lines(value_width, wrap).len() as u16)
        .sum::<u16>()
}

/// A layout: how much room each part takes. [`Layout::all`] lists them from the roomiest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    title: Title,
    /// The empty rows between the parts.
    gap: u16,
    /// Long values on two rows rather than one.
    wrap: bool,
    /// The rows of language names under the bar, at most.
    legend: u16,
    worktrees: usize,
    /// The actions as a column (a menu) rather than one row.
    menu: bool,
    stats: bool,
}

impl Layout {
    /// The layouts from the roomiest, whose title is `large`.
    fn all(large: Title) -> Vec<Layout> {
        let roomy = Layout {
            title: large,
            gap: 2,
            wrap: true,
            legend: 3,
            worktrees: WORKTREES,
            menu: true,
            stats: true,
        };
        let steps: [fn(Layout) -> Layout; 11] = [
            |l| Layout { gap: 1, ..l },
            // long values give up their second row before anything is dropped
            |l| Layout { wrap: false, ..l },
            |l| Layout { legend: 2, ..l },
            |l| Layout { legend: 1, ..l },
            |l| Layout {
                title: title::MEDIUM,
                ..l
            },
            |l| Layout { worktrees: 1, ..l },
            |l| Layout { menu: false, ..l },
            |l| Layout {
                title: title::SMALL,
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
        actions.push(Action {
            key: "w",
            short: "watch",
            label: "Watch mode",
            hint: model.hints.watch.clone(),
        });
    }
    if model.stats.shows_worktrees() {
        actions.push(Action {
            key: "t",
            short: "worktrees",
            label: "Worktrees",
            hint: model.hints.worktrees.clone(),
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

/// The margin between a panel's text and the vortex.
const MARGIN_X: u16 = 2;
const MARGIN_Y: u16 = 1;

/// Draws the dashboard of `model` in `area` as it is at `now`.
pub fn render(model: &Model, theme: &Theme, area: Rect, surface: &mut Surface, now: Instant) {
    let styles = Styles::new(theme);
    let s = &styles;
    let effects = theme.effects();
    surface.clear_with(area, theme.get("ui.background"));
    let look = vortex::Look {
        colors: &s.vortex,
        charset: effects.dashboard_charset,
        reach: model.timeline.reach(model.stats, now),
    };
    let mut glitches = model.glitches.map(|glitches| glitches.borrow_mut());
    let mut background_glitch = glitches
        .as_mut()
        .and_then(|glitches| glitches.background.at(effects.glitch_background));
    // the places of the vortex's cells, only for a glitch over them
    let mut drawn = background_glitch.as_ref().map(|_| Vec::new());
    if model.vortex_only {
        // the vortex and the logo (unless `dashboard-title = false`) on its panel, centered: the
        // largest title that fits with its margin, no credit line, no stats, no actions
        let logo = model
            .title
            .then(|| {
                [
                    title::large(effects.dashboard_logo),
                    title::MEDIUM,
                    title::SMALL,
                ]
                .into_iter()
                .find(|logo| {
                    logo.width() + MARGIN_X * 2 <= area.width
                        && logo.height() + MARGIN_Y * 2 <= area.height
                })
            })
            .flatten();
        let cells = logo
            .map(|logo| {
                logo.cells_in(
                    model.timeline.title(model.stats, now),
                    effects.dashboard_charset,
                )
            })
            .unwrap_or_default();
        let (title_x, title_y) = logo
            .map(|logo| {
                let y = area.y + area.height.saturating_sub(logo.height()) / 2;
                (center(area, logo.width()), y)
            })
            .unwrap_or_default();
        // the panel follows the title as it opens, like the dashboard's
        let panel = logo.map(|logo| {
            let left = cells.iter().map(|cell| cell.x).min().unwrap_or(0);
            let right = cells.iter().map(|cell| cell.x + 1).max().unwrap_or(0);
            let part = Rect::new(
                title_x + left,
                title_y,
                right.saturating_sub(left),
                logo.height(),
            );
            margin(part, area)
        });
        let spin = model.timeline.spin(model.stats, now);
        if let Some(spin) = spin {
            let holes: Vec<Rect> = panel.into_iter().collect();
            vortex::draw(surface, area, spin, &holes, &look, drawn.as_mut());
            if let Some(panel) = panel.filter(|_| vortex::fits(area)) {
                surface.clear_with(panel, s.panel);
            }
        }
        if let Some(glitch) = background_glitch {
            glitch.draw(now, surface, drawn.as_deref().unwrap_or(&[]));
        }
        if !cells.is_empty() {
            let mut canvas = Canvas { area, surface };
            draw_title(&mut canvas, &cells, title_x, title_y, s);
            let logo_glitch = glitches
                .as_mut()
                .and_then(|glitches| glitches.logo.at(effects.glitch_logo));
            if let Some(glitch) = logo_glitch {
                glitch_title(canvas.surface, glitch, &cells, title_x, title_y, area, now);
            }
        }
        return;
    }
    let git = model.stats.shows_git();
    let wide = area.width >= WIDE;
    let panel = if wide {
        PANEL
    } else {
        STACKED.min(area.width.saturating_sub(4)).max(1)
    };
    let actions = actions(model);
    // the rows of the title and its credit line, with the gap under them; none without the
    // title (`dashboard-title = false`)
    let head = |layout: &Layout| -> u16 {
        if !model.title {
            return 0;
        }
        let rows = layout.title.height();
        rows + if rows > 1 { 2 } else { 1 } + layout.gap + 1
    };
    let title_width = |layout: &Layout| if model.title { layout.title.width() } else { 0 };
    let languages_height = |layout: &Layout| 2 + legend_rows(panel, layout.legend);

    let height = |layout: &Layout| -> u16 {
        let menu = if layout.menu {
            actions.len() as u16
        } else {
            row_lines(&actions, area.width).len() as u16
        };
        let mut total = head(layout) + menu;
        if git && layout.stats {
            let rows = project_rows(model.stats, s, layout.worktrees);
            let project = project_height(&rows, panel, layout.wrap);
            let languages = languages_height(layout);
            let stats = if wide {
                project.max(languages)
            } else {
                project + layout.gap.max(1) + languages
            };
            total += stats + layout.gap + 1;
        }
        total
    };
    let layouts = Layout::all(title::large(effects.dashboard_logo));
    // the actions on one row, or two when one is too narrow for them
    let row_fits = row_lines(&actions, area.width).len() <= 2;
    let fits = |layout: &&Layout| {
        height(layout) <= area.height
            && title_width(layout) <= area.width
            && (layout.menu || row_fits)
    };
    let layout = layouts
        .iter()
        .find(fits)
        .or_else(|| layouts.iter().rfind(|l| title_width(l) <= area.width))
        .copied()
        .unwrap_or(*layouts.last().unwrap());

    // where each part goes
    let total = height(&layout);
    let mut y = area.y + area.height.saturating_sub(total) / 2;
    let title_y = y;
    let credit_y = y + layout.title.height() + u16::from(layout.title.height() > 1);
    y += head(&layout);
    let mut stats_part = None;
    if git && layout.stats {
        let rows = project_rows(model.stats, s, layout.worktrees);
        let project = project_height(&rows, panel, layout.wrap);
        let (width, height) = if wide {
            (
                PANEL * 2 + PANEL_GAP,
                project.max(languages_height(&layout)),
            )
        } else {
            (
                panel,
                project + layout.gap.max(1) + languages_height(&layout),
            )
        };
        stats_part = Some(Rect::new(center(area, width), y, width, height));
        y += height + layout.gap + 1;
    }
    let actions_part = actions_rect(area, &actions, y, layout.menu);

    // the title as it is now: its cells, and how wide they spread
    let opened = model.timeline.title(model.stats, now);
    let cells = if model.title {
        layout.title.cells_in(opened, effects.dashboard_charset)
    } else {
        Vec::new()
    };
    let title_x = center(area, title_width(&layout));
    let credit = title::CREDIT;
    let credit_width = credit.width() as u16;

    // the vortex, and the panels cut out of it
    let spin = model.timeline.spin(model.stats, now);
    if let Some(spin) = spin.filter(|_| vortex::fits(area)) {
        let left = cells.iter().map(|cell| cell.x).min().unwrap_or(0);
        let right = cells.iter().map(|cell| cell.x + 1).max().unwrap_or(0);
        let credit_x = center(area, credit_width);
        let title_left = (title_x + left).min(credit_x);
        let title_right = (title_x + right).max(credit_x + credit_width);
        let title_part = Rect::new(
            title_left,
            title_y,
            title_right - title_left,
            credit_y + 1 - title_y,
        );
        let title_part = model.title.then_some(title_part);
        let panels: Vec<Rect> = [title_part, stats_part, Some(actions_part)]
            .into_iter()
            .flatten()
            .map(|part| margin(part, area))
            .collect();
        vortex::draw(surface, area, spin, &panels, &look, drawn.as_mut());
        if let (Some(glitch), Some(drawn)) = (background_glitch.take(), &drawn) {
            glitch.draw(now, surface, drawn);
        }
        for &panel in &panels {
            surface.clear_with(panel, s.panel);
        }
    }
    // with no vortex drawn (a small window, `off`), no glitch over it either
    if let Some(glitch) = background_glitch {
        glitch.draw(now, surface, &[]);
    }

    let mut canvas = Canvas { area, surface };
    if model.title {
        draw_title(&mut canvas, &cells, title_x, title_y, s);
        let logo_glitch = glitches
            .as_mut()
            .and_then(|glitches| glitches.logo.at(effects.glitch_logo));
        if let Some(glitch) = logo_glitch {
            glitch_title(canvas.surface, glitch, &cells, title_x, title_y, area, now);
        }
        canvas.text(
            center(area, credit_width),
            credit_y,
            credit,
            s.credit,
            area.width,
        );
    }

    if let Some(part) = stats_part {
        let y = part.y;
        if wide {
            let left = part.x;
            draw_project(&mut canvas, model, left, y, PANEL, layout, now, s);
            draw_languages(
                &mut canvas,
                model,
                left + PANEL + PANEL_GAP,
                y,
                PANEL,
                layout,
                now,
                s,
            );
        } else {
            let left = part.x;
            let project = draw_project(&mut canvas, model, left, y, panel, layout, now, s);
            let y = y + project + layout.gap.max(1);
            draw_languages(&mut canvas, model, left, y, panel, layout, now, s);
        }
    }

    draw_actions(&mut canvas, &actions, actions_part, layout.menu, s);
}

/// `part` with the margin of its panel, within `area`.
fn margin(part: Rect, area: Rect) -> Rect {
    let left = part.x.saturating_sub(MARGIN_X).max(area.x);
    let top = part.y.saturating_sub(MARGIN_Y).max(area.y);
    let right = (part.x + part.width + MARGIN_X).min(area.right());
    let bottom = (part.y + part.height + MARGIN_Y).min(area.bottom());
    Rect::new(
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    )
}

fn center(area: Rect, width: u16) -> u16 {
    area.x + area.width.saturating_sub(width) / 2
}

/// The title at `x`, `y`: the letters in the title's style, their shadow in its own.
fn draw_title(canvas: &mut Canvas, cells: &[title::Cell], x: u16, y: u16, s: &Styles) {
    let mut buf = [0; 4];
    for cell in cells {
        let style = if cell.letter { s.logo } else { s.shadow };
        canvas.text(
            x + cell.x,
            y + cell.y,
            cell.c.encode_utf8(&mut buf),
            style,
            1,
        );
    }
}

/// The glitch of the title, over its letters (not their shadow) that are on screen.
fn glitch_title(
    surface: &mut Surface,
    glitch: &mut glitch::Glitch,
    cells: &[title::Cell],
    x: u16,
    y: u16,
    area: Rect,
    now: Instant,
) {
    let places: Vec<(u16, u16)> = cells
        .iter()
        .filter(|cell| cell.letter)
        .map(|cell| (x + cell.x, y + cell.y))
        .filter(|&(x, y)| x < area.right() && y < area.bottom())
        .collect();
    glitch.draw(now, surface, &places);
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

/// A placeholder with a light running along it.
pub fn shimmer(width: u16, born: Instant, now: Instant, s: &Styles) -> Vec<Span> {
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

/// Where a row of the project section is in the cascade of values typing in: the top one first.
fn place(row: usize) -> f64 {
    row as f64 / 6.0
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
    let born = model.timeline.born;
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
        detail.insert(0, (format!("{} ", spinner(born, now)), s.spinner));
    }
    draw_heading(canvas, x, y, width, "PROJECT", detail, s);
    let value_width = width.saturating_sub(LABEL);
    let mut row_y = y + 1;
    for (i, row) in rows.iter().enumerate() {
        canvas.text(
            x,
            row_y,
            &row.label,
            row.label_style.unwrap_or(s.label),
            LABEL,
        );
        let lines = row.lines(value_width, layout.wrap);
        // the shimmer until the value is in and its turn in the cascade comes, then typing in
        let typed = match &row.value {
            Value::Ready(_, at) => model.timeline.typed(*at, place(i), now),
            Value::Loading => 0.0,
        };
        if typed == 0.0 {
            let shimmer = shimmer(value_width.min(14), born, now, s);
            canvas.spans(x + LABEL, row_y, &shimmer, value_width);
        } else {
            let shown = (wrap::columns(&lines) as f64 * typed).ceil() as usize;
            for (line, spans) in wrap::typed(&lines, shown).iter().enumerate() {
                canvas.spans(x + LABEL, row_y + line as u16, spans, value_width);
            }
        }
        row_y += lines.len() as u16;
    }
    row_y - y
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
    let born = model.timeline.born;
    let counts: Counts = model.stats.languages.lock().unwrap().clone();
    let done = model.stats.languages_done.get();
    let total = counts.total();
    // the total types in last in the cascade
    let typed = match &done {
        Some((_, at)) => model.timeline.typed(*at, 1.0, now),
        None => 0.0,
    };

    // the heading: the total once counted, the files so far while counting
    let detail = match &done {
        Some((Err(err), _)) if typed > 0.0 => vec![(err.clone(), s.label)],
        Some((Ok(()), _)) if typed > 0.0 => {
            let mut spans = vec![(format!("{} lines", thousands(total)), s.detail)];
            if counts.unreadable > 0 {
                spans.push((
                    format!(" · {} unreadable", thousands(counts.unreadable as u64)),
                    s.label,
                ));
            }
            let full = spans_width(&spans) as usize;
            let shown = (full as f64 * typed).ceil() as usize;
            let mut spans = wrap::typed(&[spans], shown).remove(0);
            // right-aligned as it types in
            let pad = full.saturating_sub(spans_width(&spans) as usize);
            spans.insert(0, (" ".repeat(pad), s.detail));
            spans
        }
        _ => {
            let mut spans = vec![(format!("{} ", spinner(born, now)), s.spinner)];
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

    // the bar: the share of each language, over the part of the files read so far, filling in
    // over the loading time at least
    let shares = languages::shares(&counts.lines, LANGUAGES);
    let filled = model.timeline.filled(counts.progress(), now);
    let filled = (width as f64 * filled).round() as u16;
    let widths = languages::widths(&shares, filled);
    let mut col = x;
    for (i, (share, cells)) in shares.iter().zip(&widths).enumerate() {
        let style = s.language(i, &share.language);
        canvas.text(col, y + 1, &"━".repeat(*cells as usize), style, *cells);
        col += cells;
    }
    if col < x + width {
        let rest = x + width - col;
        let track = if done.is_some() && filled == width {
            vec![("─".repeat(rest as usize), s.track)]
        } else {
            shimmer(rest, born, now, s)
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

/// The width of the actions as a menu, and of its labels.
fn menu_width(actions: &[Action]) -> (u16, u16) {
    const LABEL: u16 = 16;
    let hint_width = actions
        .iter()
        .filter_map(|action| action.hint.as_ref())
        .map(|hint| hint.width() as u16)
        .max()
        .unwrap_or(0);
    (3 + 3 + LABEL + hint_width, LABEL)
}

/// The actions on rows of at most `width` columns, as ranges of them: as many on each row as
/// fit, the same number on each when they take two (the last row may have one less).
fn row_lines(actions: &[Action], width: u16) -> Vec<std::ops::Range<usize>> {
    if actions_row_width(actions) <= width || actions.is_empty() {
        return vec![0..actions.len()];
    }
    for rows in 2..=actions.len() {
        let per_row = actions.len().div_ceil(rows);
        let lines: Vec<_> = (0..actions.len())
            .step_by(per_row)
            .map(|start| start..(start + per_row).min(actions.len()))
            .collect();
        if lines
            .iter()
            .all(|line| actions_row_width(&actions[line.clone()]) <= width)
        {
            return lines;
        }
    }
    (0..actions.len()).map(|i| i..i + 1).collect()
}

/// Where the actions go, from row `y`.
fn actions_rect(area: Rect, actions: &[Action], y: u16, menu: bool) -> Rect {
    if menu {
        let (width, _) = menu_width(actions);
        Rect::new(center(area, width), y, width, actions.len() as u16)
    } else {
        let lines = row_lines(actions, area.width);
        let width = lines
            .iter()
            .map(|line| actions_row_width(&actions[line.clone()]))
            .max()
            .unwrap_or(0)
            .min(area.width);
        Rect::new(center(area, width), y, width, lines.len() as u16)
    }
}

fn draw_actions(canvas: &mut Canvas, actions: &[Action], part: Rect, menu: bool, s: &Styles) {
    let badge = |key: &str| format!(" {key} ");
    let (x, y) = (part.x, part.y);
    if menu {
        // a column: the key, the action, and the keymap's key for it on the right
        let (width, label) = menu_width(actions);
        for (i, action) in actions.iter().enumerate() {
            let row = y + i as u16;
            canvas.text(x, row, &badge(action.key), s.key, 3);
            canvas.text(x + 6, row, action.label, s.action, label);
            if let Some(hint) = &action.hint {
                let w = hint.width() as u16;
                canvas.text(x + width - w, row, hint, s.hint, w);
            }
        }
    } else {
        // a row (or two): the keys and the actions, each row centered
        for (row, line) in row_lines(actions, canvas.area.width)
            .into_iter()
            .enumerate()
        {
            let line = &actions[line];
            let mut spans: Vec<Span> = Vec::new();
            for (i, action) in line.iter().enumerate() {
                if i > 0 {
                    spans.push(("   ".into(), s.action));
                }
                spans.push((badge(action.key), s.key));
                spans.push((format!(" {}", action.short), s.action));
            }
            let width = actions_row_width(line).min(part.width);
            let line_x = x + (part.width - width) / 2;
            canvas.spans(line_x, y + row as u16, &spans, width);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use vortex_vcs::summary::{
        Changes, Commit, Dated, Head, InProgress, Operation, Remote, Upstream, Worktree,
    };
    use vortex_view::editor::DashboardBackground;

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
            worktrees: Some(":wt".into()),
            watch: Some(":watch".into()),
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
            Ok(vec![Dated {
                worktree: Worktree {
                    path: PathBuf::from("/src/vortex-review"),
                    branch: Some("review".into()),
                    short_id: Some("8b83cad".into()),
                    git_dir: PathBuf::from("/src/vortex/.git/worktrees/vortex-review"),
                    missing: false,
                },
                modified: None,
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
        draw_with(
            stats,
            width,
            height,
            Timeline::new(born, DashboardBackground::Off),
            now,
        )
    }

    pub fn draw_with(
        stats: &Stats,
        width: u16,
        height: u16,
        timeline: Timeline,
        now: Instant,
    ) -> String {
        text(&surface(stats, width, height, timeline, now))
    }

    pub fn surface(
        stats: &Stats,
        width: u16,
        height: u16,
        timeline: Timeline,
        now: Instant,
    ) -> Surface {
        surface_titled(stats, width, height, timeline, now, true)
    }

    /// [`surface`], with the title shown or not.
    pub fn surface_titled(
        stats: &Stats,
        width: u16,
        height: u16,
        timeline: Timeline,
        now: Instant,
        title: bool,
    ) -> Surface {
        let theme = vortex_view::theme::Loader::new(&[]).default();
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Surface::empty(area);
        let hints = hints();
        let model = Model {
            stats,
            timeline,
            dir: Path::new("/src/vortex"),
            hints: &hints,
            title,
            vortex_only: false,
            glitches: None,
        };
        render(&model, &theme, area, &mut buffer, now);
        buffer
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
        born + Duration::from_secs(8)
    }

    /// The stats still loading (the shimmer frozen at one frame), the title still `VX`.
    fn loading_time(born: Instant) -> Instant {
        born + Duration::from_millis(830)
    }

    fn timeline(born: Instant, background: DashboardBackground) -> Timeline {
        Timeline::new(born, background)
    }

    const SIZES: [(u16, u16); 4] = [(160, 50), (100, 44), (60, 24), (120, 40)];
    const MODES: [DashboardBackground; 3] = [
        DashboardBackground::On,
        DashboardBackground::Settle,
        DashboardBackground::Static,
    ];

    fn is_blank(surface: &Surface, x: u16, y: u16) -> bool {
        surface[(x, y)].symbol.trim().is_empty()
    }

    /// The cells the vortex drew: those that differ from the same frame without a vortex.
    fn vortex_cells(with: &Surface, without: &Surface) -> Vec<(u16, u16)> {
        let area = with.area;
        let mut cells = Vec::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if with[(x, y)].symbol != without[(x, y)].symbol {
                    cells.push((x, y));
                }
            }
        }
        cells
    }

    /// [`surface_titled`] with a theme, and the glitches of a run when given.
    fn surface_themed(
        theme: &Theme,
        stats: &Stats,
        (width, height): (u16, u16),
        timeline: Timeline,
        now: Instant,
        glitches: Option<&RefCell<Glitches>>,
    ) -> Surface {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Surface::empty(area);
        let hints = hints();
        let model = Model {
            stats,
            timeline,
            dir: Path::new("/src/vortex"),
            hints: &hints,
            title: true,
            vortex_only: false,
            glitches,
        };
        render(&model, theme, area, &mut buffer, now);
        buffer
    }

    fn matrix() -> Theme {
        vortex_view::theme::Loader::new(&[]).load("matrix").unwrap()
    }

    /// A theme inheriting the default one with the given `[effects]` lines, in a temp dir.
    fn themed(effects: &str) -> Theme {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("themes")).unwrap();
        std::fs::write(
            dir.path().join("themes/glitchy.toml"),
            format!("inherits = \"vortex\"\n[effects]\n{effects}\n"),
        )
        .unwrap();
        vortex_view::theme::Loader::new(&[dir.path().to_path_buf()])
            .load("glitchy")
            .unwrap()
    }

    #[test]
    fn the_runes_logo_settled() {
        let born = Instant::now();
        let stats = loaded(born);
        let drawn = draw(&stats, 160, 50, born, settled(born));
        assert!(drawn.contains("▀█      █▀     ▄██▄"), "{drawn}");
        snapshot("runes-160", &drawn);
    }

    #[test]
    fn the_matrix_theme_draws_the_classic_logo_and_an_ascii_vortex() {
        let born = Instant::now();
        let stats = loaded(born);
        let theme = matrix();
        let effects = theme.effects();
        assert_eq!(
            effects.dashboard_logo,
            vortex_view::theme::DashboardLogo::Classic
        );
        assert_eq!(
            effects.dashboard_charset,
            vortex_view::theme::DashboardCharset::Ascii
        );
        let now = settled(born);
        let off = surface_themed(
            &theme,
            &stats,
            (160, 50),
            timeline(born, DashboardBackground::Off),
            now,
            None,
        );
        let off_text = text(&off);
        assert!(off_text.contains("██╗   ██╗ ██████╗ ██████╗ ████████╗███████╗██╗  ██╗"));
        snapshot("classic-160", &off_text);

        let still = surface_themed(
            &theme,
            &stats,
            (160, 50),
            timeline(born, DashboardBackground::Static),
            now,
            None,
        );
        let still_text = text(&still);
        snapshot("ascii-static-160", &still_text);
        assert!(
            !still_text.chars().any(|c| "░▒▓".contains(c)),
            "{still_text}"
        );
        // `·` is also the panels' separator (`2 staged · 5 unstaged`): as many as without a vortex
        let dots = |text: &str| text.chars().filter(|&c| c == '·').count();
        assert_eq!(dots(&still_text), dots(&off_text));
        // the same blocks as without the vortex, in the same places: the title's
        let blocks = |surface: &Surface| -> Vec<(u16, u16)> {
            vortex_cells(surface, &Surface::empty(surface.area))
                .into_iter()
                .filter(|&(x, y)| surface[(x, y)].symbol.as_str() == "█")
                .collect()
        };
        assert!(!blocks(&off).is_empty());
        assert_eq!(blocks(&still), blocks(&off));
        // everything the vortex drew is in the ascii ramp
        let changed = vortex_cells(&still, &off);
        assert!(changed.len() > 500);
        for (x, y) in changed {
            let c = still[(x, y)].symbol.chars().next().unwrap();
            assert!(
                super::super::charset::ASCII.contains(&c),
                "{c:?} at {x},{y}"
            );
        }
    }

    #[test]
    fn the_logo_glitches_only_over_the_title() {
        let born = Instant::now();
        let stats = loaded(born);
        let theme = themed("glitch-logo = 10.0");
        assert_eq!(theme.effects().glitch_logo, 10.0);
        let still = timeline(born, DashboardBackground::Static);
        let glitches = RefCell::new(Glitches::default());
        let mut differing: Vec<(u16, u16)> = Vec::new();
        for k in 0..40 {
            let now = settled(born) + Duration::from_millis(k * 50);
            let plain = surface_themed(&theme, &stats, (160, 50), still, now, None);
            let glitched = surface_themed(&theme, &stats, (160, 50), still, now, Some(&glitches));
            for (x, y) in vortex_cells(&glitched, &plain) {
                let was = plain[(x, y)].symbol.chars().next().unwrap();
                let now = glitched[(x, y)].symbol.chars().next().unwrap();
                assert!("▀▄█".contains(was), "{was:?} at {x},{y} was not a letter");
                assert!(glitch::CHARS.contains(&(now as u8)), "{now:?}");
                differing.push((x, y));
            }
        }
        assert!(differing.len() > 20, "{}", differing.len());
        // all within the rows of the title
        let top = differing.iter().map(|&(_, y)| y).min().unwrap();
        let bottom = differing.iter().map(|&(_, y)| y).max().unwrap();
        assert!(bottom - top < title::RUNES.height(), "{top}..{bottom}");
        assert!(glitches.borrow().logo.get().is_some());
        assert!(glitches.borrow().background.get().is_none());
    }

    #[test]
    fn a_theme_without_effects_draws_no_glitch() {
        let born = Instant::now();
        let stats = loaded(born);
        let theme = vortex_view::theme::Loader::new(&[]).default();
        let glitches = RefCell::new(Glitches::default());
        for mode in [DashboardBackground::Static, DashboardBackground::Off] {
            for k in 0..10 {
                let now = settled(born) + Duration::from_millis(k * 70);
                let plain =
                    surface_themed(&theme, &stats, (160, 50), timeline(born, mode), now, None);
                let with = surface_themed(
                    &theme,
                    &stats,
                    (160, 50),
                    timeline(born, mode),
                    now,
                    Some(&glitches),
                );
                assert_eq!(with, plain, "{mode:?} frame {k}");
            }
        }
        assert!(glitches.borrow().logo.get().is_none());
        assert!(glitches.borrow().background.get().is_none());
    }

    #[test]
    fn the_background_glitches_over_the_vortex_and_never_the_panels() {
        let born = Instant::now();
        let stats = loaded(born);
        let theme = themed("glitch-background = 10.0");
        let still = timeline(born, DashboardBackground::Static);
        let off = surface_themed(
            &theme,
            &stats,
            (160, 50),
            timeline(born, DashboardBackground::Off),
            settled(born),
            None,
        );
        let glitches = RefCell::new(Glitches::default());
        let mut changes = 0;
        let mut seen = std::collections::HashSet::<char>::new();
        for k in 0..30 {
            let now = settled(born) + Duration::from_millis(k * 50);
            let plain = surface_themed(&theme, &stats, (160, 50), still, now, None);
            let glitched = surface_themed(&theme, &stats, (160, 50), still, now, Some(&glitches));
            let vortex: std::collections::HashSet<_> =
                vortex_cells(&plain, &off).into_iter().collect();
            assert!(vortex.len() > 500);
            for (x, y) in vortex_cells(&glitched, &plain) {
                assert!(
                    vortex.contains(&(x, y)),
                    "glitch outside the vortex at {x},{y}"
                );
                let c = glitched[(x, y)].symbol.chars().next().unwrap();
                assert!(glitch::CHARS.contains(&(c as u8)), "{c:?}");
                changes += 1;
                seen.insert(c);
            }
            // the panels and the title are as without the glitch
            for y in 0..50 {
                for x in 0..160 {
                    if !vortex.contains(&(x, y)) {
                        assert_eq!(glitched[(x, y)].symbol, plain[(x, y)].symbol, "{x},{y}");
                    }
                }
            }
        }
        assert!(changes > 100, "{changes}");
        assert!(seen.len() > 5);
        assert!(glitches.borrow().background.get().is_some());
        assert!(glitches.borrow().logo.get().is_none());
    }

    #[test]
    fn the_vortex_never_draws_on_the_panels() {
        let born = Instant::now();
        let stats = loaded(born);
        for (width, height) in SIZES {
            for mode in MODES {
                let mut t = 0;
                while t < 8000 {
                    let now = born + Duration::from_millis(t);
                    t += 97;
                    let without = surface(
                        &stats,
                        width,
                        height,
                        timeline(born, DashboardBackground::Off),
                        now,
                    );
                    let with = surface(&stats, width, height, timeline(born, mode), now);
                    // a panel is the margin of 2 columns and 1 row around the text of a part: the
                    // text is all there, and nothing else is in the margin
                    for y in 0..height {
                        for x in 0..width {
                            if is_blank(&without, x, y) {
                                continue;
                            }
                            assert_eq!(
                                with[(x, y)].symbol,
                                without[(x, y)].symbol,
                                "{mode:?} {width}x{height} at {}ms: text lost at {x},{y}:\n{}",
                                t - 97,
                                text(&with)
                            );
                            for ty in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                                for tx in x.saturating_sub(2)..=(x + 2).min(width - 1) {
                                    assert_eq!(
                                        with[(tx, ty)].symbol,
                                        without[(tx, ty)].symbol,
                                        "{mode:?} {width}x{height} at {}ms: the vortex at \
                                         {tx},{ty} is next to the text at {x},{y}:\n{}",
                                        t - 97,
                                        text(&with)
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_vortex_is_drawn_in_the_modes_that_have_one() {
        let born = Instant::now();
        let stats = loaded(born);
        let now = settled(born);
        let off = surface(
            &stats,
            160,
            50,
            timeline(born, DashboardBackground::Off),
            now,
        );
        for mode in [DashboardBackground::On, DashboardBackground::Static] {
            let with = surface(&stats, 160, 50, timeline(born, mode), now);
            assert!(vortex_cells(&with, &off).len() > 100, "{mode:?}");
        }
    }

    #[test]
    fn the_off_mode_draws_no_shades_once_the_title_is_open() {
        let born = Instant::now();
        let stats = loaded(born);
        let open = born + super::super::timeline::MIN_LOADING + super::super::timeline::TITLE;
        for (width, height) in SIZES {
            let mut t = open;
            while t < born + Duration::from_secs(8) {
                let drawn = surface(
                    &stats,
                    width,
                    height,
                    timeline(born, DashboardBackground::Off),
                    t,
                );
                let shades = text(&drawn).chars().filter(|c| "░▒▓".contains(*c)).count();
                assert_eq!(shades, 0, "{width}x{height}:\n{}", text(&drawn));
                t += Duration::from_millis(397);
            }
        }
    }

    #[test]
    fn a_small_window_draws_no_vortex() {
        let born = Instant::now();
        let stats = loaded(born);
        for mode in MODES {
            for t in (0..8000).step_by(331) {
                let now = born + Duration::from_millis(t);
                let off = surface(
                    &stats,
                    40,
                    12,
                    timeline(born, DashboardBackground::Off),
                    now,
                );
                let with = surface(&stats, 40, 12, timeline(born, mode), now);
                assert_eq!(text(&with), text(&off), "{mode:?} at {t}ms");
            }
        }
    }

    #[test]
    fn without_the_title_its_block_is_gone() {
        let born = Instant::now();
        let stats = loaded(born + Duration::from_millis(300));
        let now = settled(born);
        let draw = |width, height, title| {
            let off = timeline(born, DashboardBackground::Off);
            text(&surface_titled(&stats, width, height, off, now, title))
        };
        let titled = draw(100, 44, true);
        let untitled = draw(100, 44, false);
        assert!(titled.contains("██▀▀▄▄") && titled.contains(title::CREDIT));
        assert!(
            !untitled.contains('█') && !untitled.contains('▀'),
            "{untitled}"
        );
        assert!(!untitled.contains(title::CREDIT), "{untitled}");
        let row = |drawn: &str, text: &str| drawn.lines().position(|line| line.contains(text));
        // the rows of the title are gone, so what was under it moves up
        assert!(row(&untitled, "PROJECT") < row(&titled, "PROJECT"));
        // and a short window keeps what the title would have pushed out
        let shows = |height, title, text| draw(100, height, title).contains(text);
        for height in 10..44 {
            if shows(height, true, "LANGUAGES") {
                assert!(shows(height, false, "LANGUAGES"), "{height} rows");
            }
        }
        assert!((10..44).any(|h| !shows(h, true, "LANGUAGES") && shows(h, false, "LANGUAGES")));
        snapshot(
            "no-title-100",
            &text(&surface_titled(
                &stats,
                100,
                44,
                timeline(born, DashboardBackground::Static),
                now,
                false,
            )),
        );
    }

    /// `:vortex`: the vortex and the logo on its panel, nothing else.
    fn vortex_only(title: bool, width: u16, height: u16) -> String {
        let born = Instant::now();
        let stats = loaded(born + Duration::from_millis(300));
        let theme = vortex_view::theme::Loader::new(&[]).default();
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Surface::empty(area);
        let hints = hints();
        let model = Model {
            stats: &stats,
            timeline: timeline(born, DashboardBackground::Static),
            dir: Path::new("/src/vortex"),
            hints: &hints,
            title,
            vortex_only: true,
            glitches: None,
        };
        render(&model, &theme, area, &mut buffer, settled(born));
        text(&buffer)
    }

    #[test]
    fn the_vortex_alone_keeps_the_logo_unless_the_title_is_off() {
        let titled = vortex_only(true, 160, 50);
        assert!(titled.contains("██▀▀▄▄"), "{titled}");
        // the logo on its panel, centered, with no credit line, stats or actions
        let row = titled
            .lines()
            .position(|line| line.contains("██▀▀▄▄"))
            .unwrap();
        assert!((20..=25).contains(&row), "row {row}");
        let line = titled.lines().nth(row).unwrap();
        let left = line.find("▀█      █▀").unwrap();
        assert!(line[..left].ends_with("  "), "no panel margin: {line}");
        for gone in [title::CREDIT, "PROJECT", "Start typing"] {
            assert!(!titled.contains(gone), "{gone}");
        }
        let untitled = vortex_only(false, 160, 50);
        assert!(!untitled.contains('▀'), "{untitled}");
        // a narrow window takes a smaller title
        let narrow = vortex_only(true, 60, 20);
        assert!(
            !narrow.contains("██▀▀▄▄") && narrow.contains("┏━┓"),
            "{narrow}"
        );
    }

    #[test]
    fn the_first_frame_has_the_title_as_vx() {
        let born = Instant::now();
        let stats = loaded(born);
        let drawn = surface(
            &stats,
            160,
            50,
            timeline(born, DashboardBackground::Off),
            born,
        );
        let cells = title::RUNES.cells(0.0);
        let open = title::RUNES.cells(1.0);
        assert!(!cells.is_empty() && cells.len() < open.len());
        let left = (160 - title::RUNES.width()) / 2;
        let rows = title::RUNES.height();
        // some row puts the title there, and nothing else is in its box
        let found = (0..50 - rows).any(|top| {
            let drawn_cells = cells.iter().all(|cell| {
                drawn[(left + cell.x, top + cell.y)].symbol.as_str() == cell.c.to_string()
            });
            let used = (top..top + rows)
                .flat_map(|y| (left..left + title::RUNES.width()).map(move |x| (x, y)))
                .filter(|&(x, y)| !is_blank(&drawn, x, y))
                .count();
            drawn_cells && used == cells.len()
        });
        assert!(found, "no VX on:\n{}", text(&drawn));
    }

    #[test]
    fn a_still_vortex() {
        let born = Instant::now();
        let stats = loaded(born);
        let still = timeline(born, DashboardBackground::Static);
        snapshot(
            "static-160",
            &draw_with(&stats, 160, 50, still, settled(born)),
        );
        snapshot(
            "static-100",
            &draw_with(&stats, 100, 44, still, settled(born)),
        );
    }

    #[test]
    fn no_vortex() {
        let born = Instant::now();
        let stats = loaded(born);
        snapshot("off-160", &draw(&stats, 160, 44, born, settled(born)));
        snapshot("off-100", &draw(&stats, 100, 44, born, settled(born)));
    }

    #[test]
    fn vx_opens_to_vortex() {
        let born = Instant::now();
        let stats = Stats::default();
        let ready = born + super::super::timeline::MIN_LOADING;
        let title = super::super::timeline::TITLE;
        let off = timeline(born, DashboardBackground::Off);
        snapshot(
            "title-vx",
            &draw_with(&stats, 100, 20, off, born + title / 2),
        );
        snapshot(
            "title-opening",
            &draw_with(&stats, 100, 20, off, ready + title.mul_f64(0.55)),
        );
        snapshot(
            "title-vortex",
            &draw_with(&stats, 100, 20, off, ready + title),
        );
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
        let still = timeline(born, DashboardBackground::Static);
        snapshot(
            "folder-160",
            &draw_with(&stats, 160, 44, still, settled(born)),
        );
        snapshot("folder-100", &draw(&stats, 100, 44, born, settled(born)));
    }

    #[test]
    fn narrow_and_short_windows() {
        let born = Instant::now();
        let stats = loaded(born);
        let still = timeline(born, DashboardBackground::Static);
        snapshot(
            "narrow-60x24",
            &draw_with(&stats, 60, 24, still, settled(born)),
        );
        // too small for a vortex
        snapshot(
            "narrow-40x12",
            &draw_with(&stats, 40, 12, still, settled(born)),
        );
    }

    #[test]
    fn long_values_wrap() {
        let born = Instant::now();
        let stats = loaded(born);
        stats.head.set_at(
            Ok(Head {
                branch: Some("feature/dashboard-background-and-title".into()),
                commit: Some(Commit {
                    short_id: "a72b8d0".into(),
                    summary: "feat(dashboard): a startup dashboard for vx and vx <folder>, \
                              with the stats of the project and its languages (#28)"
                        .into(),
                }),
            }),
            born,
        );
        stats.remote.set_at(
            Ok(Some(Remote {
                name: "upstream".into(),
                url: "https://git.example.org/some-organization/a-rather-long-repository-name.git"
                    .into(),
            })),
            born,
        );
        snapshot("wrapped-160", &draw(&stats, 160, 44, born, settled(born)));
        snapshot("wrapped-100", &draw(&stats, 100, 44, born, settled(born)));
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

    /// Writes HTML pictures of the dashboard to `$PREVIEW_DIR`, for looking at it while
    /// working on it: `PREVIEW_DIR=/tmp/p cargo test -p vortex-term preview -- --ignored`.
    /// `PREVIEW_THEME` picks a theme by name, `PREVIEW_TIMES` the times in milliseconds,
    /// `PREVIEW_SIZE` the size (`160x50`) and `PREVIEW_MODE` the background.
    #[test]
    #[ignore]
    fn preview() {
        let Some(dir) = std::env::var_os("PREVIEW_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let loader = vortex_view::theme::Loader::new(&[
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime")
        ]);
        let theme = match std::env::var("PREVIEW_THEME") {
            Ok(name) => loader.load(&name).unwrap(),
            Err(_) => loader.default(),
        };
        let (width, height) = std::env::var("PREVIEW_SIZE")
            .ok()
            .and_then(|size| {
                let (w, h) = size.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((160, 50));
        let mode = match std::env::var("PREVIEW_MODE").as_deref() {
            Ok("settle" | "load") => DashboardBackground::Settle,
            Ok("drain") => DashboardBackground::Drain,
            Ok("grow") => DashboardBackground::Grow,
            Ok("grow-on") => DashboardBackground::GrowOn,
            Ok("static") => DashboardBackground::Static,
            Ok("off") => DashboardBackground::Off,
            _ => DashboardBackground::On,
        };
        let times: Vec<u64> = std::env::var("PREVIEW_TIMES")
            .unwrap_or_else(|_| "5000".into())
            .split(',')
            .map(|t| t.parse().unwrap())
            .collect();
        let born = Instant::now();
        let stats = loaded(born + Duration::from_millis(300));
        let hints = hints();
        for t in times {
            let area = Rect::new(0, 0, width, height);
            let mut buffer = Surface::empty(area);
            let model = Model {
                stats: &stats,
                timeline: Timeline::new(born, mode),
                dir: Path::new("/src/vortex"),
                hints: &hints,
                title: true,
                vortex_only: false,
                glitches: None,
            };
            render(
                &model,
                &theme,
                area,
                &mut buffer,
                born + Duration::from_millis(t),
            );
            std::fs::write(dir.join(format!("frame-{t:05}.html")), html(&buffer)).unwrap();
        }
    }

    fn css(color: Color, fallback: &str) -> String {
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Reset => fallback.to_string(),
            Color::Black => "#000".into(),
            Color::Red => "#c33".into(),
            Color::Green => "#3c3".into(),
            Color::Yellow => "#cc3".into(),
            Color::Blue => "#36c".into(),
            Color::Magenta => "#c3c".into(),
            Color::Cyan => "#3cc".into(),
            Color::Gray => "#888".into(),
            Color::LightRed => "#f66".into(),
            Color::LightGreen => "#6f6".into(),
            Color::LightYellow => "#ff6".into(),
            Color::LightBlue => "#69f".into(),
            Color::LightMagenta => "#f6f".into(),
            Color::LightCyan => "#6ff".into(),
            Color::LightGray => "#bbb".into(),
            Color::White => "#fff".into(),
            Color::Indexed(i) => format!("hsl({}, 50%, 60%)", i as u32 * 37 % 360),
        }
    }

    fn html(buffer: &Surface) -> String {
        let area = buffer.area;
        let mut out = String::from(
            "<html><body style='margin:0;background:#1d1d1d'><pre style='font: 14px/17px \"DejaVu Sans Mono\", monospace; margin:8px'>",
        );
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &buffer[(x, y)];
                let fg = css(cell.fg, "#ddd");
                let bg = css(cell.bg, "#1d1d1d");
                let symbol = match cell.symbol.as_str() {
                    "<" => "&lt;",
                    ">" => "&gt;",
                    "&" => "&amp;",
                    s => s,
                };
                let bold = if cell.modifier.contains(Modifier::BOLD) {
                    "font-weight:bold;"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "<span style='color:{fg};background:{bg};{bold}'>{symbol}</span>"
                ));
            }
            out.push('\n');
        }
        out.push_str("</pre></body></html>");
        out
    }
}
