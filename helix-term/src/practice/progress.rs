//! The best key count of each solved exercise, kept in `practice.toml` in the data directory:
//!
//! ```toml
//! [exercises."motions/01-word-motions"]
//! best = 12
//! ```

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use super::Exercise;

/// How far an exercise has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Never solved.
    Unsolved,
    /// Solved, with more keys than par.
    Solved,
    /// Solved with par keys or fewer.
    AtPar,
}

impl Status {
    /// The mark in the list of exercises.
    pub fn symbol(self) -> &'static str {
        match self {
            Status::Unsolved => "·",
            Status::Solved => "✓",
            Status::AtPar => "★",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    best: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default)]
    exercises: BTreeMap<String, Entry>,
}

thread_local! {
    static FILE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Makes this thread keep its progress in `path` rather than in the data directory. Tests use it
/// so that they never touch the real file.
pub fn set_progress_file(path: impl Into<PathBuf>) {
    FILE.with_borrow_mut(|file| *file = Some(path.into()));
}

/// The file with the progress.
pub fn progress_file() -> PathBuf {
    FILE.with_borrow(Clone::clone)
        .unwrap_or_else(|| helix_loader::data_dir().join("practice.toml"))
}

impl Progress {
    /// Reads the progress from `path`. A missing file is no progress. A file that can't be read
    /// or parsed is an error, and must not be overwritten.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|err| anyhow!("{}: {err}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(anyhow!("{}: {err}", path.display())),
        }
    }

    /// Writes the progress to `path`, through a temporary file next to it so that a crash
    /// doesn't leave half a file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = toml::to_string(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut temp = path.as_os_str().to_owned();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, text).map_err(|err| anyhow!("{}: {err}", temp.display()))?;
        std::fs::rename(&temp, path).map_err(|err| anyhow!("{}: {err}", path.display()))
    }

    /// The fewest keys `id` was solved with.
    pub fn best(&self, id: &str) -> Option<usize> {
        self.exercises.get(id).map(|entry| entry.best)
    }

    pub fn status(&self, exercise: &Exercise) -> Status {
        match self.best(&exercise.id) {
            None => Status::Unsolved,
            Some(best) if best <= exercise.par() => Status::AtPar,
            Some(_) => Status::Solved,
        }
    }

    /// Records a solution of `id` with `keys` keys, and returns whether it is a new best.
    pub fn record(&mut self, id: &str, keys: usize) -> bool {
        match self.exercises.get_mut(id) {
            Some(entry) if entry.best <= keys => false,
            Some(entry) => {
                entry.best = keys;
                true
            }
            None => {
                self.exercises.insert(id.to_owned(), Entry { best: keys });
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(id: &str, teach: &str) -> Exercise {
        Exercise::for_test(id, teach)
    }

    #[test]
    fn record_keeps_the_minimum() {
        let mut progress = Progress::default();
        assert!(progress.record("a/01-x", 12));
        assert!(!progress.record("a/01-x", 12));
        assert!(!progress.record("a/01-x", 15));
        assert_eq!(progress.best("a/01-x"), Some(12));
        assert!(progress.record("a/01-x", 9));
        assert_eq!(progress.best("a/01-x"), Some(9));
        assert_eq!(progress.best("a/02-y"), None);
    }

    #[test]
    fn status_compares_best_to_par() {
        let mut progress = Progress::default();
        let ex = exercise("a/01-x", "wd"); // par 2
        assert_eq!(progress.status(&ex), Status::Unsolved);
        progress.record(&ex.id, 5);
        assert_eq!(progress.status(&ex), Status::Solved);
        progress.record(&ex.id, 3);
        assert_eq!(progress.status(&ex), Status::Solved);
        progress.record(&ex.id, 2);
        assert_eq!(progress.status(&ex), Status::AtPar);
        progress.record(&ex.id, 1);
        assert_eq!(progress.status(&ex), Status::AtPar);
    }

    #[test]
    fn round_trip_and_file_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deeper").join("practice.toml");
        assert_eq!(Progress::load(&path).unwrap(), Progress::default());

        let mut progress = Progress::default();
        progress.record("motions/01-word-motions", 12);
        progress.save(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[exercises.\"motions/01-word-motions\"]\nbest = 12\n"
        );
        assert_eq!(Progress::load(&path).unwrap(), progress);
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn a_corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("practice.toml");
        std::fs::write(&path, "[exercises.\"a\"]\nbest = \"many\"\n").unwrap();
        let err = Progress::load(&path).unwrap_err().to_string();
        assert!(err.contains("practice.toml"), "{err}");
        // and it is left alone
        assert!(std::fs::read_to_string(&path).unwrap().contains("many"));
    }
}
