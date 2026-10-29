//! Filtering the tree: the whole workspace is walked once in the background, then each query is
//! matched against the paths found, fuzzy like the file picker does.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};

use super::row::Entry;
use super::tree::Node;

/// A file or directory of the workspace.
pub struct Found {
    /// Relative to the root of the tree.
    pub path: PathBuf,
    pub is_dir: bool,
}

/// A walk of the workspace on another thread. The walk stops when this is dropped.
pub struct Walk {
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<Vec<Found>>>>,
}

impl Walk {
    pub fn start(root: &Path) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let slot = Arc::new(Mutex::new(None));
        let (root, thread_cancel, thread_slot) = (root.to_path_buf(), cancel.clone(), slot.clone());
        let work = move || {
            let found = walk(&root, &thread_cancel);
            if !thread_cancel.load(Ordering::Relaxed) {
                *thread_slot.lock().unwrap() = Some(found);
                helix_event::request_redraw();
            }
        };
        // On the blocking threads of the runtime when there is one, which the redraw request
        // needs in tests.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(work);
            }
            Err(_) => {
                std::thread::spawn(work);
            }
        }
        Self { cancel, slot }
    }

    /// The result, once the walk is over.
    pub fn take(&self) -> Option<Vec<Found>> {
        self.slot.lock().unwrap().take()
    }
}

impl Drop for Walk {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Every file and directory below `root` that git doesn't ignore, hidden ones included, `.git`
/// excluded. Directories ignored by git are not entered.
pub fn walk(root: &Path, cancel: &AtomicBool) -> Vec<Found> {
    let walk = ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .parents(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    let mut found = Vec::new();
    for entry in walk.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(path) = entry.path().strip_prefix(root) else {
            continue;
        };
        if path.as_os_str().is_empty() {
            continue;
        }
        // A symlink to a directory is a directory in the tree, but is not entered.
        let is_dir = match entry.file_type() {
            Some(file_type) if !file_type.is_symlink() => file_type.is_dir(),
            _ => entry.path().is_dir(),
        };
        found.push(Found {
            path: path.to_path_buf(),
            is_dir,
        });
    }
    found
}

/// An entry that matches the query.
pub struct Hit {
    /// Relative to the root of the tree.
    pub path: PathBuf,
    pub is_dir: bool,
    pub score: u32,
    /// Positions (in characters) in the path of the characters matched.
    pub indices: Vec<u32>,
}

/// The entries of `found` that match `query`, in the order they were found.
pub fn search(found: &[Found], query: &str) -> Vec<Hit> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buffer = Vec::new();
    let mut indices = Vec::new();
    found
        .iter()
        .filter_map(|entry| {
            let text = entry.path.to_string_lossy();
            let haystack = Utf32Str::new(&text, &mut buffer);
            indices.clear();
            let score = pattern.indices(haystack, &mut matcher, &mut indices)?;
            indices.sort_unstable();
            indices.dedup();
            Some(Hit {
                path: entry.path.clone(),
                is_dir: entry.is_dir,
                score,
                indices: indices.clone(),
            })
        })
        .collect()
}

/// The tree of the `hits`: each of them with the directories above it, in tree order (directories
/// first, then by name regardless of case). Directories are expanded, except the `collapsed`
/// ones, and the ones holding no hit. Returns the index of the entry with the best score too.
pub fn tree(
    root: &Path,
    hits: &[Hit],
    collapsed: &HashSet<PathBuf>,
) -> (Vec<Entry>, Option<usize>) {
    // Every path to show with whether it is a directory and the hit it is, by path.
    let mut shown: HashMap<&Path, (bool, Option<&Hit>)> = HashMap::new();
    for hit in hits {
        shown.insert(&hit.path, (hit.is_dir, Some(hit)));
    }
    for hit in hits {
        for ancestor in hit.path.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            shown.entry(ancestor).or_insert((true, None));
        }
    }
    let mut children: HashMap<&Path, Vec<&Path>> = HashMap::new();
    for path in shown.keys() {
        children
            .entry(path.parent().unwrap_or(Path::new("")))
            .or_default()
            .push(path);
    }
    let name = |path: &Path| {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    for siblings in children.values_mut() {
        siblings.sort_by_cached_key(|path| (!shown[path].0, name(path).to_lowercase()));
    }

    struct Walker<'a> {
        root: &'a Path,
        shown: &'a HashMap<&'a Path, (bool, Option<&'a Hit>)>,
        children: &'a HashMap<&'a Path, Vec<&'a Path>>,
        collapsed: &'a HashSet<PathBuf>,
        entries: Vec<Entry>,
        best: Option<(u32, usize)>,
    }

    impl Walker<'_> {
        fn push(&mut self, dir: &Path, depth: usize) {
            let Some(children) = self.children.get(dir) else {
                return;
            };
            for &path in children {
                let (is_dir, hit) = self.shown[path];
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let absolute = self.root.join(path);
                let has_children = self.children.contains_key(path);
                let expanded = is_dir && has_children && !self.collapsed.contains(&absolute);
                let mut entry = Entry::new(
                    Node {
                        name: name.to_string(),
                        path: absolute,
                        depth,
                        is_dir,
                        ignored: false,
                    },
                    expanded,
                );
                if let Some(hit) = hit {
                    // Positions in the path to positions in the name.
                    let start = path.to_string_lossy().chars().count() - name.chars().count();
                    entry.matched = hit
                        .indices
                        .iter()
                        .filter_map(|index| index.checked_sub(start as u32))
                        .collect();
                    // The first of equally good hits wins.
                    if self.best.is_none_or(|(score, _)| hit.score > score) {
                        self.best = Some((hit.score, self.entries.len()));
                    }
                }
                self.entries.push(entry);
                if expanded {
                    self.push(path, depth + 1);
                }
            }
        }
    }

    let mut walker = Walker {
        root,
        shown: &shown,
        children: &children,
        collapsed,
        entries: Vec::new(),
        best: None,
    };
    walker.push(Path::new(""), 0);
    let best = walker.best.map(|(_, index)| index);
    (walker.entries, best)
}
