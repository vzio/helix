//! The languages of a repository for the dashboard: the lines (newlines) of the files git
//! tracks, grouped by the editor's own language detection (the file types of `languages.toml`).
//! Binary files (the hex view's check) and lockfiles (`[editor.changes] lockfiles`) are skipped,
//! files of no known language count as "other".
//!
//! The files are read by a few threads at once, which add what they counted to the shared
//! [`Counts`] as they go, so the dashboard can draw the bar filling in.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use vortex_core::syntax::Loader;

/// The name of the group of files of no known language, and of the languages too small to have
/// a segment of their own in the bar.
pub const OTHER: &str = "other";

/// What has been counted so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// The files to read, once git listed them.
    pub files: Option<usize>,
    /// The files read (or skipped) so far.
    pub done: usize,
    /// Lines by language id (`rust`, `toml`) or [`OTHER`].
    pub lines: HashMap<String, u64>,
    /// The files that couldn't be read.
    pub unreadable: usize,
    /// Whether every file has been read.
    pub finished: bool,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.lines.values().sum()
    }

    /// The share of the files read so far, from 0 to 1.
    pub fn progress(&self) -> f64 {
        match self.files {
            _ if self.finished => 1.0,
            Some(0) => 1.0,
            Some(files) => self.done as f64 / files as f64,
            None => 0.0,
        }
    }
}

/// How a tracked file counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum File {
    Lines(u64),
    Binary,
    Unreadable,
}

/// The lines of the file at `path` (its newlines), read in chunks so a large file isn't held in
/// memory, or that it is binary, judged on its first chunk.
pub fn count_file(path: &Path) -> File {
    let Ok(mut file) = std::fs::File::open(path) else {
        return File::Unreadable;
    };
    let mut buffer = vec![0u8; 64 * 1024];
    let mut lines = 0u64;
    let mut first = true;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return File::Unreadable,
        };
        let chunk = &buffer[..read];
        if first {
            if vortex_view::hex::is_binary(chunk) {
                return File::Binary;
            }
            first = false;
        }
        lines += chunk.iter().filter(|&&byte| byte == b'\n').count() as u64;
    }
    File::Lines(lines)
}

/// The language id of `path` by its name, or [`OTHER`].
pub fn language_of(loader: &Loader, path: &Path) -> String {
    loader
        .language_for_filename(path)
        .map(|language| loader.language(language).config().language_id.clone())
        .unwrap_or_else(|| OTHER.to_string())
}

/// Counts the lines of `files` into `counts`, with up to `threads` threads, skipping the
/// lockfiles. Returns when every file has been read, or soon after `cancelled` says so.
pub fn count(
    files: Vec<PathBuf>,
    loader: &Loader,
    is_lockfile: impl Fn(&Path) -> bool,
    threads: usize,
    counts: &Mutex<Counts>,
    cancelled: impl Fn() -> bool + Sync,
) {
    let files: Vec<PathBuf> = files
        .into_iter()
        .filter(|file| !is_lockfile(file))
        .collect();
    {
        let mut counts = counts.lock().unwrap();
        counts.files = Some(files.len());
        counts.done = 0;
    }
    let next = AtomicUsize::new(0);
    // a thread adds what it counted every so many files
    const BATCH: usize = 64;
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| {
                let mut lines: HashMap<String, u64> = HashMap::new();
                let mut done = 0;
                let mut unreadable = 0;
                let flush =
                    |lines: &mut HashMap<String, u64>, done: &mut usize, unreadable: &mut usize| {
                        let mut counts = counts.lock().unwrap();
                        for (language, n) in lines.drain() {
                            *counts.lines.entry(language).or_default() += n;
                        }
                        counts.done += std::mem::take(done);
                        counts.unreadable += std::mem::take(unreadable);
                    };
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(index).filter(|_| !cancelled()) else {
                        break;
                    };
                    match count_file(path) {
                        File::Lines(n) => *lines.entry(language_of(loader, path)).or_default() += n,
                        File::Binary => {}
                        File::Unreadable => unreadable += 1,
                    }
                    done += 1;
                    if done == BATCH {
                        flush(&mut lines, &mut done, &mut unreadable);
                    }
                }
                flush(&mut lines, &mut done, &mut unreadable);
            });
        }
    });
    counts.lock().unwrap().finished = true;
}

/// The name shown for a language id: the usual spelling of the common ones, else the id with a
/// capital.
pub fn display_name(id: &str) -> String {
    let known = match id {
        "bash" => "Bash",
        "c" => "C",
        "c-sharp" => "C#",
        "cpp" => "C++",
        "css" => "CSS",
        "dockerfile" => "Dockerfile",
        "elixir" => "Elixir",
        "git-attributes" => "Git attributes",
        "git-config" => "Git config",
        "git-ignore" => "Gitignore",
        "go" => "Go",
        "html" => "HTML",
        "java" => "Java",
        "javascript" => "JavaScript",
        "json" => "JSON",
        "jsonc" => "JSONC",
        "jsx" => "JSX",
        "kotlin" => "Kotlin",
        "lua" => "Lua",
        "make" => "Make",
        "markdown" => "Markdown",
        "nix" => "Nix",
        "php" => "PHP",
        "python" => "Python",
        "ruby" => "Ruby",
        "rust" => "Rust",
        "scheme" => "Scheme",
        "scss" => "SCSS",
        "sql" => "SQL",
        "swift" => "Swift",
        "toml" => "TOML",
        "tsq" => "Tree-sitter",
        "tsx" => "TSX",
        "typescript" => "TypeScript",
        "xml" => "XML",
        "yaml" => "YAML",
        "zig" => "Zig",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first
            .to_uppercase()
            .chain(chars)
            .collect::<String>()
            .replace('-', " "),
        None => String::new(),
    }
}

/// A segment of the bar: a language (or [`OTHER`]) and its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub language: String,
    pub lines: u64,
}

/// The languages with a segment of their own, the most lines first: at most `max` of them, each
/// with at least a 200th of the lines; the rest, with the files of no known language, make the
/// last segment, [`OTHER`], when there is any.
pub fn shares(lines: &HashMap<String, u64>, max: usize) -> Vec<Share> {
    let total: u64 = lines.values().sum();
    let mut languages: Vec<Share> = lines
        .iter()
        .filter(|(language, &n)| language.as_str() != OTHER && n > 0)
        .map(|(language, &n)| Share {
            language: language.clone(),
            lines: n,
        })
        .collect();
    // ties by name, so the order doesn't change between frames
    languages.sort_by(|a, b| {
        b.lines
            .cmp(&a.lines)
            .then_with(|| a.language.cmp(&b.language))
    });
    let shown = languages
        .iter()
        .take(max)
        .take_while(|share| share.lines * 200 >= total)
        .count();
    let rest: u64 = languages[shown..]
        .iter()
        .map(|share| share.lines)
        .sum::<u64>()
        + lines.get(OTHER).copied().unwrap_or(0);
    languages.truncate(shown);
    if rest > 0 {
        languages.push(Share {
            language: OTHER.to_string(),
            lines: rest,
        });
    }
    languages
}

/// The width in cells of each share in a bar `width` cells wide (largest remainders), each at
/// least one cell when there is room.
pub fn widths(shares: &[Share], width: u16) -> Vec<u16> {
    let total: u64 = shares.iter().map(|share| share.lines).sum();
    if total == 0 || shares.is_empty() {
        return vec![0; shares.len()];
    }
    let width = width as u64;
    let exact: Vec<(u64, u64)> = shares
        .iter()
        .map(|share| (share.lines * width / total, share.lines * width % total))
        .collect();
    let mut cells: Vec<u64> = exact.iter().map(|&(cells, _)| cells).collect();
    let mut left = width - cells.iter().sum::<u64>();
    let mut by_remainder: Vec<usize> = (0..shares.len()).collect();
    by_remainder.sort_by(|&a, &b| exact[b].1.cmp(&exact[a].1).then(a.cmp(&b)));
    for &i in by_remainder.iter() {
        if left == 0 {
            break;
        }
        cells[i] += 1;
        left -= 1;
    }
    // every share visible: take a cell from the widest for each empty one
    if width >= shares.len() as u64 {
        while let Some(empty) = cells.iter().position(|&c| c == 0) {
            let widest = (0..cells.len()).max_by_key(|&i| cells[i]).unwrap();
            cells[widest] -= 1;
            cells[empty] += 1;
        }
    }
    cells.into_iter().map(|c| c as u16).collect()
}

/// `48210` as `48,210`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|&(l, n)| (l.to_string(), n)).collect()
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(48210), "48,210");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn names() {
        assert_eq!(display_name("rust"), "Rust");
        assert_eq!(display_name("toml"), "TOML");
        assert_eq!(display_name("git-commit"), "Git commit");
        assert_eq!(display_name("ocaml"), "Ocaml");
    }

    #[test]
    fn small_languages_go_to_other() {
        let shares = shares(
            &lines(&[("rust", 9000), ("toml", 500), ("make", 10), ("other", 300)]),
            5,
        );
        let names: Vec<_> = shares
            .iter()
            .map(|s| (s.language.as_str(), s.lines))
            .collect();
        // make is under a 200th of the lines
        assert_eq!(names, [("rust", 9000), ("toml", 500), ("other", 310)]);
    }

    #[test]
    fn at_most_max_segments() {
        let shares = shares(&lines(&[("a", 600), ("b", 500), ("c", 400), ("d", 300)]), 2);
        let names: Vec<_> = shares
            .iter()
            .map(|s| (s.language.as_str(), s.lines))
            .collect();
        assert_eq!(names, [("a", 600), ("b", 500), ("other", 700)]);
    }

    #[test]
    fn widths_add_up_and_show_everyone() {
        let shares = shares(&lines(&[("rust", 10000), ("toml", 60), ("other", 60)]), 5);
        let widths = widths(&shares, 40);
        assert_eq!(widths.iter().sum::<u16>(), 40);
        assert!(widths.iter().all(|&w| w >= 1), "{widths:?}");
        assert_eq!(super::widths(&[], 40), Vec::<u16>::new());
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git can be run");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn loader() -> Loader {
        Loader::new(
            vortex_loader::config::default_lang_config()
                .try_into()
                .unwrap(),
        )
        .unwrap()
    }

    /// A repository of two Rust files (3 and 5 lines), a `Cargo.toml` (4 lines), a lockfile, a
    /// binary file, a file of no known type (2 lines), and an untracked Rust file.
    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q"]);
        std::fs::create_dir(path.join("src")).unwrap();
        std::fs::write(path.join("src/a.rs"), "fn a() {}\n\nfn b() {}\n").unwrap();
        std::fs::write(
            path.join("src/b.rs"),
            "// one\n// two\nfn c() {\n}\n// five\n",
        )
        .unwrap();
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"1\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(path.join("Cargo.lock"), "# lock\n".repeat(50)).unwrap();
        std::fs::write(path.join("x.png"), b"\x89PNG\r\n\x1a\n\x00\x00\n\n\n").unwrap();
        std::fs::write(path.join("data.bin"), b"\x00\x01\n\x02\n").unwrap();
        std::fs::write(path.join("notes.zzz"), "some\nnotes\n").unwrap();
        git(path, &["add", "-A"]);
        git(path, &["commit", "-q", "-m", "files"]);
        std::fs::write(path.join("untracked.rs"), "fn u() {}\n".repeat(7)).unwrap();
        dir
    }

    #[test]
    fn counting_the_files_of_a_repository() {
        let dir = repository();
        let repo = vortex_vcs::summary::Repo::open(dir.path(), true)
            .unwrap()
            .expect("a repository");
        let files = repo.tracked_files().unwrap();
        // the untracked file isn't listed
        assert_eq!(files.len(), 7, "{files:?}");
        assert!(files.iter().all(|file| !file.ends_with("untracked.rs")));

        let loader = loader();
        let counts = Mutex::new(Counts::default());
        count(
            files,
            &loader,
            |path| vortex_view::editor::ChangesConfig::default().is_lockfile(path),
            2,
            &counts,
            || false,
        );
        let counts = counts.into_inner().unwrap();
        // 7 tracked, without the lockfile
        assert_eq!(counts.files, Some(6));
        assert_eq!(counts.done, 6);
        assert_eq!(counts.unreadable, 0);
        assert!(counts.finished);
        assert_eq!(counts.progress(), 1.0);
        // the binary files and the lockfile have no lines, the untracked file isn't there
        let mut lines: Vec<_> = counts.lines.iter().map(|(l, n)| (l.as_str(), *n)).collect();
        lines.sort();
        assert_eq!(lines, [("other", 2), ("rust", 8), ("toml", 4)]);
        assert_eq!(counts.total(), 14);
    }

    #[test]
    fn counting_stops_when_cancelled() {
        let dir = repository();
        let files: Vec<PathBuf> = ["src/a.rs", "src/b.rs"]
            .iter()
            .map(|file| dir.path().join(file))
            .collect();
        let counts = Mutex::new(Counts::default());
        count(files, &loader(), |_| false, 1, &counts, || true);
        let counts = counts.into_inner().unwrap();
        assert_eq!(counts.done, 0);
        assert_eq!(counts.total(), 0);
        assert!(counts.finished);
    }

    #[test]
    fn language_by_file_name() {
        let loader = loader();
        assert_eq!(language_of(&loader, Path::new("src/a.rs")), "rust");
        assert_eq!(language_of(&loader, Path::new("Cargo.toml")), "toml");
        assert_eq!(language_of(&loader, Path::new("notes.zzz")), OTHER);
    }

    #[test]
    fn counting_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("a.rs");
        std::fs::write(&text, "fn main() {}\n\nlet x;\n").unwrap();
        assert_eq!(count_file(&text), File::Lines(3));
        let binary = dir.path().join("b.bin");
        std::fs::write(&binary, b"\x00\x01\n\n").unwrap();
        assert_eq!(count_file(&binary), File::Binary);
        assert_eq!(count_file(&dir.path().join("missing")), File::Unreadable);
        // newlines past the first chunk count too
        let big = dir.path().join("big.txt");
        std::fs::write(&big, "x\n".repeat(100_000)).unwrap();
        assert_eq!(count_file(&big), File::Lines(100_000));
    }
}
