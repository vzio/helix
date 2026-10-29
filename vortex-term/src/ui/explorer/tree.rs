//! The directories of the tree: what is in them, read once and kept, and which entries git ignores.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::gitignore::{DirRules, Matcher, Outer};

#[derive(Clone, Debug)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    /// Ignored by git, directly or because a parent directory is.
    pub ignored: bool,
}

type Listings = HashMap<PathBuf, Listing>;

/// The entries of a directory, as listed at `stamp`.
struct Listing {
    stamp: Stamp,
    listed_at: SystemTime,
    /// Whether the directory itself was ignored, which makes all its entries ignored.
    dir_ignored: bool,
    /// The directory's own ignore rules, for listing the directories below it.
    rules: Arc<DirRules>,
    entries: Vec<Node>,
}

/// Modification times of a directory and of its `.gitignore`. The first changes when entries are
/// added, removed or renamed, the second when the ignore rules of the directory change.
type Stamp = (Option<SystemTime>, Option<SystemTime>);

fn stamp(dir: &Path) -> Stamp {
    let modified = |path: &Path| path.metadata().and_then(|meta| meta.modified()).ok();
    (modified(dir), modified(&dir.join(".gitignore")))
}

/// Modification times are coarse (a kernel tick on Linux), so a change made right after a listing
/// can keep the same time. Like git's "racy" entries, a listing made this soon after a change is
/// never trusted.
const RACY: Duration = Duration::from_secs(2);

impl Listing {
    fn is_current(&self, dir: &Path) -> bool {
        let (dir_time, ignore_time) = self.stamp;
        let racy = [dir_time, ignore_time]
            .into_iter()
            .flatten()
            .any(|time| time + RACY >= self.listed_at);
        !racy && stamp(dir) == self.stamp
    }
}

/// Drops the listings of the directories that changed on disk, with the ones below them: a
/// `.gitignore` applies to the whole subtree.
fn drop_changed_listings(listings: &mut Listings) {
    let changed: Vec<PathBuf> = listings
        .iter()
        .filter(|(dir, listing)| !listing.is_current(dir))
        .map(|(dir, _)| dir.clone())
        .collect();
    if !changed.is_empty() {
        listings.retain(|dir, _| !changed.iter().any(|changed| dir.starts_with(changed)));
    }
}

/// Reads directories into `listings`, with the ignore rules of the tree rooted at `root`.
struct Lister<'a> {
    root: &'a Path,
    outer: &'a Outer,
    listings: &'a mut Listings,
}

impl Lister<'_> {
    /// The visible tree: the entries of the root, and of the `expanded` directories below them.
    fn tree(&mut self, expanded: &HashSet<PathBuf>, hide_ignored: bool) -> Vec<Node> {
        let mut nodes = Vec::new();
        let root = self.root;
        self.push_children(root, 0, false, expanded, hide_ignored, &mut nodes);
        nodes
    }

    fn push_children(
        &mut self,
        dir: &Path,
        depth: usize,
        dir_ignored: bool,
        expanded: &HashSet<PathBuf>,
        hide_ignored: bool,
        nodes: &mut Vec<Node>,
    ) {
        let entries: Vec<Node> = self
            .listing(dir, dir_ignored)
            .entries
            .iter()
            .filter(|entry| !(hide_ignored && entry.ignored))
            .cloned()
            .collect();
        for mut node in entries {
            node.depth = depth;
            let path = node.path.clone();
            let ignored = node.ignored;
            let expand = node.is_dir && expanded.contains(&path);
            nodes.push(node);
            if expand {
                self.push_children(&path, depth + 1, ignored, expanded, hide_ignored, nodes);
            }
        }
    }

    /// The listing of `dir`, read from disk unless it is already there. The directories above it
    /// up to the root are expected to be listed already, for their ignore rules.
    fn listing(&mut self, dir: &Path, dir_ignored: bool) -> &Listing {
        let listed = self
            .listings
            .get(dir)
            .is_some_and(|listing| listing.dir_ignored == dir_ignored);
        if !listed {
            let stamp = stamp(dir);
            let listed_at = SystemTime::now();
            // Everything inside an ignored directory is ignored, its rules don't matter.
            let (rules, entries) = if dir_ignored {
                (Arc::default(), list_dir(dir, None))
            } else {
                let rules = DirRules::read(dir);
                let mut chain = vec![rules.clone()];
                chain.extend(
                    dir.ancestors()
                        .skip(1)
                        .take_while(|parent| parent.starts_with(self.root))
                        .map(|parent| match self.listings.get(parent) {
                            Some(listing) => listing.rules.clone(),
                            None => DirRules::read(parent),
                        }),
                );
                let entries = list_dir(dir, Some(&Matcher::new(&chain, self.outer)));
                (rules, entries)
            };
            let listing = Listing {
                stamp,
                listed_at,
                dir_ignored,
                rules,
                entries,
            };
            self.listings.insert(dir.to_path_buf(), listing);
        }
        &self.listings[dir]
    }
}

/// The tree of a workspace: the directories that are expanded, and the listings of the ones
/// shown.
pub struct Tree {
    pub root: PathBuf,
    /// Git's ignore rules from outside the tree.
    outer: Outer,
    /// Listed directories, so expanding or collapsing one doesn't read the others again.
    listings: Listings,
    pub expanded: HashSet<PathBuf>,
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        Self {
            outer: Outer::read(&root),
            root,
            listings: HashMap::new(),
            expanded: HashSet::new(),
        }
    }

    /// Reads again the directories that changed on disk since they were listed.
    pub fn refresh(&mut self) {
        drop_changed_listings(&mut self.listings);
    }

    /// Reads every directory again, for the changes `refresh` can't see (global git excludes,
    /// coarse modification times).
    pub fn reload(&mut self) {
        self.outer = Outer::read(&self.root);
        self.listings.clear();
    }

    /// The visible tree, flattened in display order: the entries of the root, and of the
    /// expanded directories below them.
    pub fn nodes(&mut self, hide_ignored: bool) -> Vec<Node> {
        self.expanded.retain(|dir| dir.is_dir());
        // Collapsed directories are listed again when expanded, instead of being checked for
        // changes on every refresh.
        self.listings
            .retain(|dir, _| *dir == self.root || self.expanded.contains(dir));
        let mut lister = Lister {
            root: &self.root,
            outer: &self.outer,
            listings: &mut self.listings,
        };
        lister.tree(&self.expanded, hide_ignored)
    }

    /// Expands the parent directories of `path`. Without `into_ignored`, a path inside a
    /// directory ignored by git is left alone.
    pub fn reveal(&mut self, path: &Path, into_ignored: bool) {
        let Some(parent) = path.parent() else {
            return;
        };
        let Ok(relative) = parent.strip_prefix(&self.root) else {
            return;
        };
        let mut lister = Lister {
            root: &self.root,
            outer: &self.outer,
            listings: &mut self.listings,
        };
        let mut dirs = Vec::new();
        let mut dir = self.root.clone();
        let mut dir_ignored = false;
        for component in relative.components() {
            let child = dir.join(component);
            let entry = lister
                .listing(&dir, dir_ignored)
                .entries
                .iter()
                .find(|entry| entry.path == child);
            // Not listed: gone, or inside `.git`.
            let Some(entry) = entry else {
                return;
            };
            if entry.ignored && !into_ignored {
                return;
            }
            dir_ignored = entry.ignored;
            dirs.push(child.clone());
            dir = child;
        }
        self.expanded.extend(dirs);
    }

    /// Lists `dir` again, like expanding it. The directories above it are expected to be listed.
    #[cfg(feature = "bench")]
    pub fn bench_list(&mut self, dir: &Path) -> usize {
        self.listings.remove(dir);
        let mut lister = Lister {
            root: &self.root,
            outer: &self.outer,
            listings: &mut self.listings,
        };
        lister.listing(dir, false).entries.len()
    }
}

/// Entries of `dir`, directories first. Without a `matcher`, the directory is ignored by git, and
/// so is everything inside it.
fn list_dir(dir: &Path, matcher: Option<&Matcher>) -> Vec<Node> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut nodes: Vec<Node> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != ".git")
        .map(|entry| {
            let path = entry.path();
            // Only symlinks need another stat to know where they lead.
            let is_dir = match entry.file_type() {
                Ok(file_type) if !file_type.is_symlink() => file_type.is_dir(),
                _ => path.is_dir(),
            };
            let ignored = matcher.is_none_or(|matcher| matcher.is_ignored(&path, is_dir));
            Node {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
                ignored,
                depth: 0,
                path,
            }
        })
        .collect();
    // Lowercasing once per entry, not once per comparison.
    nodes.sort_by_cached_key(|node| (!node.is_dir, node.name.to_lowercase()));
    nodes
}

#[cfg(test)]
pub(super) fn test_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
    std::fs::create_dir_all(root.join("src/nested")).unwrap();
    std::fs::write(root.join("src/main.rs"), "").unwrap();
    std::fs::write(root.join("src/nested/.gitignore"), "secret.txt\n").unwrap();
    std::fs::write(root.join("src/nested/secret.txt"), "").unwrap();
    std::fs::write(root.join("src/nested/public.txt"), "").unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::write(root.join("target/debug/app"), "").unwrap();
    std::fs::write(root.join("build.log"), "").unwrap();
    std::fs::write(root.join("README.md"), "").unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tree(
        root: &Path,
        expanded: &HashSet<PathBuf>,
        hide_ignored: bool,
        listings: &mut Listings,
    ) -> Vec<Node> {
        let outer = Outer::read(root);
        let mut lister = Lister {
            root,
            outer: &outer,
            listings,
        };
        lister.tree(expanded, hide_ignored)
    }

    /// The ignored entries of `dir`, the way the `ignore` crate tells them.
    fn ignored_by_walk(dir: &Path) -> HashSet<PathBuf> {
        let not_ignored: HashSet<PathBuf> = ignore::WalkBuilder::new(dir)
            .max_depth(Some(1))
            .hidden(false)
            .ignore(false)
            .parents(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build()
            .filter_map(Result::ok)
            .filter(|entry| entry.depth() == 1)
            .map(|entry| entry.into_path())
            .collect();
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| !not_ignored.contains(path) && !path.ends_with(".git"))
            .collect()
    }

    #[test]
    fn ignores_what_the_ignore_crate_ignores() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let write = |path: &str, contents: &str| {
            let path = repo.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        };
        write(".git/info/exclude", "excluded.txt\n");
        write(".gitignore", "*.log\n/build\nonly_dir/\n!keep.log\n");
        write("a.log", "");
        write("keep.log", "");
        write("excluded.txt", "");
        write("build/out", "");
        write("only_dir", "");
        write("src/build/out", "");
        write("src/only_dir/x", "");
        write("src/.gitignore", "!b.log\ngenerated.rs\n");
        write("src/b.log", "");
        write("src/c.log", "");
        write("src/generated.rs", "");
        write("src/main.rs", "");
        write("src/deep/.gitignore", "*.rs\n");
        write("src/deep/lib.rs", "");
        write("src/deep/d.log", "");
        // A repository of its own: the outer rules stop at it.
        write("nested/.git/info/exclude", "");
        write("nested/a.log", "");
        write("nested/generated.rs", "");

        // From the root of the repository, and from a directory below it.
        for root in [repo.clone(), repo.join("src")] {
            let expanded: HashSet<PathBuf> =
                ["src", "src/build", "src/only_dir", "src/deep", "nested"]
                    .iter()
                    .map(|path| repo.join(path))
                    .filter(|path| path.starts_with(&root))
                    .collect();
            let mut listings = HashMap::new();
            build_tree(&root, &expanded, false, &mut listings);
            for dir in std::iter::once(&root).chain(&expanded) {
                let listing = &listings[dir];
                if listing.dir_ignored {
                    continue;
                }
                let ignored: HashSet<PathBuf> = listing
                    .entries
                    .iter()
                    .filter(|node| node.ignored)
                    .map(|node| node.path.clone())
                    .collect();
                assert_eq!(ignored, ignored_by_walk(dir), "in {}", dir.display());
            }
        }
    }

    fn names(nodes: &[Node]) -> Vec<(String, bool)> {
        nodes
            .iter()
            .map(|node| {
                let indent = "  ".repeat(node.depth);
                (format!("{indent}{}", node.name), node.ignored)
            })
            .collect()
    }

    use super::test_repo as repo;

    #[test]
    fn lists_directories_first_and_dims_ignored_files() {
        let dir = repo();
        let root = dir.path();
        let expanded: HashSet<PathBuf> = ["src", "src/nested", "target"]
            .iter()
            .map(|path| root.join(path))
            .collect();
        let tree = build_tree(root, &expanded, false, &mut HashMap::new());
        let expected = [
            ("src", false),
            ("  nested", false),
            ("    .gitignore", false),
            ("    public.txt", false),
            ("    secret.txt", true),
            ("  main.rs", false),
            ("target", true),
            ("  debug", true),
            (".gitignore", false),
            ("build.log", true),
            ("README.md", false),
        ];
        let expected: Vec<(String, bool)> = expected
            .iter()
            .map(|(name, ignored)| (name.to_string(), *ignored))
            .collect();
        assert_eq!(names(&tree), expected);
    }

    #[test]
    fn hides_ignored_files() {
        let dir = repo();
        let root = dir.path();
        let expanded: HashSet<PathBuf> = [root.join("src"), root.join("src/nested")].into();
        let tree = build_tree(root, &expanded, true, &mut HashMap::new());
        let names: Vec<String> = names(&tree).into_iter().map(|(name, _)| name).collect();
        assert_eq!(
            names,
            [
                "src",
                "  nested",
                "    .gitignore",
                "    public.txt",
                "  main.rs",
                ".gitignore",
                "README.md"
            ]
        );
    }

    #[test]
    fn lists_again_only_what_changed() {
        let dir = repo();
        let root = dir.path();
        let expanded: HashSet<PathBuf> = [root.join("src")].into();
        let mut listings = HashMap::new();
        let names_of = |listings: &mut Listings| -> Vec<(String, bool)> {
            names(&build_tree(root, &expanded, false, listings))
        };
        names_of(&mut listings);

        // Unchanged directories come from the listings.
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
        assert!(!names_of(&mut listings).contains(&("  lib.rs".into(), false)));
        // The directory's modification time changed, so it is listed again.
        drop_changed_listings(&mut listings);
        assert!(names_of(&mut listings).contains(&("  lib.rs".into(), false)));

        // A changed `.gitignore` lists its directory and everything below it again.
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n*.rs\n").unwrap();
        drop_changed_listings(&mut listings);
        let tree = names_of(&mut listings);
        assert!(tree.contains(&("  lib.rs".into(), true)));
        assert!(tree.contains(&("  main.rs".into(), true)));
    }

    /// Opens `dir` so that its modification time can be changed. Windows only opens a directory
    /// with `FILE_FLAG_BACKUP_SEMANTICS`, and needs write access to change its times.
    fn open_dir(dir: &Path) -> std::fs::File {
        let mut options = std::fs::OpenOptions::new();
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            options.write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        }
        #[cfg(not(windows))]
        options.read(true);
        options.open(dir).unwrap()
    }

    #[test]
    fn listing_is_current_until_the_directory_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let an_hour_ago = SystemTime::now() - Duration::from_secs(3600);
        open_dir(root).set_modified(an_hour_ago).unwrap();
        let mut listings = HashMap::new();
        build_tree(root, &HashSet::new(), false, &mut listings);
        assert!(listings[root].is_current(root));

        std::fs::write(root.join("new.txt"), "").unwrap();
        assert!(!listings[root].is_current(root));
    }

    #[test]
    fn collapsed_directories_are_not_kept() {
        let dir = repo();
        let root = dir.path();
        let mut tree = Tree::new(root.to_path_buf());
        tree.reveal(&root.join("src/nested/public.txt"), true);
        tree.nodes(false);
        assert!(tree.listings.contains_key(&root.join("src/nested")));

        tree.expanded.remove(&root.join("src"));
        tree.expanded.remove(&root.join("src/nested"));
        tree.nodes(false);
        assert_eq!(tree.listings.keys().collect::<Vec<_>>(), [root]);
    }

    #[test]
    fn reveal_expands_the_parents_but_not_into_ignored_directories() {
        let dir = repo();
        let root = dir.path();
        let mut tree = Tree::new(root.to_path_buf());
        tree.reveal(&root.join("target/debug/app"), false);
        assert!(tree.expanded.is_empty());
        tree.reveal(&root.join("target/debug/app"), true);
        assert!(tree.expanded.contains(&root.join("target/debug")));
        tree.reveal(&root.join("src/nested/public.txt"), false);
        assert!(tree.expanded.contains(&root.join("src/nested")));
        // Gone or inside `.git`: left alone.
        let before = tree.expanded.len();
        tree.reveal(&root.join("missing/file"), true);
        tree.reveal(&root.join(".git/config"), true);
        assert_eq!(tree.expanded.len(), before);
    }
}
