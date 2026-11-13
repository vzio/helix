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

#[cfg(test)]
mod tests {
    use super::*;

    fn color(value: Value) -> Result<Color, String> {
        match value.as_str() {
            Some(s) if s.len() == 7 && s.starts_with('#') => {
                let n = u32::from_str_radix(&s[1..], 16).map_err(|e| e.to_string())?;
                Ok(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
            }
            _ => Err("expected #rrggbb".into()),
        }
    }

    fn parse(source: &str) -> (Effects, Vec<String>) {
        let value: Value = toml::from_str(source).unwrap();
        let mut warnings = Vec::new();
        let effects = Effects::parse(value, color, &mut warnings);
        (effects, warnings)
    }

    #[test]
    fn a_table_with_every_key_parses_to_its_values() {
        let (effects, warnings) = parse(
            r##"
            dashboard-charset = "ascii"
            dashboard-logo = "classic"
            glitch-logo = 0.6
            glitch-background = 0.004
            glitch-diagnostics = 0.01
            terminal-phosphor = "#33dd66"
            "##,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            effects,
            Effects {
                dashboard_charset: DashboardCharset::Ascii,
                dashboard_logo: DashboardLogo::Classic,
                glitch_logo: 0.6,
                glitch_background: 0.004,
                glitch_diagnostics: 0.01,
                terminal_phosphor: Some(Color::Rgb(0x33, 0xdd, 0x66)),
            }
        );
    }

    #[test]
    fn the_explicit_defaults_parse_to_the_defaults() {
        let (effects, warnings) =
            parse("dashboard-charset = \"blocks\"\ndashboard-logo = \"runes\"");
        assert!(warnings.is_empty());
        assert_eq!(effects, Effects::default());
    }

    #[test]
    fn an_empty_table_and_no_table_are_all_off() {
        let (effects, warnings) = parse("");
        assert!(warnings.is_empty());
        assert_eq!(effects, Effects::default());
        assert_eq!(effects.dashboard_charset, DashboardCharset::Blocks);
        assert_eq!(effects.dashboard_logo, DashboardLogo::Runes);
        assert_eq!(
            (
                effects.glitch_logo,
                effects.glitch_background,
                effects.glitch_diagnostics
            ),
            (0.0, 0.0, 0.0)
        );
        assert_eq!(effects.terminal_phosphor, None);
        // no table: `Theme` never calls `parse`, and `merge` of nothing is nothing
        assert_eq!(merge(None, None), None);
    }

    #[test]
    fn integer_rates_are_accepted() {
        let (effects, warnings) =
            parse("glitch-logo = 1\nglitch-background = 0\nglitch-diagnostics = 10");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(effects.glitch_logo, 1.0);
        assert_eq!(effects.glitch_background, 0.0);
        assert_eq!(effects.glitch_diagnostics, 10.0);
    }

    #[test]
    fn each_wrong_value_warns_once_and_keeps_the_rest() {
        let wrong = [
            ("dashboard-charset", "\"braille\""),
            ("dashboard-logo", "\"fancy\""),
            ("dashboard-charset", "3"),
            ("glitch-logo", "-0.5"),
            ("glitch-background", "10.5"),
            ("glitch-diagnostics", "11"),
            ("glitch-logo", "\"often\""),
            ("terminal-phosphor", "\"not a color\""),
            ("terminal-phosphor", "3"),
            ("no-such-effect", "true"),
        ];
        for (key, value) in wrong {
            let source = format!(
                "{key} = {value}\n{}",
                // valid keys other than the wrong one
                [
                    ("dashboard-charset", "\"ascii\""),
                    ("dashboard-logo", "\"classic\""),
                    ("glitch-logo", "2"),
                    ("glitch-background", "0.5"),
                    ("glitch-diagnostics", "0.25"),
                    ("terminal-phosphor", "\"#010203\""),
                ]
                .iter()
                .filter(|(k, _)| *k != key)
                .map(|(k, v)| format!("{k} = {v}\n"))
                .collect::<String>()
            );
            let (effects, warnings) = parse(&source);
            assert_eq!(warnings.len(), 1, "{key} = {value}: {warnings:?}");
            assert!(warnings[0].contains(key), "{key}: {warnings:?}");
            let mut expected = Effects {
                dashboard_charset: DashboardCharset::Ascii,
                dashboard_logo: DashboardLogo::Classic,
                glitch_logo: 2.0,
                glitch_background: 0.5,
                glitch_diagnostics: 0.25,
                terminal_phosphor: Some(Color::Rgb(1, 2, 3)),
            };
            match key {
                "dashboard-charset" => expected.dashboard_charset = DashboardCharset::Blocks,
                "dashboard-logo" => expected.dashboard_logo = DashboardLogo::Runes,
                "glitch-logo" => expected.glitch_logo = 0.0,
                "glitch-background" => expected.glitch_background = 0.0,
                "glitch-diagnostics" => expected.glitch_diagnostics = 0.0,
                "terminal-phosphor" => expected.terminal_phosphor = None,
                _ => {}
            }
            assert_eq!(effects, expected, "{key} = {value}");
        }
    }

    #[test]
    fn a_table_that_is_not_a_table_warns_and_is_all_off() {
        let mut warnings = Vec::new();
        let effects = Effects::parse(Value::Integer(3), color, &mut warnings);
        assert_eq!(effects, Effects::default());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("effects"), "{warnings:?}");
    }

    fn table(source: &str) -> Value {
        toml::from_str(source).unwrap()
    }

    #[test]
    fn merge_lets_the_child_override_and_keeps_the_parents_other_keys() {
        let parent =
            table("glitch-logo = 1.0\ndashboard-charset = \"ascii\"\nglitch-background = 0.1");
        let child = table("glitch-logo = 2.0\ndashboard-logo = \"classic\"");
        let merged = merge(Some(&parent), Some(&child)).unwrap();
        assert_eq!(
            merged,
            table(
                "glitch-logo = 2.0\ndashboard-charset = \"ascii\"\nglitch-background = 0.1\n\
                 dashboard-logo = \"classic\""
            )
        );
    }

    #[test]
    fn merge_with_only_a_child_or_only_a_parent() {
        let some = table("glitch-logo = 1.0");
        assert_eq!(merge(None, Some(&some)), Some(some.clone()));
        assert_eq!(merge(Some(&some), None), Some(some.clone()));
        assert_eq!(merge(None, None), None);
        // an empty child keeps the whole parent
        let empty = table("");
        assert_eq!(merge(Some(&some), Some(&empty)), Some(some));
    }
}
