//! Reading and writing single entries of the git index.
//!
//! Reads go through gix. Writes use the git CLI (`hash-object` and `update-index`) because gix
//! can't write the index safely yet. Workspaces are untrusted by default, so the commands are
//! run with `core.fsmonitor` disabled and the contents are converted to git form in-process
//! (gix drops untrusted clean/smudge drivers under `Trust::Reduced`) and hashed with
//! `--no-filters`, so no repository configured program is ever run.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind;
use gix::{ObjectId, Repository};

use super::{open_repo, to_worktree};

/// The kind of file an index entry is, as far as staging cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Executable,
    Symlink,
    Submodule,
}

impl Kind {
    fn mode(self) -> &'static str {
        match self {
            Kind::File => "100644",
            Kind::Executable => "100755",
            Kind::Symlink => "120000",
            Kind::Submodule => "160000",
        }
    }

    fn from_index(mode: gix::index::entry::Mode) -> Self {
        use gix::index::entry::Mode;
        if mode.contains(Mode::SYMLINK) && !mode.contains(Mode::FILE) {
            Kind::Symlink
        } else if mode == Mode::COMMIT {
            Kind::Submodule
        } else if mode.contains(Mode::FILE_EXECUTABLE) && mode.contains(Mode::FILE) {
            Kind::Executable
        } else {
            Kind::File
        }
    }

    fn from_tree(kind: EntryKind) -> Result<Self> {
        Ok(match kind {
            EntryKind::Blob => Kind::File,
            EntryKind::BlobExecutable => Kind::Executable,
            EntryKind::Link => Kind::Symlink,
            EntryKind::Commit => Kind::Submodule,
            EntryKind::Tree => bail!("entry is a directory"),
        })
    }
}

/// The repository of `file`, and the path of `file` in the working tree (symlinks in its
/// directories resolved) and relative to it.
struct Target {
    repo: Repository,
    abs: PathBuf,
    rela: PathBuf,
}

impl Target {
    fn new(file: &Path, trust_full: bool) -> Result<Self> {
        debug_assert!(file.is_absolute());
        // Resolve the directory only: `file` may be deleted, and if it is a symlink it is the
        // symlink that is meant, not what it points to.
        let name = file.file_name().context("file has no name")?;
        let dir = file.parent().context("file has no parent directory")?;
        let abs = gix::path::realpath(dir)
            .context("resolve symlinks")?
            .join(name);
        let dir = abs.parent().context("file has no parent directory")?;
        let repo = open_repo(dir, trust_full)
            .context("failed to open git repo")?
            .to_thread_local();
        let work_dir = repo.workdir().context("repo has no worktree")?;
        let rela = abs
            .strip_prefix(work_dir)
            .with_context(|| format!("{} is not in the working tree", abs.display()))?
            .to_path_buf();
        Ok(Target { repo, abs, rela })
    }

    /// The path relative to the working tree, with `/` as separator.
    fn rela_str(&self) -> Result<String> {
        let bstr = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(&*self.rela));
        Ok(bstr
            .to_str()
            .map_err(|_| anyhow::anyhow!("path {} is not valid UTF-8", self.rela.display()))?
            .to_owned())
    }

    /// The kind and blob id of the entry of `file` in HEAD, `None` if HEAD doesn't have it or
    /// there is no commit yet.
    fn head_entry(&self) -> Result<Option<(Kind, ObjectId)>> {
        let Some(head) = self.repo.head()?.try_peel_to_id()? else {
            return Ok(None);
        };
        let tree = head.object()?.peel_to_commit()?.tree()?;
        let Some(entry) = tree.lookup_entry_by_path(&self.rela)? else {
            return Ok(None);
        };
        Ok(Some((
            Kind::from_tree(entry.mode().kind())
                .with_context(|| format!("can't read {} from HEAD", self.rela.display()))?,
            entry.object_id(),
        )))
    }

    /// The kind and blob id of the entry of `file` in the index.
    fn index_entry(&self) -> Result<Option<(Kind, ObjectId)>> {
        let index = self.repo.index_or_empty()?;
        let rela = gix::path::to_unix_separators_on_windows(gix::path::into_bstr(&*self.rela));
        Ok(index
            .entry_by_path(rela.as_ref())
            .map(|entry| (Kind::from_index(entry.mode), entry.id)))
    }

    fn git(&self) -> Result<Command> {
        let work_dir = self.repo.workdir().context("repo has no worktree")?;
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir")
            .arg(self.repo.git_dir())
            .arg("--work-tree")
            .arg(work_dir)
            // never run a configured fsmonitor hook
            .args(["-c", "core.fsmonitor=false"])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(cmd)
    }

    fn update_index(&self, args: &[&str]) -> Result<()> {
        let mut cmd = self.git()?;
        cmd.arg("update-index").args(args);
        run(cmd, None).map(drop)
    }
}

/// Runs `cmd` with `stdin` and returns its stdout, or an error with its stderr.
fn run(mut cmd: Command, stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().context("failed to run git")?;
    // write from another thread so that a chatty git can't deadlock us
    let writer = stdin.map(|data| {
        let mut pipe = child.stdin.take().expect("stdin is piped");
        let data = data.to_vec();
        std::thread::spawn(move || pipe.write_all(&data))
    });
    let output = child.wait_with_output().context("failed to run git")?;
    if let Some(writer) = writer {
        // a broken pipe is reported by git's exit status
        let _ = writer.join();
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git failed: {}", stderr.trim());
    }
    Ok(output.stdout)
}

/// The version of `file` in the index, converted like `get_diff_base`, `None` when the file isn't
/// in the index.
pub fn index_file(file: &Path, trust_full: bool) -> Result<Option<Vec<u8>>> {
    let target = Target::new(file, trust_full)?;
    let Some((kind, id)) = target.index_entry()? else {
        return Ok(None);
    };
    if matches!(kind, Kind::Symlink | Kind::Submodule) {
        bail!(
            "{} is not a regular file in the index",
            target.rela.display()
        );
    }
    to_worktree(&target.repo, &target.abs, id).map(Some)
}

/// Sets the index version of `file` to `contents` (working-tree form), or removes `file` from the
/// index with `None`.
pub fn set_index_file(file: &Path, contents: Option<&[u8]>, trust_full: bool) -> Result<()> {
    let target = Target::new(file, trust_full)?;
    let rela = target.rela_str()?;
    let Some(contents) = contents else {
        return target.update_index(&["--force-remove", "--", &rela]);
    };

    let kind = match target.index_entry()? {
        Some((kind, _)) => kind,
        None => match target.head_entry()? {
            Some((kind, _)) => kind,
            None if is_executable(&target.abs) => Kind::Executable,
            None => Kind::File,
        },
    };
    match kind {
        Kind::Symlink => bail!("can't stage {rela}: it is a symlink"),
        Kind::Submodule => bail!("can't stage {rela}: it is a submodule"),
        Kind::File | Kind::Executable => {}
    }

    let data = to_git(&target, contents)?;
    let mut cmd = target.git()?;
    cmd.args(["hash-object", "-w", "--no-filters", "--stdin"]);
    let out = run(cmd, Some(&data))?;
    let oid = String::from_utf8_lossy(&out).trim().to_owned();
    target.update_index(&[
        "--add",
        "--cacheinfo",
        &format!("{},{oid},{rela}", kind.mode()),
    ])
}

/// Sets the index version of `file` to its HEAD version (same blob id and mode), or removes it
/// from the index when HEAD doesn't have it (or there is no HEAD commit yet).
pub fn reset_index_file(file: &Path, trust_full: bool) -> Result<()> {
    let target = Target::new(file, trust_full)?;
    let rela = target.rela_str()?;
    match target.head_entry()? {
        Some((kind, id)) => target.update_index(&[
            "--add",
            "--cacheinfo",
            &format!("{},{id},{rela}", kind.mode()),
        ]),
        None => target.update_index(&["--force-remove", "--", &rela]),
    }
}

/// Converts working tree contents to the form stored in git (eol conversion and so on), without
/// running drivers that gix considers untrusted.
fn to_git(target: &Target, contents: &[u8]) -> Result<Vec<u8>> {
    let (mut pipeline, index) = target.repo.filter_pipeline(None)?;
    let mut outcome = pipeline.convert_to_git(contents, &target.rela, &index)?;
    let mut buf = Vec::with_capacity(contents.len());
    outcome.read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::git::test::{create_commit, empty_git_repo, exec_git_args};

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn read_index_file() {
        let repo = empty_git_repo();
        let dir = repo.path();
        // the index is read in the working tree's form: keep LF whatever the user's git config
        // says (core.autocrlf is true on Windows CI runners)
        exec_git_args(&["config", "core.autocrlf", "false"], dir);
        let file = dir.join("a.txt");
        fs::write(&file, "one\n").unwrap();
        // untracked
        assert_eq!(index_file(&file, true).unwrap(), None);
        create_commit(dir, true);
        assert_eq!(index_file(&file, true).unwrap().unwrap(), b"one\n");
        // modified and staged
        fs::write(&file, "two\n").unwrap();
        exec_git_args(&["add", "a.txt"], dir);
        fs::write(&file, "three\n").unwrap();
        assert_eq!(index_file(&file, true).unwrap().unwrap(), b"two\n");
        // deleted from disk still reads
        fs::remove_file(&file).unwrap();
        assert_eq!(index_file(&file, false).unwrap().unwrap(), b"two\n");
    }

    #[test]
    fn set_and_remove() {
        let repo = empty_git_repo();
        let dir = repo.path();
        // the index is read in the working tree's form: keep LF whatever the user's git config
        // says (core.autocrlf is true on Windows CI runners)
        exec_git_args(&["config", "core.autocrlf", "false"], dir);
        let file = dir.join("a.txt");
        fs::write(&file, "one\ntwo\n").unwrap();
        create_commit(dir, true);
        fs::write(&file, "ONE\ntwo\nthree\n").unwrap();

        set_index_file(&file, Some(b"ONE\ntwo\n"), true).unwrap();
        let diff = git_out(dir, &["diff", "--cached", "--no-color"]);
        assert!(diff.contains("-one\n+ONE\n"), "{diff}");
        assert!(!diff.contains("three"), "{diff}");
        assert_eq!(index_file(&file, true).unwrap().unwrap(), b"ONE\ntwo\n");

        // removing stages a deletion
        set_index_file(&file, None, true).unwrap();
        assert_eq!(
            git_out(dir, &["diff", "--cached", "--name-status"]),
            "D\ta.txt\n"
        );
        assert_eq!(index_file(&file, true).unwrap(), None);
    }

    #[test]
    fn set_on_untracked_adds() {
        let repo = empty_git_repo();
        let dir = repo.path();
        let file = dir.join("new.txt");
        fs::write(&file, "x\ny\n").unwrap();
        set_index_file(&file, Some(b"x\n"), true).unwrap();
        assert_eq!(
            git_out(dir, &["ls-files", "--stage"])
                .split_whitespace()
                .next(),
            Some("100644")
        );
        assert_eq!(git_out(dir, &["show", ":new.txt"]), "x\n");
    }

    #[test]
    fn reset_restores_head() {
        let repo = empty_git_repo();
        let dir = repo.path();
        let file = dir.join("a.txt");
        fs::write(&file, "one\n").unwrap();
        create_commit(dir, true);
        fs::write(&file, "two\n").unwrap();
        exec_git_args(&["add", "a.txt"], dir);
        reset_index_file(&file, true).unwrap();
        assert_eq!(git_out(dir, &["diff", "--cached"]), "");
        assert_eq!(git_out(dir, &["status", "--porcelain"]), " M a.txt\n");

        // a newly added file becomes untracked again
        let new = dir.join("new.txt");
        fs::write(&new, "n\n").unwrap();
        exec_git_args(&["add", "new.txt"], dir);
        reset_index_file(&new, true).unwrap();
        assert_eq!(
            git_out(dir, &["status", "--porcelain"]),
            " M a.txt\n?? new.txt\n"
        );
    }

    #[test]
    fn reset_without_head_commit() {
        let repo = empty_git_repo();
        let dir = repo.path();
        let file = dir.join("a.txt");
        fs::write(&file, "one\n").unwrap();
        exec_git_args(&["add", "a.txt"], dir);
        reset_index_file(&file, true).unwrap();
        assert_eq!(git_out(dir, &["ls-files", "--stage"]), "");
    }

    #[test]
    fn reset_deleted_file() {
        let repo = empty_git_repo();
        let dir = repo.path();
        let file = dir.join("a.txt");
        fs::write(&file, "one\n").unwrap();
        create_commit(dir, true);
        fs::remove_file(&file).unwrap();
        set_index_file(&file, None, true).unwrap();
        assert_eq!(git_out(dir, &["status", "--porcelain"]), "D  a.txt\n");
        reset_index_file(&file, true).unwrap();
        assert_eq!(git_out(dir, &["status", "--porcelain"]), " D a.txt\n");
    }

    #[cfg(unix)]
    #[test]
    fn keeps_executable_mode() {
        use std::os::unix::fs::PermissionsExt;
        let repo = empty_git_repo();
        let dir = repo.path();
        let file = dir.join("run.sh");
        fs::write(&file, "#!/bin/sh\necho a\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        create_commit(dir, true);
        set_index_file(&file, Some(b"#!/bin/sh\necho b\n"), true).unwrap();
        assert!(git_out(dir, &["ls-files", "--stage"]).starts_with("100755"));
        reset_index_file(&file, true).unwrap();
        assert!(git_out(dir, &["ls-files", "--stage"]).starts_with("100755"));

        // a new executable file is added as executable
        let new = dir.join("new.sh");
        fs::write(&new, "x\n").unwrap();
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755)).unwrap();
        set_index_file(&new, Some(b"x\n"), true).unwrap();
        assert!(git_out(dir, &["ls-files", "--stage", "new.sh"]).starts_with("100755"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks() {
        let repo = empty_git_repo();
        let dir = repo.path();
        fs::write(dir.join("target"), "t\n").unwrap();
        std::os::unix::fs::symlink("target", dir.join("link")).unwrap();
        create_commit(dir, true);
        let err = set_index_file(&dir.join("link"), Some(b"x"), true).unwrap_err();
        assert!(err.to_string().contains("it is a symlink"), "{err}");
    }
}
