//! Upstream Helix, which Vortex is a fork of: the version it branched from and the hint for
//! people who used Helix before. The only place in the code that names upstream's directories,
//! which Vortex never reads or writes.
use etcetera::base_strategy::{choose_base_strategy, BaseStrategy};
use std::path::Path;

use crate::{config_dir, data_dir};

/// The upstream release and commit Vortex branched from, as upstream's own `--version` printed
/// them there.
pub const VERSION: &str = "Helix 25.07.1 ba40e547";

/// Vortex never reads Helix's configuration. When it has no config of its own (its config
/// directory is missing or empty: startup creates it) but Helix's directory exists, this returns
/// a hint with the command that copies it, once: a marker in the data directory remembers that
/// it was given (and the hint isn't given if that can't be written). Only the existence of
/// Helix's directory is checked.
pub fn config_hint() -> Option<String> {
    let strategy = choose_base_strategy().ok()?;
    hint(
        &config_dir(),
        &strategy.config_dir().join("helix"),
        &data_dir().join("helix-config-hint-given"),
    )
}

fn hint(ours: &Path, helix: &Path, marker: &Path) -> Option<String> {
    let has_config = std::fs::read_dir(ours).is_ok_and(|mut entries| entries.next().is_some());
    if has_config || !helix.is_dir() || marker.exists() {
        return None;
    }
    crate::ensure_parent_dir(marker);
    std::fs::write(marker, "").ok()?;
    // the contents, into Vortex's directory: it may exist already (empty)
    let to = ours.display();
    let copy = format!("cp -r '{}' '{to}'", helix.join(".").display());
    Some(format!(
        "Vortex doesn't read Helix's config. To copy it: {copy}"
    ))
}

#[cfg(test)]
mod tests {
    use super::hint;

    #[test]
    fn given_once_when_only_helix_has_a_config() {
        let tmp = tempfile::tempdir().unwrap();
        let (ours, helix) = (tmp.path().join("vortex"), tmp.path().join("helix"));
        let marker = tmp.path().join("data").join("hint-given");
        // no config at all: nothing to say
        assert_eq!(hint(&ours, &helix, &marker), None);

        std::fs::create_dir(&helix).unwrap();
        std::fs::write(helix.join("config.toml"), "theme = \"x\"").unwrap();
        // startup creates Vortex's config directory: empty, it's no config
        std::fs::create_dir(&ours).unwrap();
        let text = hint(&ours, &helix, &marker).unwrap();
        let copy = format!("cp -r '{}/.' '{}'", helix.display(), ours.display());
        assert!(text.ends_with(&copy), "{text}");
        // the command does copy the config into the existing, empty directory
        let status = std::process::Command::new("sh")
            .args(["-c", text.split(": ").last().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
        assert!(ours.join("config.toml").is_file());
        std::fs::remove_file(ours.join("config.toml")).unwrap();
        assert!(marker.exists());
        // once
        assert_eq!(hint(&ours, &helix, &marker), None);
        // and never once Vortex has its own
        std::fs::remove_file(&marker).unwrap();
        std::fs::write(ours.join("config.toml"), "").unwrap();
        assert_eq!(hint(&ours, &helix, &marker), None);
        // Helix's directory is only looked at, never written
        let names: Vec<_> = std::fs::read_dir(&helix)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }
}
