//! A generator of big git repositories for performance tests.
//!
//! The history is written as a `git fast-import` stream, so thousands of commits take a fraction
//! of a second instead of the minutes a `git commit` each would: a repository of 3,000 commits is
//! made in well under a second, 50,000 in a few seconds. Everything is deterministic: the same
//! [`RepoSpec`] always gives the same commits (same ids, since authors and dates are fixed).
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

/// The shape of a generated repository.
///
/// The history is a straight line on the branch `main`. Every commit changes exactly one file:
/// either the tracked file (every `touch_every`th commit, from `tracked_from` on) or one of the
/// "noise" files, which the other commits go through in turn.
pub struct RepoSpec {
    /// Total number of commits on `main`.
    pub commits: usize,
    /// Number of noise files, besides the tracked one, cycled through by the other commits.
    /// They live in a few directories (`noise/a/file17.txt`), so the trees have depth.
    pub files: usize,
    /// Path of the tracked file, relative to the repository, e.g. `src/tracked.rs`.
    pub tracked_path: &'static str,
    /// Number of lines of the tracked file when it is created.
    pub tracked_lines: usize,
    /// Every `touch_every`th commit (counted from the one that creates the file) edits the
    /// tracked file, at a line that moves around: some edits insert a line, so the lines below
    /// shift. Must not be 0.
    pub touch_every: usize,
    /// The index of the commit (0 = the first) that creates the tracked file; the commits before
    /// it only touch noise files. Must be less than `commits`.
    pub tracked_from: usize,
}

/// A generated repository, deleted when this is dropped.
pub struct GeneratedRepo {
    pub dir: TempDir,
    /// The tracked file, absolute (the directory is canonicalized).
    pub tracked: PathBuf,
    /// Number of commits that changed the tracked file, the one that created it included.
    pub touches: usize,
}

/// Timestamp of the first commit (2000-01-01T00:00:00Z); each commit is a minute after the last.
const START: u64 = 946_684_800;

/// Runs `git` in `dir` with the configuration of the user and the system out of the way.
fn git(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env("GIT_TERMINAL_PROMPT", "false")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_COUNT", "3")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "init.defaultBranch")
        .env("GIT_CONFIG_VALUE_1", "main")
        .env("GIT_CONFIG_KEY_2", "core.autocrlf")
        .env("GIT_CONFIG_VALUE_2", "false");
    command
}

fn run(dir: &Path, args: &[&str]) {
    let output = git(dir, args).output().expect("git can be run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Adds a commit of one file to the `fast-import` stream.
fn commit(stream: &mut Vec<u8>, index: usize, path: &str, contents: &str) {
    let message = format!("commit {index}: {path}");
    let who = format!(
        "Jane Doe <jane@example.com> {} +0000",
        START + index as u64 * 60
    );
    write!(
        stream,
        "commit refs/heads/main\nauthor {who}\ncommitter {who}\ndata {}\n{message}\n",
        message.len()
    )
    .unwrap();
    write!(
        stream,
        "M 100644 inline {path}\ndata {}\n{contents}\n",
        contents.len()
    )
    .unwrap();
}

/// Generates the repository of `spec`, with its working tree checked out.
pub fn generate(spec: &RepoSpec) -> GeneratedRepo {
    assert!(spec.touch_every > 0, "touch_every must not be 0");
    assert!(
        spec.tracked_from < spec.commits,
        "the tracked file is never created"
    );

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    run(&root, &["init", "-q"]);

    let files = spec.files.max(1);
    let mut tracked: Vec<String> = (0..spec.tracked_lines.max(1))
        .map(|i| format!("pub fn function_{i}() -> usize {{ {i} }}"))
        .collect();
    let mut stream = Vec::new();
    let mut touches = 0;
    let mut noise = 0;
    for index in 0..spec.commits {
        let since = index.checked_sub(spec.tracked_from);
        match since {
            Some(0) => {}
            Some(since) if since % spec.touch_every == 0 => {
                // an edit at a line that wanders about the file; every third one inserts a line
                let at = (touches * 7 + 3) % tracked.len();
                if touches % 3 == 0 {
                    tracked.insert(at, format!("// inserted by commit {index}"));
                } else {
                    tracked[at] = format!("pub fn edited_{index}() -> usize {{ {index} }}");
                }
            }
            _ => {
                let path = format!(
                    "noise/{}/file{}.txt",
                    ["a", "b", "c", "d"][noise % files % 4],
                    noise % files
                );
                commit(&mut stream, index, &path, &format!("noise {index}\n"));
                noise += 1;
                continue;
            }
        }
        touches += 1;
        let mut contents = tracked.join("\n");
        contents.push('\n');
        commit(&mut stream, index, spec.tracked_path, &contents);
    }

    // the pack is thrown away with the repository: don't spend time compressing it
    let mut import = git(
        &root,
        &["-c", "pack.compression=0", "fast-import", "--quiet"],
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .expect("git fast-import can be run");
    import.stdin.take().unwrap().write_all(&stream).unwrap();
    let output = import.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git fast-import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // HEAD is the unborn `main`: this puts the files of the last commit in the working tree
    run(&root, &["reset", "-q", "--hard", "main"]);

    GeneratedRepo {
        tracked: root.join(spec.tracked_path),
        dir,
        touches,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn generates_a_repository_fast() {
    let start = std::time::Instant::now();
    let repo = generate(&RepoSpec {
        commits: 3_000,
        files: 40,
        tracked_path: "src/tracked.rs",
        tracked_lines: 200,
        touch_every: 10,
        tracked_from: 5,
    });
    let took = start.elapsed();
    println!("3,000 commits in {took:?}");
    assert!(took.as_secs() < 10, "took {took:?}");

    let count = git(repo.dir.path(), &["rev-list", "--count", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "3000");
    assert!(repo.tracked.is_file());
    // the creation and then one edit in ten: 5, 15, 25, ... 2995
    assert_eq!(repo.touches, 300);
    let log = git(
        repo.dir.path(),
        &["rev-list", "--count", "HEAD", "--", "src/tracked.rs"],
    )
    .output()
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "300");
    assert!(repo.dir.path().join("noise").is_dir());
    // the working tree is clean
    let status = git(repo.dir.path(), &["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(status.stdout.is_empty());
}
