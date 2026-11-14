use crate::keymap;
use crate::keymap::{merge_keys, merge_layers, KeyTrie, Layer};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs;
use std::io::Error as IOError;
use toml::de::Error as TomlError;
use vortex_loader::merge_toml_values;
use vortex_view::{document::Mode, theme};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub theme: Option<theme::Config>,
    pub keys: HashMap<Mode, KeyTrie>,
    /// The keys of the fork's modes (`[keys.watch]`, `[keys.debug]`).
    pub layers: HashMap<Layer, KeyTrie>,
    pub editor: vortex_view::editor::Config,
}

/// `[keys]`: a table per editor mode, and the fork's layers.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RawKeys {
    watch: Option<KeyTrie>,
    debug: Option<KeyTrie>,
    #[serde(flatten)]
    modes: HashMap<Mode, KeyTrie>,
}

impl RawKeys {
    /// Merges these keys into `keys` and `layers`.
    fn merge_into(self, keys: &mut HashMap<Mode, KeyTrie>, layers: &mut HashMap<Layer, KeyTrie>) {
        merge_keys(keys, self.modes);
        if let Some(watch) = self.watch {
            merge_layers(layers, HashMap::from([(Layer::Watch, watch)]));
        }
        if let Some(debug) = self.debug {
            merge_layers(layers, HashMap::from([(Layer::Debug, debug)]));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRaw {
    pub theme: Option<theme::Config>,
    pub keys: Option<RawKeys>,
    pub editor: Option<toml::Value>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            theme: None,
            keys: keymap::default(),
            layers: keymap::layers(),
            editor: vortex_view::editor::Config::default(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigLoadError {
    BadConfig(TomlError),
    Error(IOError),
}

impl Default for ConfigLoadError {
    fn default() -> Self {
        ConfigLoadError::Error(IOError::new(std::io::ErrorKind::NotFound, "place holder"))
    }
}

impl Display for ConfigLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigLoadError::BadConfig(err) => err.fmt(f),
            ConfigLoadError::Error(err) => err.fmt(f),
        }
    }
}

impl Config {
    pub fn load(
        global: Result<&String, ConfigLoadError>,
        local: Result<String, ConfigLoadError>,
    ) -> Result<Config, ConfigLoadError> {
        let global_config: Result<ConfigRaw, ConfigLoadError> =
            global.and_then(|file| toml::from_str(file).map_err(ConfigLoadError::BadConfig));
        let local_config: Result<ConfigRaw, ConfigLoadError> =
            local.and_then(|file| toml::from_str(&file).map_err(ConfigLoadError::BadConfig));
        let res = match (global_config, local_config) {
            (Ok(global), Ok(local)) => {
                let mut keys = keymap::default();
                let mut layers = keymap::layers();
                if let Some(global_keys) = global.keys {
                    global_keys.merge_into(&mut keys, &mut layers);
                }
                if let Some(local_keys) = local.keys {
                    local_keys.merge_into(&mut keys, &mut layers);
                }

                let editor = match (global.editor, local.editor) {
                    (None, None) => vortex_view::editor::Config::default(),
                    (None, Some(val)) | (Some(val), None) => {
                        val.try_into().map_err(ConfigLoadError::BadConfig)?
                    }
                    (Some(global), Some(local)) => merge_toml_values(global, local, 3)
                        .try_into()
                        .map_err(ConfigLoadError::BadConfig)?,
                };

                Config {
                    theme: local.theme.or(global.theme),
                    keys,
                    layers,
                    editor,
                }
            }
            // if any configs are invalid return that first
            (_, Err(ConfigLoadError::BadConfig(err)))
            | (Err(ConfigLoadError::BadConfig(err)), _) => {
                return Err(ConfigLoadError::BadConfig(err))
            }
            (Ok(config), Err(_)) | (Err(_), Ok(config)) => {
                let mut keys = keymap::default();
                let mut layers = keymap::layers();
                if let Some(keymap) = config.keys {
                    keymap.merge_into(&mut keys, &mut layers);
                }
                Config {
                    theme: config.theme,
                    keys,
                    layers,
                    editor: config.editor.map_or_else(
                        || Ok(vortex_view::editor::Config::default()),
                        |val| val.try_into().map_err(ConfigLoadError::BadConfig),
                    )?,
                }
            }

            // these are just two io errors return the one for the global config
            (Err(err), Err(_)) => return Err(err),
        };

        Ok(res)
    }

    pub fn load_default() -> Result<Config, ConfigLoadError> {
        let global_config =
            fs::read_to_string(vortex_loader::config_file()).map_err(ConfigLoadError::Error)?;
        let local_config = fs::read_to_string(vortex_loader::workspace_config_file())
            .map_err(ConfigLoadError::Error);

        let phony_config = ConfigLoadError::Error(IOError::other("hacky placeholder"));
        let global_parsed = Config::load(Ok(&global_config), Err(phony_config))?;

        // We need to build a transient `WorkspaceTrust` just to ask whether the workspace is
        // trusted enough to load its `.vortex/config.toml`. The persisted-trust file on disk is the
        // source of truth either way; this transient instance has an empty cache and is dropped
        // after the check.
        let trust = vortex_loader::workspace_trust::WorkspaceTrust::new(
            (&global_parsed.editor.workspace_trust).into(),
        );
        if trust
            .query_current(vortex_loader::workspace_trust::TrustQuery::LocalConfig)
            .is_trusted()
        {
            let mut merged = Config::load(Ok(&global_config), local_config)?;
            // editor.workspace-trust is global/user-scope only. Without this override, a
            // workspace's `.vortex/config.toml` could set `level = "insecure"`; once the user trusted
            // *that* workspace, refresh_config would re-load with the override merged in and from
            // then on every subsequent workspace in the session would be implicitly trusted. Pin
            // the gate's own configuration to the global file.
            merged.editor.workspace_trust = global_parsed.editor.workspace_trust;
            Ok(merged)
        } else {
            Ok(global_parsed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Config {
        fn load_test(config: &str) -> Config {
            Config::load(Ok(&config.to_owned()), Err(ConfigLoadError::default())).unwrap()
        }
    }

    #[test]
    fn parsing_keymaps_config_file() {
        use crate::keymap;
        use vortex_core::hashmap;
        use vortex_view::document::Mode;

        let sample_keymaps = r#"
            [keys.insert]
            y = "move_line_down"
            S-C-a = "delete_selection"

            [keys.normal]
            A-F12 = "move_next_word_end"
        "#;

        let mut keys = keymap::default();
        merge_keys(
            &mut keys,
            hashmap! {
                Mode::Insert => keymap!({ "Insert mode"
                    "y" => move_line_down,
                    "S-C-a" => delete_selection,
                }),
                Mode::Normal => keymap!({ "Normal mode"
                    "A-F12" => move_next_word_end,
                }),
            },
        );

        assert_eq!(
            Config::load_test(sample_keymaps),
            Config {
                keys,
                ..Default::default()
            }
        );
    }

    /// `[keys.watch]` remaps watch mode's keys; the other modes' tables still work beside it.
    #[test]
    fn watch_keys_in_their_own_table() {
        use crate::keymap::{KeyTrie, Layer};
        let config = Config::load_test(
            r#"
            [keys.watch]
            l = "watch_look"
            v = "no_op"

            [keys.normal]
            A-F12 = "move_next_word_end"
        "#,
        );
        let watch = &config.layers[&Layer::Watch];
        let look = KeyTrie::MappableCommand(crate::commands::MappableCommand::watch_look);
        assert_eq!(watch.search(&["l".parse().unwrap()]), Some(&look));
        assert_ne!(watch.search(&["v".parse().unwrap()]), Some(&look));
        // the defaults are still there
        assert!(watch.search(&["n".parse().unwrap()]).is_some());
        assert!(config.keys[&Mode::Normal]
            .search(&["A-F12".parse().unwrap()])
            .is_some());
        // an unknown table is an error
        assert!(Config::load(
            Ok(&"[keys.wach]\nl = \"watch_look\"".to_owned()),
            Err(ConfigLoadError::default()),
        )
        .is_err());
    }

    #[test]
    fn keys_resolve_to_correct_defaults() {
        // From serde default
        let default_keys = Config::load_test("").keys;
        assert_eq!(default_keys, keymap::default());

        // From the Default trait
        let default_keys = Config::default().keys;
        assert_eq!(default_keys, keymap::default());
    }

    #[test]
    fn explorer_options_of_the_former_side_panel_are_ignored() {
        let config = Config::load_test(
            "[editor.explorer]\nwidth = 40\nicons = false\nauto-reveal = false\n\
             hide-gitignored = true\ngit-status = false",
        );
        assert!(config.editor.explorer.hide_gitignored);
        assert!(!config.editor.explorer.git_status);

        // Other unknown options are still an error.
        let typo = Config::load(
            Ok(&"[editor.explorer]\nhide-gitignore = true".to_owned()),
            Err(ConfigLoadError::default()),
        );
        assert!(typo.is_err());
    }
}
