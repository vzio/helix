//! Tests of the `:worktrees` picker (this fork) and of what the dashboard shows of worktrees.
//!
//! They look at a repository with a main worktree and linked ones, built by [`fleet`]. The
//! working directory is the process's, which tests can't change, so each test runs itself again
//! in a process of its own with the working directory in the fleet (see
//! [`super::git_log::Repo::enter_with`]).

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use super::git_log::{git, on_screen, status, Repo, Session};

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86400);

/// Sets the modification time of a file, if it exists.
fn touch(path: &Path, time: SystemTime) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        file.set_modified(time).unwrap();
    }
}

fn touch_git_dir(git_dir: &Path, time: SystemTime) {
    for file in ["index", "HEAD", "logs/HEAD"] {
        touch(&git_dir.join(file), time);
    }
}

/// Every plain file of `dir` (not its subfolders) gets `time`.
fn touch_files(dir: &Path, time: SystemTime) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            touch(&path, time);
        }
    }
}

fn commit(dir: &Path, file: &str, message: &str) {
    fs::write(dir.join(file), format!("{file}\n")).unwrap();
    git(dir, "", &["add", "-A"]);
    git(dir, "", &["commit", "-q", "-m", message]);
}

/// The folders of [`fleet`].
pub(super) struct Fleet {
    pub main: PathBuf,
    pub dirty: PathBuf,
    pub ahead: PathBuf,
    pub detached: PathBuf,
    pub gone: PathBuf,
}

impl Fleet {
    pub fn at(parent: &Path) -> Self {
        let others = parent.join("others");
        fs::create_dir_all(&others).unwrap();
        Fleet {
            main: parent.join("main"),
            dirty: others.join("dirty"),
            ahead: others.join("ahead"),
            detached: others.join("detached"),
            gone: others.join("gone"),
        }
    }
}

/// A repository `main` (on its branch `main`, pushed to a remote) with three linked worktrees in
/// `others/` and a fourth whose folder was deleted:
///
/// - `dirty` (branch `dirty`): a staged, an unstaged and an untracked file, modified 2 hours ago;
/// - `ahead` (branch `ahead`): two commits ahead of its upstream, the local branch `main`, 3 days;
/// - `detached`: nothing, 40 days;
/// - `gone` (branch `gone`): deleted.
///
/// The main worktree is clean and was last modified 400 days ago. Returns the main worktree.
pub(super) fn fleet(parent: &Path) -> PathBuf {
    build(parent, true)
}

/// [`fleet`] without the deleted worktree: three other worktrees, which the dashboard lists in
/// full.
pub(super) fn fleet_alive(parent: &Path) -> PathBuf {
    build(parent, false)
}

/// A repository with one other worktree, `alive` (3 days), and one whose folder was deleted.
pub(super) fn one_and_a_missing(parent: &Path) -> PathBuf {
    let fleet = Fleet::at(parent);
    let main = init(&fleet);
    git(
        &main,
        "",
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "alive",
            fleet.ahead.to_str().unwrap(),
        ],
    );
    git(
        &main,
        "",
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "gone",
            fleet.gone.to_str().unwrap(),
        ],
    );
    fs::remove_dir_all(&fleet.gone).unwrap();
    touch_files(&fleet.ahead, SystemTime::now() - 500 * DAY);
    touch_git_dir(
        &main.join(".git/worktrees/ahead"),
        SystemTime::now() - 3 * DAY,
    );
    main
}

/// A repository whose only other worktree is missing.
pub(super) fn only_a_missing(parent: &Path) -> PathBuf {
    let fleet = Fleet::at(parent);
    let main = init(&fleet);
    git(
        &main,
        "",
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "gone",
            fleet.gone.to_str().unwrap(),
        ],
    );
    fs::remove_dir_all(&fleet.gone).unwrap();
    main
}

/// A folder outside of any repository.
pub(super) fn plain(parent: &Path) -> PathBuf {
    let dir = parent.join("plain");
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The main worktree, with a commit and a remote.
fn init(fleet: &Fleet) -> PathBuf {
    let remote = fleet.main.parent().unwrap().join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, "", &["init", "-q", "--bare"]);
    fs::create_dir_all(&fleet.main).unwrap();
    git(&fleet.main, "", &["init", "-q"]);
    commit(&fleet.main, "tracked.txt", "Initial commit");
    git(
        &fleet.main,
        "",
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&fleet.main, "", &["push", "-q", "-u", "origin", "main"]);
    fleet.main.clone()
}

fn build(parent: &Path, with_gone: bool) -> PathBuf {
    let fleet = Fleet::at(parent);
    init(&fleet);

    let add = |path: &Path, args: &[&str]| {
        let mut full = vec!["worktree", "add", "-q"];
        full.extend_from_slice(args);
        full.push(path.to_str().unwrap());
        git(&fleet.main, "", &full);
    };
    add(&fleet.dirty, &["-b", "dirty"]);
    add(&fleet.ahead, &["-b", "ahead"]);
    add(&fleet.detached, &["--detach"]);
    if with_gone {
        add(&fleet.gone, &["-b", "gone"]);
    }

    fs::write(fleet.dirty.join("staged.txt"), "staged\n").unwrap();
    git(&fleet.dirty, "", &["add", "staged.txt"]);
    fs::write(fleet.dirty.join("tracked.txt"), "changed\n").unwrap();
    fs::write(fleet.dirty.join("untracked.txt"), "untracked\n").unwrap();

    commit(&fleet.ahead, "one.txt", "one");
    commit(&fleet.ahead, "two.txt", "two");
    git(
        &fleet.ahead,
        "",
        &["branch", "--set-upstream-to=main", "ahead"],
    );

    if with_gone {
        fs::remove_dir_all(&fleet.gone).unwrap();
    }

    // the times: all files are old, the dirty ones and the git files make the "modified"
    let now = SystemTime::now();
    let git_dir = fleet.main.join(".git");
    let linked = git_dir.join("worktrees");
    for (dir, git_dir, ago) in [
        (&fleet.main, git_dir.clone(), 400 * DAY),
        (&fleet.dirty, linked.join("dirty"), 5 * DAY),
        (&fleet.ahead, linked.join("ahead"), 3 * DAY),
        (&fleet.detached, linked.join("detached"), 40 * DAY),
    ] {
        touch_files(dir, now - 500 * DAY);
        touch_git_dir(&git_dir, now - ago);
    }
    touch(&fleet.dirty.join("staged.txt"), now - 6 * HOUR);
    touch(&fleet.dirty.join("untracked.txt"), now - 4 * HOUR);
    touch(
        &fleet.dirty.join("tracked.txt"),
        now - 2 * HOUR - 5 * MINUTE,
    );
    fleet.main
}

/// The path as the picker and the dashboard show it.
pub(super) fn shown(path: &Path) -> String {
    vortex_stdx::path::fold_home_dir(path).display().to_string()
}

const NO_WORKTREES: &str = "no other worktrees";

/// Whether the picker is on the screen.
fn picker_open(session: &Session) -> bool {
    session.rows().iter().any(|row| row.starts_with('┌'))
}

/// A row of the picker with its spaces collapsed and without the cursor.
pub(super) fn flat(row: &str) -> String {
    let row = row.trim_start();
    let row = row.strip_prefix('>').unwrap_or(row);
    row.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The worktree rows of the picker, flat, in order.
fn listed(session: &Session) -> Vec<String> {
    session
        .frame()
        .iter()
        .filter(|row| row.contains("others/") || row.contains("/main"))
        .map(|row| flat(row))
        .collect()
}

/// Opens the picker and waits for every row to be read (the times of the rows that can be).
async fn open(session: &mut Session, loaded: usize) {
    session.keys(":worktrees<ret>").await;
    session
        .wait_for_screen("the worktrees", |rows| {
            let ago = rows.iter().filter(|row| row.contains(" ago")).count();
            ago >= loaded
                && !rows
                    .iter()
                    .any(|row| row.contains("─────────") && row.contains("others/"))
        })
        .await;
    // the rows are in their final order a frame after the last one is read
    session.run_for(Duration::from_millis(300)).await;
}

fn cwd() -> PathBuf {
    vortex_stdx::env::current_working_dir()
}

fn documents(session: &Session) -> Vec<vortex_view::DocumentId> {
    let mut ids: Vec<_> = session.app.editor.documents().map(|doc| doc.id()).collect();
    ids.sort();
    ids
}

#[tokio::test(flavor = "multi_thread")]
async fn the_picker_lists_the_current_worktree_first_then_by_last_modified() {
    let Some(main) = Repo::enter_with(
        module_path!(),
        "the_picker_lists_the_current_worktree_first_then_by_last_modified",
        fleet,
    ) else {
        return;
    };
    let fleet = Fleet::at(main.parent().unwrap());
    let mut session = Session::new();
    session.first_frame().await;
    open(&mut session, 4).await;

    // no header with the names of the columns: the row under the prompt is a worktree
    assert!(
        flat(&session.frame()[2]).starts_with(&format!("● {}", shown(&fleet.main))),
        "{:#?}",
        session.frame()
    );
    // and nothing in the status line: it is for results
    assert_eq!(status(&session.app), "");
    // the current one first and marked (the others have no mark), the rest newest first, the
    // missing one last: `dirty` was read as the newest though its git dir is the older one of
    // the two that are at all recent
    assert_eq!(
        listed(&session),
        vec![
            format!("● {} main clean all pushed 1y ago", shown(&fleet.main)),
            format!(
                "{} dirty 1 staged · 1 unstaged · 1 untracked no upstream 2h ago",
                shown(&fleet.dirty)
            ),
            format!("{} ahead clean 2 unpushed 3d ago", shown(&fleet.ahead)),
            format!(
                "{} detached at {} clean no upstream 1mo ago",
                shown(&fleet.detached),
                git(&fleet.detached, "", &["rev-parse", "--short", "HEAD"]).trim()
            ),
            format!("{} missing", shown(&fleet.gone)),
        ],
        "{:#?}",
        session.frame()
    );
    assert_eq!(
        session
            .frame()
            .iter()
            .filter(|row| row.contains('●'))
            .count(),
        1
    );
    // the cursor starts on the current worktree
    assert!(
        session
            .frame()
            .iter()
            .any(|row| row.trim_start().starts_with("> ●")),
        "{:#?}",
        session.frame()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_counts_fill_in_from_the_background() {
    let Some(_) = Repo::enter_with(
        module_path!(),
        "the_counts_fill_in_from_the_background",
        fleet,
    ) else {
        return;
    };
    let mut session = Session::new();
    session.first_frame().await;
    // right after the command nothing but the paths and branches is certain; the cells are
    // shimmering (`─`) or already filled in, and in the end they all are
    session.keys(":worktrees<ret>").await;
    assert!(picker_open(&session));
    session
        .wait_for_screen("the counts", |rows| {
            on_screen(rows, "1 staged · 1 unstaged · 1 untracked")
                && on_screen(rows, "2 unpushed")
                && on_screen(rows, "all pushed")
                && on_screen(rows, "no upstream")
        })
        .await;
    session
        .wait_for_screen("every time", |rows| {
            rows.iter().filter(|row| row.contains(" ago")).count() == 4
        })
        .await;
    // nothing is left of the loading cells in the rows
    for row in session.frame().iter().filter(|row| row.contains("others/")) {
        assert!(
            !row.contains("───"),
            "{row:?} still loads:\n{:#?}",
            session.frame()
        );
    }
}

/// The status line of `:cd` to `dir`.
fn cd_status(dir: &Path) -> String {
    format!(
        "Current working directory is now {}",
        dir.canonicalize().unwrap().display()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_changes_the_directory_like_cd() {
    let Some(main) = Repo::enter_with(module_path!(), "enter_changes_the_directory_like_cd", fleet)
    else {
        return;
    };
    let fleet = Fleet::at(main.parent().unwrap());
    let mut session = Session::new();
    session.first_frame().await;
    let before = documents(&session);
    let views = session.app.editor.tree.views().count();

    // the second row: `dirty`
    open(&mut session, 4).await;
    session.keys("j<ret>").await;
    assert!(!picker_open(&session), "{:#?}", session.rows());
    assert_eq!(cwd(), fleet.dirty.canonicalize().unwrap());
    let picked = status(&session.app);
    assert_eq!(picked, cd_status(&fleet.dirty));
    // no buffer was opened or closed, no view split
    assert_eq!(documents(&session), before);
    assert_eq!(session.app.editor.tree.views().count(), views);

    // `:cd` says the same, on the way back and to the same place
    session
        .keys(&format!(":cd {}<ret>", fleet.main.display()))
        .await;
    assert_eq!(cwd(), fleet.main.canonicalize().unwrap());
    session
        .keys(&format!(":cd {}<ret>", fleet.dirty.display()))
        .await;
    assert_eq!(status(&session.app), picked);
    assert_eq!(cwd(), fleet.dirty.canonicalize().unwrap());
    assert_eq!(documents(&session), before);

    // from there the picker lists the same worktrees, `dirty` current now
    session.keys(":wt<ret>").await;
    session
        .wait_for_screen("the picker", |rows| on_screen(rows, "others/dirty"))
        .await;
    assert!(listed(&session)[0].starts_with(&format!("● {}", shown(&fleet.dirty))));
}

#[tokio::test(flavor = "multi_thread")]
async fn enter_on_the_current_worktree_only_closes_the_picker() {
    let Some(main) = Repo::enter_with(
        module_path!(),
        "enter_on_the_current_worktree_only_closes_the_picker",
        fleet,
    ) else {
        return;
    };
    let mut session = Session::new();
    session.first_frame().await;
    let before = documents(&session);
    let start = cwd();
    assert_eq!(start, main);
    open(&mut session, 4).await;
    session.keys("<ret>").await;
    assert!(!picker_open(&session), "{:#?}", session.rows());
    assert_eq!(cwd(), start);
    assert!(
        !status(&session.app).contains("Current working directory"),
        "{:?}",
        status(&session.app)
    );
    assert_eq!(documents(&session), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_worktree_can_not_be_picked() {
    let Some(main) = Repo::enter_with(
        module_path!(),
        "a_missing_worktree_can_not_be_picked",
        fleet,
    ) else {
        return;
    };
    let fleet = Fleet::at(main.parent().unwrap());
    let mut session = Session::new();
    session.first_frame().await;
    let before = documents(&session);
    open(&mut session, 4).await;
    // `gone` is the last of the five rows
    session.keys("jjjj<ret>").await;
    assert!(picker_open(&session), "{:#?}", session.rows());
    assert_eq!(cwd(), main);
    let message = status(&session.app);
    assert!(
        message.contains("is missing") && message.contains(&shown(&fleet.gone)),
        "{message:?}"
    );
    assert_eq!(documents(&session), before);

    // the other rows can still be picked from the same picker
    session.keys("k<ret>").await;
    assert!(!picker_open(&session));
    assert_eq!(cwd(), fleet.detached.canonicalize().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_of_a_repository_there_is_only_a_message() {
    let Some(_) = Repo::enter_with(
        module_path!(),
        "outside_of_a_repository_there_is_only_a_message",
        plain,
    ) else {
        return;
    };
    let mut session = Session::new();
    session.first_frame().await;
    session.keys(":worktrees<ret>").await;
    session.run_for(Duration::from_millis(300)).await;
    assert!(!picker_open(&session), "{:#?}", session.rows());
    assert_eq!(status(&session.app), NO_WORKTREES);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_other_worktrees_there_is_only_a_message() {
    let Some(_) = Repo::enter_in(
        module_path!(),
        "without_other_worktrees_there_is_only_a_message",
    ) else {
        return;
    };
    let mut session = Session::new();
    session.first_frame().await;
    session.keys(":wt<ret>").await;
    session.run_for(Duration::from_millis(300)).await;
    assert!(!picker_open(&session), "{:#?}", session.rows());
    assert_eq!(status(&session.app), NO_WORKTREES);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_only_a_missing_worktree_there_is_only_a_message() {
    let Some(_) = Repo::enter_with(
        module_path!(),
        "with_only_a_missing_worktree_there_is_only_a_message",
        only_a_missing,
    ) else {
        return;
    };
    let mut session = Session::new();
    session.first_frame().await;
    session.keys(":worktrees<ret>").await;
    session.run_for(Duration::from_millis(300)).await;
    assert!(!picker_open(&session), "{:#?}", session.rows());
    assert_eq!(status(&session.app), NO_WORKTREES);
}

#[tokio::test(flavor = "multi_thread")]
async fn from_a_linked_worktree_the_main_one_is_listed_too() {
    let Some(main) = Repo::enter_with(
        module_path!(),
        "from_a_linked_worktree_the_main_one_is_listed_too",
        |parent| {
            fleet(parent);
            Fleet::at(parent).ahead
        },
    ) else {
        return;
    };
    // (the working directory is `others/ahead`)
    let fleet = Fleet::at(main.parent().unwrap().parent().unwrap());
    let mut session = Session::new();
    session.first_frame().await;
    open(&mut session, 4).await;
    let rows = listed(&session);
    assert!(
        rows[0].starts_with(&format!("● {} ahead clean 2 unpushed", shown(&fleet.ahead))),
        "{rows:#?}"
    );
    assert!(rows
        .iter()
        .any(|row| row.contains(&format!("{} main", shown(&fleet.main)))));
    assert_eq!(rows.len(), 5, "{rows:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn question_mark_opens_the_key_help_in_normal_mode_and_is_typed_in_filter_mode() {
    let Some(_) = Repo::enter_with(
        module_path!(),
        "question_mark_opens_the_key_help_in_normal_mode_and_is_typed_in_filter_mode",
        fleet,
    ) else {
        return;
    };
    let mut session = Session::new();
    session.first_frame().await;
    open(&mut session, 4).await;
    assert!(!on_screen(&session.rows(), "any key closes"));

    session.keys("?").await;
    let rows = session.rows();
    assert!(on_screen(&rows, "Worktrees"), "{rows:#?}");
    assert!(on_screen(&rows, "any key closes"), "{rows:#?}");
    assert!(on_screen(&rows, "go to the worktree"), "{rows:#?}");
    // any key closes it, and does nothing else: the picker is still there
    session.keys("j").await;
    assert!(!on_screen(&session.rows(), "any key closes"));
    assert!(picker_open(&session));

    // in filter mode `?` is part of the query
    session.keys("s?").await;
    let rows = session.rows();
    assert!(!on_screen(&rows, "any key closes"), "{rows:#?}");
    assert!(session.frame()[0].contains('?'), "{:#?}", session.frame());
}
