//! A theme's effects (this fork, see FORK.md, "Theme effects"): the optional `[effects]` table
//! of a theme file. Themes are data, and so are their effects: how the dashboard draws its
//! background and title, the glitch rates, the terminal panel's phosphor filter. The table is
//! read once, when the theme loads, and inherited through `inherits` key by key, like the
//! palette.
//!
//! Every effect is off when a theme doesn't set it. A wrong value is a warning (shown like the
//! theme's other warnings) and falls back to the default; it never stops the theme from loading.

use toml::{map::Map, Value};

use crate::graphics::Color;

/// How the dashboard's background and the title's opening are drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DashboardCharset {
    /// Shaded blocks, `·░▒▓█`.
    #[default]
    Blocks,
    /// ASCII characters ordered by how much ink they have.
    Ascii,
}

/// Which wordmark the dashboard's title is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DashboardLogo {
    /// The runic, brush-stroke wordmark.
    #[default]
    Runes,
    /// The block letters with a shadow the dashboard had first.
    Classic,
}

/// The highest glitch rate a theme may set: at 10 a second, a cell with a glitch of about
/// 100 ms is glitched about all the time.
pub const MAX_GLITCH_RATE: f64 = 10.0;

/// The effects of a theme. [`Effects::default`] is a theme without an `[effects]` table.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Effects {
    pub dashboard_charset: DashboardCharset,
    pub dashboard_logo: DashboardLogo,
    /// Glitch rates, each a chance per cell per second (0 is off): the dashboard's title, its
    /// background, and the ghost text of error diagnostics.
    pub glitch_logo: f64,
    pub glitch_background: f64,
    pub glitch_diagnostics: f64,
    /// The color the terminal panel is drawn in, at each color's own brightness; `None` is off.
    pub terminal_phosphor: Option<Color>,
}

/// The keys of the table, for the warning about an unknown one.
const KEYS: [&str; 6] = [
    "dashboard-charset",
    "dashboard-logo",
    "glitch-logo",
    "glitch-background",
    "glitch-diagnostics",
    "terminal-phosphor",
];

impl Effects {
    /// Reads the `[effects]` table; `color` resolves a color the way the theme's styles do (the
    /// palette's names, `#rrggbb`, ANSI numbers). Every wrong value is a warning in `warnings`,
    /// and the effect keeps its default.
    pub fn parse(
        value: Value,
        color: impl Fn(Value) -> Result<Color, String>,
        warnings: &mut Vec<String>,
    ) -> Self {
        let mut effects = Self::default();
        let Value::Table(table) = value else {
            warnings.push(format!("[effects] should be a table, found {value}"));
            return effects;
        };
        let mut warn = |key: &str, message: String| {
            warnings.push(format!("[effects] {key}: {message}, ignored"));
        };
        for (key, value) in table {
            match key.as_str() {
                "dashboard-charset" => match value.as_str() {
                    Some("blocks") => effects.dashboard_charset = DashboardCharset::Blocks,
                    Some("ascii") => effects.dashboard_charset = DashboardCharset::Ascii,
                    _ => warn(
                        &key,
                        format!("expected \"blocks\" or \"ascii\", found {value}"),
                    ),
                },
                "dashboard-logo" => match value.as_str() {
                    Some("runes") => effects.dashboard_logo = DashboardLogo::Runes,
                    Some("classic") => effects.dashboard_logo = DashboardLogo::Classic,
                    _ => warn(
                        &key,
                        format!("expected \"runes\" or \"classic\", found {value}"),
                    ),
                },
                "glitch-logo" | "glitch-background" | "glitch-diagnostics" => {
                    let rate = match value {
                        Value::Float(rate) => Some(rate),
                        Value::Integer(rate) => Some(rate as f64),
                        _ => None,
                    };
                    match rate {
                        Some(rate) if (0.0..=MAX_GLITCH_RATE).contains(&rate) => {
                            let slot = match key.as_str() {
                                "glitch-logo" => &mut effects.glitch_logo,
                                "glitch-background" => &mut effects.glitch_background,
                                _ => &mut effects.glitch_diagnostics,
                            };
                            *slot = rate;
                        }
                        _ => warn(
                            &key,
                            format!(
                                "expected a chance per cell per second from 0 to \
                                 {MAX_GLITCH_RATE}, found {value}"
                            ),
                        ),
                    }
                }
                "terminal-phosphor" => match color(value.clone()) {
                    Ok(color) => effects.terminal_phosphor = Some(color),
                    Err(err) => warn(&key, format!("not a color: {value} {err}")),
                },
                _ => warn(
                    &key,
                    format!("unknown effect (the effects are {})", KEYS.join(", ")),
                ),
            }
        }
        effects
    }
}

/// Merges a child theme's `[effects]` into its parent's, key by key: a child that sets one
/// effect keeps the others of its parent.
pub fn merge(parent: Option<&Value>, child: Option<&Value>) -> Option<Value> {
    match (parent, child) {
        (Some(Value::Table(parent)), Some(Value::Table(child))) => {
            let mut merged: Map<String, Value> = parent.clone();
            merged.extend(child.clone());
            Some(Value::Table(merged))
        }
        (_, Some(child)) => Some(child.clone()),
        (parent, None) => parent.cloned(),
    }
}
