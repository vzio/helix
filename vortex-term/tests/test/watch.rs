//! Tests of watch mode: `:watch` follows the files another process changes, `n`/`N` go through
//! the session's changes, `:watch-changes` lists them.
//!
//! Like the tests of the commit log, each one runs again in a process of its own with the
//! working directory in a throwaway repository (see [`Repo::enter_with`]). The files are
//! changed with `std::fs` and `sh` from the test, like an agent or a formatter would: the editor
//! only sees them through the watcher (200 ms debounce), so the tests wait for what they expect
//! with a polling loop and a generous timeout.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use vortex_term::application::Application;
use vortex_view::{
    current_ref,
    editor::Severity,
    watch::{FileKind, Quiet, SessionFile},
};

use super::git_log::{git, status, Repo, Session};
use super::uncommitted::{commit_all, cwd, lines, write};

/// Time for the watcher to see a write and the debounce to pass, at the most.
const TIMEOUT: Duration = Duration::from_secs(20);
/// Between two writes that must have different modification times.
const MTIME_GAP: Duration = Duration::from_millis(25);

fn enter(test: &str) -> Option<PathBuf> {
    Repo::enter_with(module_path!(), test, make)
}

/// The repository of the tests: committed text files, a lockfile, ignored paths, a remote that
/// can't be reached, and at the start of a session a file that is already dirty and one that
/// is untracked.
fn make(parent: &Path) -> PathBuf {
    let work = parent.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, "", &["init", "-q"]);
    let put = |name: &str, text: &str| std::fs::write(work.join(name), text).unwrap();
    put(".gitignore", "ignored/\n*.log\n");
    for (name, prefix) in [
        ("a.txt", "a"),
        ("b.txt", "b"),
        ("c.txt", "c"),
        ("other.txt", "o"),
        ("dirty.txt", "d"),
        ("del.txt", "x"),
    ] {
        put(name, &lines(prefix, 10));
    }
    put("Cargo.lock", &lines("lock", 30));
    git(&work, "", &["add", "-A"]);
    git(&work, "", &["commit", "-q", "-m", "Start"]);
    // a remote nothing listens on, and the branch tracking it
    git(
        &work,
        "",
        &["remote", "add", "origin", "http://127.0.0.1:9/x.git"],
    );
    git(&work, "", &["config", "branch.main.remote", "origin"]);
    git(
        &work,
        "",
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    // dirty when the watch starts: its line 2 is changed (not a change of the session)
    put(
        "dirty.txt",
        &lines("d", 10).replace("d2\n", "d2 before the watch\n"),
    );
    put("untracked.txt", &lines("u", 5));
    work
}

/// Replaces line `number` (from 1) of the file with `text`, as another program would.
fn edit(name: &str, number: usize, text: &str) {
    let path = cwd().join(name);
    let old = std::fs::read_to_string(&path).unwrap();
    let new: String = old
        .split_inclusive('\n')
        .enumerate()
        .map(|(i, line)| {
            if i + 1 == number {
                format!("{text}\n")
            } else {
                line.to_string()
            }
        })
        .collect();
    std::fs::write(path, new).unwrap();
}

fn session_of(app: &Application) -> &vortex_view::watch::Session {
    app.editor.watch.session.as_ref().expect("a watch session")
}

/// The file of the session `name`.
fn file<'a>(app: &'a Application, name: &str) -> Option<&'a SessionFile> {
    app.editor
        .watch
        .session
        .as_ref()
        .and_then(|session| session.file(&cwd().join(name)))
}

/// The lines (from 0, start and end) of the hunks of the session's file `name`, in the order of the file.
fn hunks(app: &Application, name: &str) -> Vec<(u32, u32)> {
    file(app, name)
        .unwrap_or_else(|| panic!("{name} is not in the session"))
        .hunks
        .iter()
        .map(|hunk| (hunk.after.start, hunk.after.end))
        .collect()
}

/// The focused file's name and the line (from 0) of its cursor.
fn focus(app: &Application) -> (String, usize) {
    let (view, doc) = current_ref!(app.editor);
    let name = doc
        .path()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .unwrap_or_else(|| "[scratch]".to_string());
    let text = doc.text().slice(..);
    (name, doc.selection(view.id).primary().cursor_line(text))
}

fn at(name: &str, line: usize) -> (String, usize) {
    (name.to_string(), line)
}

fn is_error(app: &Application) -> Option<String> {
    app.editor
        .get_status()
        .filter(|(_, severity)| **severity == Severity::Error)
        .map(|(message, _)| message.to_string())
}

impl Session {
    /// Runs the editor until `condition` holds.
    async fn wait_watch(&mut self, what: &str, condition: impl Fn(&Application) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !condition(&self.app) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}\nfocus: {:?}\nstatus: {:?}\nscreen:\n{}",
                focus(&self.app),
                status(&self.app),
                self.rows()
                    .iter()
                    .map(|row| row.trim_end())
                    .filter(|row| !row.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            self.run_for(Duration::from_millis(50)).await;
        }
    }

    /// `:watch`, and a moment for the watcher to be armed.
    async fn start_watch(&mut self) {
        self.keys(":watch<ret>").await;
        assert!(self.app.editor.watch.is_active(), "{}", status(&self.app));
        self.run_for(Duration::from_millis(300)).await;
    }

    /// Waits for the focused file and cursor line (from 0).
    async fn wait_for_focus(&mut self, name: &str, line: usize) {
        let expected = at(name, line);
        self.wait_watch(&format!("the focus on {expected:?}"), |app| {
            focus(app) == expected
        })
        .await;
    }

    /// Changes line `number` (from 1) of a file and waits for following to take the cursor
    /// there.
    async fn change_and_follow(&mut self, name: &str, number: usize, text: &str) {
        edit(name, number, text);
        self.wait_for_focus(name, number - 1).await;
    }

    async fn wait_for_screen_text(&mut self, text: &str) {
        let wanted = text.to_string();
        self.wait_watch(&format!("{text:?} on the screen"), move |app| {
            super::git_log::on_screen(&super::git_log::screen_rows(app), &wanted)
        })
        .await;
    }

    fn on_screen(&self, text: &str) -> bool {
        super::git_log::on_screen(&self.rows(), text)
    }

    fn following(&self) -> bool {
        session_of(&self.app).following
    }

    fn watching(&self) -> bool {
        self.app.editor.watch.is_active()
    }

    fn new_count(&self) -> usize {
        session_of(&self.app).new_count()
    }

    /// Three files changed one after the other (the newest last): a line 2, b line 5, c
    /// line 8. Ends following c.
    async fn three_changes(&mut self) {
        self.start_watch().await;
        self.change_and_follow("a.txt", 2, "a2 agent").await;
        self.change_and_follow("b.txt", 5, "b5 agent").await;
        self.change_and_follow("c.txt", 8, "c8 agent").await;
        assert!(self.following());
    }
}

// 1. The baseline

#[tokio::test(flavor = "multi_thread")]
async fn the_session_has_only_the_changes_made_since_the_watch_started() {
    let Some(_) = enter("the_session_has_only_the_changes_made_since_the_watch_started") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    assert!(session_of(&session.app).is_empty());

    // a file that was dirty (line 2 changed before the watch), one that was clean, one that was
    // untracked: another line of each changes now
    edit("dirty.txt", 7, "d7 agent");
    std::thread::sleep(MTIME_GAP);
    edit("a.txt", 4, "a4 agent");
    std::thread::sleep(MTIME_GAP);
    edit("untracked.txt", 5, "u5 agent");
    session
        .wait_watch("all three files in the session", |app| {
            ["dirty.txt", "a.txt", "untracked.txt"]
                .iter()
                .all(|name| file(app, name).is_some())
        })
        .await;

    // only the new hunks: the line changed before the watch (line 2) never shows
    assert_eq!(hunks(&session.app, "dirty.txt"), [(6, 7)]);
    assert_eq!(hunks(&session.app, "a.txt"), [(3, 4)]);
    assert_eq!(hunks(&session.app, "untracked.txt"), [(4, 5)]);
    for name in ["dirty.txt", "a.txt", "untracked.txt"] {
        assert_eq!(file(&session.app, name).unwrap().kind, FileKind::Modified);
    }
    assert_eq!(session_of(&session.app).change_count(), 3);
    // the file the session ended up on has the same hunk in its gutter
    let (view, doc) = current_ref!(session.app.editor);
    let _ = view;
    assert_eq!(doc.watch_diff().expect("a watch gutter").load().len(), 1);

    // the same for the picker
    session.keys(":watch-changes<ret>").await;
    let rows = session.list();
    assert_eq!(rows.len(), 6, "{rows:#?}");
    assert!(
        rows.iter().all(|row| !row.contains("before the watch")),
        "{rows:#?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_changed_back_to_its_baseline_leaves_the_session() {
    let Some(_) = enter("a_file_changed_back_to_its_baseline_leaves_the_session") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    assert_eq!(hunks(&session.app, "a.txt"), [(3, 4)]);
    edit("a.txt", 4, "a4");
    session
        .wait_watch("a.txt unchanged", |app| file(app, "a.txt").is_none())
        .await;
    assert_eq!(session_of(&session.app).change_count(), 0);
}

// 2. Created and deleted files

#[tokio::test(flavor = "multi_thread")]
async fn created_and_deleted_files_are_listed_and_a_deleted_one_is_not_followed() {
    let Some(_) = enter("created_and_deleted_files_are_listed_and_a_deleted_one_is_not_followed")
    else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;

    write("new.txt", "n1\nn2\nn3\n");
    session
        .wait_watch("new.txt in the session", |app| {
            file(app, "new.txt").is_some()
        })
        .await;
    assert_eq!(
        file(&session.app, "new.txt").unwrap().kind,
        FileKind::Created
    );
    // the whole file is the hunk
    assert_eq!(hunks(&session.app, "new.txt"), [(0, 3)]);
    session.wait_for_focus("new.txt", 0).await;

    std::fs::remove_file(cwd().join("del.txt")).unwrap();
    session
        .wait_watch("del.txt in the session", |app| {
            file(app, "del.txt").is_some()
        })
        .await;
    assert_eq!(
        file(&session.app, "del.txt").unwrap().kind,
        FileKind::Deleted
    );
    assert!(hunks(&session.app, "del.txt").is_empty());
    session.run_for(Duration::from_millis(300)).await;
    // following didn't try to open it
    assert_eq!(focus(&session.app), at("new.txt", 0));
    assert!(session
        .app
        .editor
        .document_by_path(cwd().join("del.txt"))
        .is_none());
    assert_eq!(is_error(&session.app), None);
    assert!(session.following());

    // n/N only go through the file that exists
    assert_eq!(session_of(&session.app).hunks_in_order().len(), 1);

    session.keys(":watch-changes<ret>").await;
    // the newest file first: del.txt (its removed lines are one hunk), then new.txt
    assert_eq!(
        session.list(),
        [
            "── del.txt (deleted)  1 hunk  -10",
            "-- del.txt:1",
            "── new.txt (new)  1 hunk  +3",
            "++ new.txt:1-3",
        ]
    );

    // Enter on the deleted file can't open it
    session.keys("<ret>").await;
    assert!(is_error(&session.app).is_some_and(|message| message.contains("was deleted")));
}

// 3. Ignored files and lockfiles

#[tokio::test(flavor = "multi_thread")]
async fn ignored_files_are_not_in_the_session() {
    let Some(_) = enter("ignored_files_are_not_in_the_session") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    std::fs::create_dir(cwd().join("ignored")).unwrap();
    write("ignored/x.txt", "x\n");
    write("build.log", "built\n");
    std::thread::sleep(MTIME_GAP);
    edit("a.txt", 3, "a3 agent");
    session.wait_for_focus("a.txt", 2).await;
    // the batch with the ignored files is over by now
    session.run_for(Duration::from_millis(500)).await;
    let files: Vec<_> = session_of(&session.app)
        .files_newest_first()
        .into_iter()
        .map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["a.txt"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lockfile_is_listed_collapsed_and_neither_followed_nor_counted() {
    let Some(_) = enter("a_lockfile_is_listed_collapsed_and_neither_followed_nor_counted") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 2, "a2 agent").await;
    assert!(session.on_screen("WATCH following"));

    write("Cargo.lock", &lines("locked", 40));
    session
        .wait_watch("Cargo.lock in the session", |app| {
            file(app, "Cargo.lock").is_some()
        })
        .await;
    session.run_for(Duration::from_millis(300)).await;
    let lock = file(&session.app, "Cargo.lock").unwrap();
    assert_eq!(lock.quiet, Quiet::Lockfile);
    assert!(lock.hunks.is_empty());
    // not followed: the focus stays, and it still follows the next real change
    assert_eq!(focus(&session.app), at("a.txt", 1));
    assert!(session.following());
    assert_eq!(session.new_count(), 0);
    assert!(session.on_screen("WATCH following"));
    // n/N don't go there
    assert_eq!(session_of(&session.app).hunks_in_order().len(), 1);

    // paused, it isn't counted as a new change either
    session.keys("j").await;
    assert!(!session.following());
    write("Cargo.lock", &lines("again", 45));
    session.run_for(Duration::from_millis(600)).await;
    assert_eq!(session.new_count(), 0);
    assert!(session.on_screen("WATCH paused"));

    // the picker has its divider and no row of hunks
    session.keys(":watch-changes<ret>").await;
    let rows = session.list();
    assert_eq!(rows.len(), 3, "{rows:#?}");
    assert!(rows[0].contains("Cargo.lock"), "{rows:#?}");
    assert!(rows[1].contains("a.txt"), "{rows:#?}");
}

// 4. Bursts

#[tokio::test(flavor = "multi_thread")]
async fn a_burst_of_writes_ends_on_the_newest_files_first_hunk() {
    let Some(_) = enter("a_burst_of_writes_ends_on_the_newest_files_first_hunk") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    // several quick writes, a few milliseconds apart: one debounced batch
    edit("a.txt", 2, "a2 agent");
    std::thread::sleep(MTIME_GAP);
    edit("b.txt", 6, "b6 agent");
    std::thread::sleep(MTIME_GAP);
    edit("a.txt", 9, "a9 agent");
    std::thread::sleep(MTIME_GAP);
    edit("c.txt", 4, "c4 agent");
    std::thread::sleep(MTIME_GAP);
    edit("c.txt", 8, "c8 agent");
    // the newest file is c.txt: the first of its hunks
    session.wait_for_focus("c.txt", 3).await;
    session.run_for(Duration::from_millis(500)).await;
    assert_eq!(focus(&session.app), at("c.txt", 3));
    assert_eq!(hunks(&session.app, "a.txt"), [(1, 2), (8, 9)]);
    assert_eq!(hunks(&session.app, "b.txt"), [(5, 6)]);
    assert_eq!(hunks(&session.app, "c.txt"), [(3, 4), (7, 8)]);
    assert!(session.following());
    assert_eq!(session.new_count(), 0);
}

// 5. Following

#[tokio::test(flavor = "multi_thread")]
async fn following_jumps_to_the_first_new_hunk_when_idle() {
    let Some(_) = enter("following_jumps_to_the_first_new_hunk_when_idle") else {
        return;
    };
    let mut session = Session::new();
    assert_eq!(focus(&session.app).0, "[scratch]");
    session.start_watch().await;
    assert!(session.following());
    assert!(
        status(&session.app).starts_with("Watching "),
        "{}",
        status(&session.app)
    );
    assert!(
        status(&session.app).contains("1 already uncommitted") || {
            // the dirty and the untracked file
            status(&session.app).contains("2 already uncommitted")
        }
    );

    session.change_and_follow("b.txt", 7, "b7 agent").await;
    assert!(session.following());
    session.wait_for_screen_text("WATCH following").await;
    assert_eq!(session_of(&session.app).new_count(), 0);

    // the next change, in another file, is followed too; the older file stays open
    session.change_and_follow("a.txt", 3, "a3 agent").await;
    assert!(session
        .app
        .editor
        .document_by_path(cwd().join("b.txt"))
        .is_some());
    // and a second hunk of a file whose first one is not new: the new one is the target
    session.change_and_follow("a.txt", 9, "a9 agent").await;
    assert_eq!(hunks(&session.app, "a.txt"), [(2, 3), (8, 9)]);
}

// 6. Moving pauses following

async fn paused_by(keys: &str) {
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    assert!(session.following());

    session.keys(keys).await;
    assert!(!session.following(), "{keys} didn't pause following");
    assert!(session.watching());
    assert!(session.on_screen("WATCH paused"));
    let before = focus(&session.app);

    // a change now doesn't take the view away
    edit("b.txt", 6, "b6 agent");
    session
        .wait_watch("b.txt counted as new", |app| {
            session_of(app).new_count() == 1
        })
        .await;
    session.wait_for_screen_text("WATCH 1 new").await;
    assert_eq!(focus(&session.app), before);
    assert!(!session.following());

    std::thread::sleep(MTIME_GAP);
    edit("c.txt", 2, "c2 agent");
    session
        .wait_watch("c.txt counted as new", |app| {
            session_of(app).new_count() == 2
        })
        .await;
    session.wait_for_screen_text("WATCH 2 new").await;
    assert_eq!(focus(&session.app), before);
}

macro_rules! pauses_on {
    ($name:ident, $keys:expr) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let Some(_) = enter(stringify!($name)) else {
                return;
            };
            paused_by($keys).await;
        }
    };
}

pauses_on!(moving_with_j_pauses_following, "j");
pauses_on!(gg_pauses_following, "gg");
pauses_on!(a_search_pauses_following, "/a8<ret>");
pauses_on!(scrolling_pauses_following, "<C-d>");
pauses_on!(a_word_motion_pauses_following, "w");
pauses_on!(selecting_pauses_following, "vj");

#[tokio::test(flavor = "multi_thread")]
async fn the_command_line_and_esc_do_not_pause_following() {
    let Some(_) = enter("the_command_line_and_esc_do_not_pause_following") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.keys("<esc>").await;
    assert!(session.following());
    session.keys(":<esc>").await;
    assert!(session.following());
    session.keys(":watch-changes<ret>").await;
    // the picker of the changes is looking: it pauses
    assert!(!session.following());
}

// 7. n and N

#[tokio::test(flavor = "multi_thread")]
async fn n_and_n_go_through_the_changes_in_the_order_they_happened() {
    let Some(_) = enter("n_and_n_go_through_the_changes_in_the_order_they_happened") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;

    // n from the newest wraps around to the oldest and pauses
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    assert_eq!(status(&session.app), "Wrapped around to the first change");
    assert!(!session.following());
    assert!(session.watching());
    // the newest changes are counted as new again? no: they were seen when followed
    assert_eq!(session.new_count(), 0);

    session.keys("n").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    assert!(!session.following());
    session.keys("n").await;
    // the newest: following again
    assert_eq!(focus(&session.app), at("c.txt", 7));
    assert!(session.following());
    assert_eq!(status(&session.app), "Following changes again");
    session.wait_for_screen_text("WATCH following").await;

    // N goes back: each older change pauses
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    assert!(!session.following());
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    // N from the oldest wraps to the newest, which follows again
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("c.txt", 7));
    assert!(session.following());

    // a new change while paused is counted, and n goes to it and follows again
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    edit("other.txt", 3, "o3 agent");
    session
        .wait_watch("one new change", |app| session_of(app).new_count() == 1)
        .await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("c.txt", 7));
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("other.txt", 2));
    assert!(session.following());
    assert_eq!(session.new_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn n_from_the_middle_of_a_file_goes_to_the_next_change_not_the_next_line() {
    let Some(_) = enter("n_from_the_middle_of_a_file_goes_to_the_next_change_not_the_next_line")
    else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 2, "a2 agent").await;
    std::thread::sleep(MTIME_GAP);
    session.change_and_follow("a.txt", 8, "a8 agent").await;
    // a2 is older than a8: N goes there, and n back
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("a.txt", 7));
    assert!(session.following());
}

// 8. Other actions end watch mode, the changes stay

/// Three changes, then `keys`: watch mode is over, the changes are still there.
async fn ended_by(keys: &str) -> Session {
    let mut session = Session::new();
    session.three_changes().await;
    session.keys(keys).await;
    assert!(!session.watching(), "{keys} didn't end watch mode");
    assert!(session.app.editor.watch.session.is_some());
    assert_eq!(session_of(&session.app).hunks_in_order().len(), 3);
    assert!(!session.on_screen("WATCH"));
    // the gutter markers stay
    let doc = session
        .app
        .editor
        .document_by_path(cwd().join("c.txt"))
        .expect("c.txt is open");
    assert_eq!(doc.watch_diff().expect("a watch gutter").load().len(), 1);
    session
}

/// From where watch mode ended, n and N go through the session; `:watch-clear` forgets it.
async fn changes_stay_after_the_end(mut session: Session) {
    // (the cursor isn't in a hunk or is in the newest: n goes around to the oldest)
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    assert_eq!(status(&session.app), "Wrapped around to the first change");
    // watch mode is over: it doesn't follow again
    assert!(!session.following());
    assert!(!session.watching());
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("c.txt", 7));
    assert!(!session.following());
    // and back: N from the oldest wraps to the newest, with its message
    session.keys("NN").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("c.txt", 7));
    assert_eq!(status(&session.app), "Wrapped around to the last change");

    // the picker still lists them
    session.keys(":watch-changes<ret>").await;
    assert_eq!(session.list().len(), 6);
    session.keys("<esc>").await;

    // :watch-clear forgets them
    session.keys(":watch-clear<ret>").await;
    assert!(session.app.editor.watch.session.is_none());
    for name in ["a.txt", "b.txt", "c.txt"] {
        let doc = session
            .app
            .editor
            .document_by_path(cwd().join(name))
            .unwrap();
        assert!(doc.watch_diff().is_none(), "{name} keeps its markers");
    }
    // n is the search again: without a pattern it doesn't go anywhere
    let before = focus(&session.app);
    session.keys("n").await;
    assert_eq!(focus(&session.app), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_text_ends_watch_mode_but_keeps_the_changes() {
    let Some(_) = enter("typing_text_ends_watch_mode_but_keeps_the_changes") else {
        return;
    };
    let session = ended_by("ix<esc>").await;
    // (the cursor of c.txt is where the typing was)
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_a_file_ends_watch_mode_but_keeps_the_changes() {
    let Some(_) = enter("opening_a_file_ends_watch_mode_but_keeps_the_changes") else {
        return;
    };
    let session = ended_by(":open other.txt<ret>").await;
    assert_eq!(focus(&session.app).0, "other.txt");
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picker_ends_watch_mode_but_keeps_the_changes() {
    let Some(_) = enter("a_picker_ends_watch_mode_but_keeps_the_changes") else {
        return;
    };
    let mut session = ended_by("<space>f").await;
    session.wait_for_screen_text("other.txt").await;
    session.keys("<esc>").await;
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_stop_ends_watch_mode_and_the_files_are_no_longer_watched() {
    let Some(_) = enter("watch_stop_ends_watch_mode_and_the_files_are_no_longer_watched") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;
    session.keys(":watch-stop<ret>").await;
    assert!(!session.watching());
    assert!(
        status(&session.app).starts_with("Watch ended"),
        "{}",
        status(&session.app)
    );
    assert!(
        status(&session.app).contains("3 changes"),
        "{}",
        status(&session.app)
    );
    let before = focus(&session.app);
    edit("other.txt", 3, "o3 agent");
    session.run_for(Duration::from_millis(800)).await;
    assert!(file(&session.app, "other.txt").is_none());
    assert_eq!(focus(&session.app), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_search_made_once_watch_mode_ended_n_repeats_the_search() {
    let Some(_) = enter("after_a_search_made_once_watch_mode_ended_n_repeats_the_search") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.keys(":watch-stop<ret>").await;
    assert!(!session.watching());
    // the cursor is on line 4 (a hunk); a search starts from the cursor
    session.keys("/a<ret>").await;
    let (name, line) = focus(&session.app);
    assert_eq!(name, "a.txt");
    assert!(line >= 3, "the search found line {line}");
    session.keys("n").await;
    // the search goes on to the next line; it didn't go to the hunk
    assert_eq!(focus(&session.app), at("a.txt", line + 1));
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("a.txt", line));
    // the session is still there
    assert!(session.app.editor.watch.session.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn while_watching_a_search_does_not_take_n() {
    let Some(_) = enter("while_watching_a_search_does_not_take_n") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.keys("/a<ret>").await;
    assert!(!session.following());
    assert!(session.watching());
    // n is still the session's: the only change is the hunk at line 4
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("a.txt", 3));
    assert!(session.following());
}

#[tokio::test(flavor = "multi_thread")]
async fn watching_again_starts_a_fresh_session() {
    let Some(_) = enter("watching_again_starts_a_fresh_session") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.keys(":watch-stop<ret>").await;
    session.start_watch().await;
    // a.txt is dirty now: its edit is the new baseline, and the old markers are gone
    assert!(session_of(&session.app).is_empty());
    assert!(session.following());
    session.change_and_follow("b.txt", 3, "b3 agent").await;
    assert!(file(&session.app, "a.txt").is_none());
    assert_eq!(session_of(&session.app).change_count(), 1);
}

// 9. Unsaved buffers

#[tokio::test(flavor = "multi_thread")]
async fn a_buffer_with_unsaved_edits_is_never_overwritten_nor_followed() {
    let Some(_) = enter("a_buffer_with_unsaved_edits_is_never_overwritten_nor_followed") else {
        return;
    };
    let mut session = Session::new();
    session
        .keys(":open a.txt<ret>ix<esc>:open other.txt<ret>")
        .await;
    let a = cwd().join("a.txt");
    assert!(session
        .app
        .editor
        .document_by_path(&a)
        .unwrap()
        .is_modified());
    assert_eq!(focus(&session.app).0, "other.txt");
    // (watch mode starts after the edit: typing ends it)
    session.start_watch().await;

    edit("a.txt", 6, "a6 agent");
    session
        .wait_watch("a.txt in the session", |app| file(app, "a.txt").is_some())
        .await;
    session.run_for(Duration::from_millis(500)).await;
    // the buffer keeps what the user typed
    let doc = session.app.editor.document_by_path(&a).unwrap();
    assert!(doc.is_modified());
    assert_eq!(doc.text().line(0).to_string(), "xa1\n");
    assert_eq!(doc.text().line(5).to_string(), "a6\n");
    // the stale-buffer warning
    let (message, severity) = session.app.editor.get_status().expect("a warning");
    assert_eq!(*severity, Severity::Warning, "{message}");
    assert!(message.contains("changed on disk"), "{message}");
    // and following stayed where it was
    assert_eq!(focus(&session.app).0, "other.txt");

    // other files still follow
    edit("b.txt", 7, "b7 agent");
    session.wait_for_focus("b.txt", 6).await;
    // the disk has the change (in the session) but the buffer doesn't
    assert_eq!(hunks(&session.app, "a.txt"), [(5, 6)]);
    let doc = session.app.editor.document_by_path(&a).unwrap();
    assert_eq!(doc.text().line(5).to_string(), "a6\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clean_open_buffer_is_reloaded_and_followed() {
    let Some(_) = enter("a_clean_open_buffer_is_reloaded_and_followed") else {
        return;
    };
    let mut session = Session::new();
    session.keys(":open a.txt<ret>:open other.txt<ret>").await;
    session.start_watch().await;
    edit("a.txt", 6, "a6 agent");
    session.wait_for_focus("a.txt", 5).await;
    let doc = session
        .app
        .editor
        .document_by_path(cwd().join("a.txt"))
        .unwrap();
    assert!(!doc.is_modified());
    assert_eq!(doc.text().line(5).to_string(), "a6 agent\n");
    // the gutter has the hunk
    assert_eq!(doc.watch_diff().expect("a watch gutter").load().len(), 1);
}

// 10. The picker

#[tokio::test(flavor = "multi_thread")]
async fn the_picker_lists_the_newest_file_and_hunk_first_and_enter_goes_to_it() {
    let Some(_) = enter("the_picker_lists_the_newest_file_and_hunk_first_and_enter_goes_to_it")
    else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 2, "a2 agent").await;
    session.change_and_follow("b.txt", 5, "b5 agent").await;
    session.change_and_follow("a.txt", 8, "a8 agent").await;

    session.keys(":watch-changes<ret>").await;
    // opening it pauses following
    assert!(!session.following());
    assert_eq!(
        status(&session.app),
        "This session: 3 changes in 2 files, +3 -3"
    );
    // a.txt changed last: first; in it the newest hunk first
    assert_eq!(
        session.list(),
        [
            "── a.txt  2 hunks  +2 -2",
            "-+ a.txt:8",
            "-+ a.txt:2",
            "── b.txt  1 hunk  +1 -1",
            "-+ b.txt:5",
        ]
    );
    assert_eq!(session.selected_row(), "-+ a.txt:8");

    // Enter on the second file's hunk
    session.keys("jj").await;
    assert_eq!(session.selected_row(), "-+ b.txt:5");
    session.keys("<ret>").await;
    assert!(
        !session.rows().iter().any(|row| row.starts_with('┌')),
        "the picker is still open"
    );
    assert_eq!(focus(&session.app), at("b.txt", 4));

    // and on an older hunk of the first
    session.keys(":watch-changes<ret>").await;
    session.keys("j").await;
    assert_eq!(session.selected_row(), "-+ a.txt:2");
    session.keys("<ret>").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    // n goes on from there, in the order they happened
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("a.txt", 7));
    assert!(session.following());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_picker_says_so_without_a_session_or_changes() {
    let Some(_) = enter("the_picker_says_so_without_a_session_or_changes") else {
        return;
    };
    let mut session = Session::new();
    session.keys(":watch-changes<ret>").await;
    assert_eq!(status(&session.app), "No watch session: :watch starts one");
    session.start_watch().await;
    session.keys(":watch-changes<ret>").await;
    assert_eq!(status(&session.app), "No changes this session");
    assert!(!session.rows().iter().any(|row| row.starts_with('┌')));
}

// 11. `vx --watch`

#[tokio::test(flavor = "multi_thread")]
async fn the_watch_flag_starts_watch_mode() {
    let Some(_) = enter("the_watch_flag_starts_watch_mode") else {
        return;
    };
    let app = super::helpers::AppBuilder::new()
        .with_watch()
        .build()
        .unwrap();
    let mut session = Session::with_app(app);
    assert!(session.watching());
    assert!(session.following());
    assert!(session_of(&session.app).is_empty());
    session.run_for(Duration::from_millis(300)).await;
    session.change_and_follow("a.txt", 5, "a5 agent").await;
    assert_eq!(hunks(&session.app, "a.txt"), [(4, 5)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_watch_flag_nothing_is_watched() {
    let Some(_) = enter("without_the_watch_flag_nothing_is_watched") else {
        return;
    };
    let session = Session::new();
    assert!(!session.watching());
    assert!(session.app.editor.watch.session.is_none());
}

// 12. No network

#[tokio::test(flavor = "multi_thread")]
async fn watch_starts_promptly_with_a_remote_that_cannot_be_reached() {
    let Some(_) = enter("watch_starts_promptly_with_a_remote_that_cannot_be_reached") else {
        return;
    };
    // the repository has the remote and the tracking branch
    assert!(git(&cwd(), "", &["remote", "-v"]).contains("127.0.0.1:9"));
    assert_eq!(
        git(&cwd(), "", &["config", "branch.main.remote"]).trim(),
        "origin"
    );
    let mut session = Session::new();
    let start = Instant::now();
    session.keys(":watch<ret>").await;
    let took = start.elapsed();
    assert!(session.watching(), "{}", status(&session.app));
    assert!(took < Duration::from_secs(1), ":watch took {took:?}");
    session.run_for(Duration::from_millis(300)).await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    assert_eq!(hunks(&session.app, "a.txt"), [(3, 4)]);
    // following needed no network either
    assert!(start.elapsed() < Duration::from_secs(15));
}

// 13. Commands

#[tokio::test(flavor = "multi_thread")]
async fn watch_twice_toggles_and_watch_stop_says_when_it_is_not_watching() {
    let Some(_) = enter("watch_twice_toggles_and_watch_stop_says_when_it_is_not_watching") else {
        return;
    };
    let mut session = Session::new();
    session.keys(":watch-stop<ret>").await;
    assert_eq!(status(&session.app), "Not watching");
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.keys(":watch<ret>").await;
    assert!(!session.watching());
    assert!(
        status(&session.app).starts_with("Watch ended"),
        "{}",
        status(&session.app)
    );
    assert!(session.app.editor.watch.session.is_some());
    session.keys(":watch-stop<ret>").await;
    assert_eq!(status(&session.app), "Not watching");
    // :watch again starts a session
    session.keys(":watch<ret>").await;
    assert!(session.watching());
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_clear_says_what_it_did() {
    let Some(_) = enter("watch_clear_says_what_it_did") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.keys(":watch-clear<ret>").await;
    assert!(session.app.editor.watch.session.is_none());
    assert!(!session.watching());
    assert!(!session.on_screen("WATCH"));
    // nothing is watched any more
    let before = focus(&session.app);
    edit("b.txt", 2, "b2 agent");
    session.run_for(Duration::from_millis(800)).await;
    assert_eq!(focus(&session.app), before);
    session.keys(":watch-clear<ret>").await;
    assert!(session.app.editor.watch.session.is_none());
}

/// A directory that is not in a repository.
fn make_plain(parent: &Path) -> PathBuf {
    let work = parent.join("plain");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    // (the shell is how another process makes files)
    assert!(Command::new("sh")
        .args(["-c", "printf 'b\\n' > b.txt"])
        .current_dir(&work)
        .status()
        .unwrap()
        .success());
    work
}

fn enter_plain(test: &str) -> Option<PathBuf> {
    Repo::enter_with(module_path!(), test, make_plain)
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_outside_a_repository_is_an_error() {
    let Some(_) = enter_plain("watch_outside_a_repository_is_an_error") else {
        return;
    };
    let mut session = Session::new();
    session.keys(":watch<ret>").await;
    let error = is_error(&session.app).expect("an error");
    assert!(error.contains("not in a git repository"), "{error}");
    assert!(session.app.editor.watch.session.is_none());
    assert!(!session.watching());
    // nothing follows
    write("a.txt", "changed\n");
    session.run_for(Duration::from_millis(600)).await;
    assert_eq!(focus(&session.app).0, "[scratch]");
    session.keys(":watch-changes<ret>").await;
    assert_eq!(status(&session.app), "No watch session: :watch starts one");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_watch_flag_outside_a_repository_is_an_error_and_the_editor_still_starts() {
    let Some(_) =
        enter_plain("the_watch_flag_outside_a_repository_is_an_error_and_the_editor_still_starts")
    else {
        return;
    };
    let app = super::helpers::AppBuilder::new()
        .with_watch()
        .build()
        .unwrap();
    let mut session = Session::with_app(app);
    assert!(session.app.editor.watch.session.is_none());
    assert!(is_error(&session.app).is_some_and(|error| error.contains("not in a git repository")));
    session.keys("ihello<esc>").await;
    let (_, doc) = current_ref!(session.app.editor);
    assert_eq!(doc.text().to_string(), "hello\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_while_watching_changes_nothing_of_the_session() {
    let Some(_) = enter("a_commit_while_watching_changes_nothing_of_the_session") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    assert_eq!(hunks(&session.app, "a.txt"), [(3, 4)]);
    // the baseline is the commit the watch started on: committing the change keeps it listed
    commit_all("Agent work");
    session.run_for(Duration::from_millis(800)).await;
    assert_eq!(hunks(&session.app, "a.txt"), [(3, 4)]);
    assert!(session.watching());
}

// History and bookmarks

/// The places the focused view's history holds.
fn history_len(app: &Application) -> usize {
    app.editor.tree.get(app.editor.tree.focus).history.len()
}

#[tokio::test(flavor = "multi_thread")]
async fn following_and_n_record_no_history_jumps() {
    let Some(_) = enter("following_and_n_record_no_history_jumps") else {
        return;
    };
    let mut session = Session::new();
    // following goes through three files: none of it is a jump
    session.three_changes().await;
    assert_eq!(history_len(&session.app), 0);

    // nor are n and N through the session's changes, across files
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    session.keys("nN").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    assert_eq!(history_len(&session.app), 0);
    assert!(session.watching());

    // so ga, which ends watch mode, has nowhere to go back to
    session.keys("ga").await;
    assert!(!session.watching());
    assert_eq!(status(&session.app), "No jumps to go back to");
    assert_eq!(focus(&session.app), at("a.txt", 1));

    // n after the end still goes through the session's changes, without recording either
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    assert_eq!(history_len(&session.app), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn setting_a_bookmark_neither_pauses_nor_ends_watch_mode() {
    let Some(_) = enter("setting_a_bookmark_neither_pauses_nor_ends_watch_mode") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;
    session.keys("<C-s>").await;
    assert!(session.watching());
    assert!(session.following());
    assert_eq!(session.app.editor.bookmarks().len(), 1);
    // and removing it
    session.keys("<C-s>").await;
    assert!(session.following());
    assert!(session.app.editor.bookmarks().is_empty());

    // following goes on
    session.change_and_follow("a.txt", 6, "a6 agent").await;
    assert!(session.following());
}

#[tokio::test(flavor = "multi_thread")]
async fn ga_ends_watch_mode_but_keeps_the_changes() {
    let Some(_) = enter("ga_ends_watch_mode_but_keeps_the_changes") else {
        return;
    };
    let session = ended_by("ga").await;
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn going_round_the_bookmarks_ends_watch_mode_but_keeps_the_changes() {
    let Some(_) = enter("going_round_the_bookmarks_ends_watch_mode_but_keeps_the_changes") else {
        return;
    };
    let session = ended_by("<C-i>").await;
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bookmark_picker_ends_watch_mode_but_keeps_the_changes() {
    let Some(_) = enter("the_bookmark_picker_ends_watch_mode_but_keeps_the_changes") else {
        return;
    };
    let mut session = ended_by("<space>j").await;
    session.keys("<esc>").await;
    changes_stay_after_the_end(session).await;
}
