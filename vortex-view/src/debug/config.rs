//! `[editor.debugger]`: spectator mode's defaults, and the engine.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::engine::EngineKind;
use super::history::Caps;
use super::lineline::DEFAULT_CAP;
use super::spectator::{Follow, SPEEDS};

/// `[editor.debugger]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct DebuggerConfig {
    /// Lines per second playback starts at (the nearest of 0.5 to 25). Defaults to 2.
    pub speed: f64,
    /// What playback steps into: `std`, `lib` or `own`. Defaults to `own`.
    pub follow: FollowConfig,
    /// Levels of variables recorded below each scope at every stop. Defaults to 3.
    #[serde(with = "limit")]
    pub capture_depth: Option<usize>,
    /// Children of a variable recorded at most (its length is kept). Defaults to 100.
    #[serde(with = "limit")]
    pub capture_collection: Option<usize>,
    /// Stops kept for rewind; 0 records nothing. Defaults to 2000.
    #[serde(with = "limit")]
    pub history: Option<usize>,
    /// Bytes of each open memory view recorded at every stop. Defaults to 4096.
    #[serde(with = "limit")]
    pub capture_memory: Option<usize>,
    /// Show how long each step took at the end of the line it left. Defaults to true.
    pub time_labels: bool,
    /// Steps faster than this, in milliseconds after the stepping overhead, get no label.
    /// Defaults to 1.
    pub time_threshold: f64,
    /// How playback advances: `breakpoints` (hidden breakpoints on every line) or `stepping`.
    /// Defaults to `breakpoints`.
    pub engine: EngineConfig,
    /// Hits of every thread kept on the lineline; `"unlimited"` keeps them all. Defaults to
    /// 1,000,000 (about 8 MB).
    #[serde(with = "limit")]
    pub line_record: Option<usize>,
}

impl Default for DebuggerConfig {
    fn default() -> Self {
        let caps = Caps::default();
        Self {
            speed: 2.0,
            follow: FollowConfig::Own,
            capture_depth: caps.depth,
            capture_collection: caps.collection,
            history: caps.history,
            capture_memory: caps.memory,
            time_labels: true,
            time_threshold: 1.0,
            engine: EngineConfig::Breakpoints,
            line_record: Some(DEFAULT_CAP),
        }
    }
}

// the floats are plain settings, compared as values
impl Eq for DebuggerConfig {}

impl DebuggerConfig {
    pub fn caps(&self) -> Caps {
        Caps {
            depth: self.capture_depth,
            collection: self.capture_collection,
            history: self.history,
            memory: self.capture_memory,
        }
    }

    /// The index of the speed nearest to `speed`.
    pub fn speed_index(&self) -> usize {
        SPEEDS
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (*a - self.speed)
                    .abs()
                    .partial_cmp(&(*b - self.speed).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map_or(0, |(index, _)| index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineConfig {
    Breakpoints,
    Stepping,
}

impl From<EngineConfig> for EngineKind {
    fn from(config: EngineConfig) -> EngineKind {
        match config {
            EngineConfig::Breakpoints => EngineKind::Breakpoints,
            EngineConfig::Stepping => EngineKind::Stepping,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowConfig {
    Std,
    Lib,
    Own,
}

impl From<FollowConfig> for Follow {
    fn from(config: FollowConfig) -> Follow {
        match config {
            FollowConfig::Std => Follow::Std,
            FollowConfig::Lib => Follow::Lib,
            FollowConfig::Own => Follow::Own,
        }
    }
}

/// A number, or `"unlimited"` (`None`).
mod limit {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &Option<usize>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(n) => serializer.serialize_u64(*n as u64),
            None => serializer.serialize_str("unlimited"),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<usize>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Limit {
            Count(usize),
            Word(String),
        }
        match Limit::deserialize(deserializer)? {
            Limit::Count(n) => Ok(Some(n)),
            Limit::Word(word) if word == "unlimited" => Ok(None),
            Limit::Word(word) => Err(serde::de::Error::custom(format!(
                "expected a number or \"unlimited\", got \"{word}\""
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_take_numbers_or_unlimited() {
        let config: DebuggerConfig = toml::from_str(
            "capture-depth = 5\nhistory = \"unlimited\"\nspeed = 11\nfollow = \"lib\"",
        )
        .unwrap();
        assert_eq!(config.capture_depth, Some(5));
        assert_eq!(config.history, None);
        assert_eq!(config.capture_collection, Some(100));
        assert_eq!(SPEEDS[config.speed_index()], 12.0);
        assert_eq!(Follow::from(config.follow), Follow::Lib);
        assert!(toml::from_str::<DebuggerConfig>("history = \"lots\"").is_err());
        let config: DebuggerConfig =
            toml::from_str("engine = \"stepping\"\nline-record = \"unlimited\"").unwrap();
        assert_eq!(EngineKind::from(config.engine), EngineKind::Stepping);
        assert_eq!(config.line_record, None);
        assert_eq!(DebuggerConfig::default().line_record, Some(1_000_000));
        let default = DebuggerConfig::default();
        assert_eq!(SPEEDS[default.speed_index()], 2.0);
        assert_eq!(default.caps(), Caps::default());
    }
}
