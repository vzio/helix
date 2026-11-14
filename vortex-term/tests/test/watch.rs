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

use termina::event::{Event, Modifiers, MouseButton, MouseEvent, MouseEventKind};
use vortex_term::{
    application::Application,
    watch::{LEAVE_TO_EDIT, LOOK_TO_MOVE},
};
use vortex_view::{
    current_ref,
    document::Mode,
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

    /// The note on the right of the mode bar: the session's latest activity.
    fn activity(&self) -> String {
        session_of(&self.app).activity.clone().unwrap_or_default()
    }

    /// The text of the focused buffer.
    fn text(&self) -> String {
        let (_, doc) = current_ref!(self.app.editor);
        doc.text().to_string()
    }

    /// The anchor and head of the primary selection of the focused buffer.
    fn selection(&self) -> (usize, usize) {
        let (view, doc) = current_ref!(self.app.editor);
        let range = doc.selection(view.id).primary();
        (range.anchor, range.head)
    }

    /// The cell (column, row) where `text` starts on the screen.
    fn cell_of(&self, text: &str) -> (u16, u16) {
        self.rows()
            .iter()
            .enumerate()
            .find_map(|(y, row)| {
                row.find(text)
                    .map(|byte| (row[..byte].chars().count() as u16, y as u16))
            })
            .unwrap_or_else(|| panic!("no {text:?} on the screen:\n{}", self.rows().join("\n")))
    }

    /// Presses the left mouse button on the cell `skip` columns after where `text` starts.
    async fn click_on(&mut self, text: &str, skip: u16) {
        let (column, row) = self.cell_of(text);
        self.click_cell(column + skip, row).await;
    }

    /// Presses the left mouse button on a cell of the screen.
    async fn click_cell(&mut self, column: u16, row: u16) {
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: Modifiers::NONE,
        };
        self.tx.send(Ok(Event::Mouse(event))).unwrap();
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    /// Nothing is printed to the status line during a watch session.
    fn assert_quiet(&self, when: &str) {
        assert_eq!(
            self.app
                .editor
                .get_status()
                .map(|(message, _)| message.to_string()),
            None,
            "a message in the status line {when}"
        );
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
    assert!(session.on_screen("WATCH ● following"));

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
    assert!(session.on_screen("WATCH ● following"));
    // n/N don't go there
    assert_eq!(session_of(&session.app).hunks_in_order().len(), 1);

    // looking around, it isn't counted as a new change either
    session.keys("vj").await;
    assert!(!session.following());
    write("Cargo.lock", &lines("again", 45));
    session.run_for(Duration::from_millis(600)).await;
    assert_eq!(session.new_count(), 0);
    assert!(session.on_screen("WATCH ◌ looking"));

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
    // what it watches is said on the bar, not in the status line
    session.assert_quiet("when the watch started");
    let activity = session.activity();
    assert!(activity.starts_with("watching "), "{activity}");
    // the dirty and the untracked file
    assert!(
        activity.contains("2 files already uncommitted"),
        "{activity}"
    );

    session.change_and_follow("b.txt", 7, "b7 agent").await;
    assert!(session.following());
    session.wait_for_screen_text("WATCH ● following").await;
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

// 6. Following holds movement, `v` looks around

/// Following a change in a.txt, then `keys`: nothing moved, nothing was typed, following is
/// still on, and the bar says that `v` looks around.
async fn held_while_following(keys: &str) {
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    assert!(session.following());
    let (text, cursor, at) = (session.text(), session.selection(), focus(&session.app));

    session.keys(keys).await;
    assert!(session.following(), "{keys} stopped following");
    assert!(session.watching());
    assert_eq!(session.selection(), cursor, "{keys} moved the cursor");
    assert_eq!(focus(&session.app), at);
    assert_eq!(session.text(), text);
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    assert_eq!(session.activity(), LOOK_TO_MOVE);
    assert!(session.on_screen(LOOK_TO_MOVE));
    assert!(session.on_screen("WATCH ● following"));
    session.assert_quiet(&format!("after {keys}"));

    // and a change is still followed
    session.change_and_follow("b.txt", 6, "b6 agent").await;
    assert!(session.following());
}

macro_rules! held_on {
    ($name:ident, $keys:expr) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let Some(_) = enter(stringify!($name)) else {
                return;
            };
            held_while_following($keys).await;
        }
    };
}

held_on!(moving_with_j_is_held_while_following, "j");
held_on!(gg_is_held_while_following, "gg");
held_on!(a_word_motion_is_held_while_following, "w");
held_on!(scrolling_is_held_while_following, "<C-d>");
// (`/` is held, so the rest is typed in normal mode: held too, or harmless)
held_on!(starting_a_search_is_held_while_following, "/");

#[tokio::test(flavor = "multi_thread")]
async fn a_whole_search_typed_while_following_changes_nothing() {
    let Some(_) = enter("a_whole_search_typed_while_following_changes_nothing") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    let (text, cursor) = (session.text(), session.selection());
    session.keys("/a8<ret>").await;
    assert!(session.following());
    assert!(session.watching());
    assert_eq!(session.selection(), cursor);
    assert_eq!(session.text(), text);
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    session.assert_quiet("after the search");
}

/// `v`, then `keys`: you move, following is off, and changes are counted, not followed.
async fn looks_around_with(keys: &str) {
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    assert!(session.following());

    session.keys("v").await;
    assert!(!session.following());
    assert!(session.watching());
    session.wait_for_screen_text("WATCH ◌ looking").await;
    session.assert_quiet("after v");
    // the badge now offers to follow again
    assert!(session.on_screen(" v  follow"));
    let cursor = session.selection();

    session.keys(keys).await;
    assert_ne!(session.selection(), cursor, "{keys} didn't move");
    assert!(!session.following());
    assert!(session.watching());
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    let before = focus(&session.app);

    // a change now doesn't take the view away
    edit("b.txt", 6, "b6 agent");
    session
        .wait_watch("b.txt counted as new", |app| {
            session_of(app).new_count() == 1
        })
        .await;
    session.wait_for_screen_text("1 new").await;
    assert_eq!(focus(&session.app), before);
    assert!(!session.following());

    std::thread::sleep(MTIME_GAP);
    edit("c.txt", 2, "c2 agent");
    session
        .wait_watch("c.txt counted as new", |app| {
            session_of(app).new_count() == 2
        })
        .await;
    session.wait_for_screen_text("2 new").await;
    assert_eq!(focus(&session.app), before);
    session.assert_quiet("while looking");
}

macro_rules! looks_with {
    ($name:ident, $keys:expr) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let Some(_) = enter(stringify!($name)) else {
                return;
            };
            looks_around_with($keys).await;
        }
    };
}

looks_with!(j_moves_while_looking, "j");
looks_with!(gg_moves_while_looking, "gg");
looks_with!(a_word_motion_moves_while_looking, "w");
looks_with!(scrolling_moves_while_looking, "<C-d>");
looks_with!(a_search_moves_while_looking, "/a8<ret>");

#[tokio::test(flavor = "multi_thread")]
async fn v_again_follows_again_from_the_newest_change() {
    let Some(_) = enter("v_again_follows_again_from_the_newest_change") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.keys("vj").await;
    assert!(!session.following());
    // two changes while looking: counted, the view stays
    edit("b.txt", 6, "b6 agent");
    std::thread::sleep(MTIME_GAP);
    edit("c.txt", 2, "c2 agent");
    session
        .wait_watch("two new changes", |app| session_of(app).new_count() == 2)
        .await;
    assert_eq!(focus(&session.app), at("a.txt", 4));

    // v: following again, on the newest change, which is no longer new
    session.keys("v").await;
    assert!(session.following());
    assert_eq!(focus(&session.app), at("c.txt", 1));
    assert_eq!(session.new_count(), 0);
    session.wait_for_screen_text("WATCH ● following").await;
    assert!(session.on_screen(" v  look"));
    assert_eq!(session.activity(), "following again");
    session.assert_quiet("after v again");

    // and it follows the next change
    session.change_and_follow("other.txt", 5, "o5 agent").await;
    assert!(session.following());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mouse_in_the_text_looks_around() {
    let Some(_) = enter("the_mouse_in_the_text_looks_around") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    // a click on the text (the first row, well away from the bar)
    session.click_cell(3, 0).await;
    assert!(!session.following());
    assert!(session.watching());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_line_keeps_following_and_esc_leaves() {
    let Some(_) = enter("the_command_line_keeps_following_and_esc_leaves") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    // opening the command line and leaving it changes nothing
    session.keys(":<esc>").await;
    assert!(session.following());
    assert!(session.watching());
    // the watch commands keep watch mode: the picker looks around
    session.keys(":watch-changes<ret>").await;
    assert!(session.watching());
    assert!(!session.following());
    session.keys("<esc>").await;
    assert!(session.watching());
    // `:goto` looks around
    session.keys("v:goto 7<ret>").await;
    assert!(session.watching());
    assert!(!session.following());
    assert_eq!(focus(&session.app), at("a.txt", 6));
    // Esc, in the text, leaves watch mode
    session.keys("<esc>").await;
    assert!(!session.watching());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_typed_command_other_than_the_watch_commands_leaves_watch_mode() {
    let Some(_) = enter("a_typed_command_other_than_the_watch_commands_leaves_watch_mode") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;
    session.keys(":goto 3<ret>").await;
    // :goto only looks around
    assert!(session.watching());
    assert!(!session.following());
    assert_eq!(focus(&session.app), at("c.txt", 2));
    session.keys(":open other.txt<ret>").await;
    assert!(!session.watching());
    assert!(session.app.editor.watch.session.is_some());
    assert_eq!(focus(&session.app).0, "other.txt");
}

// 7. n and N

#[tokio::test(flavor = "multi_thread")]
async fn n_and_n_go_through_the_changes_in_the_order_they_happened() {
    let Some(_) = enter("n_and_n_go_through_the_changes_in_the_order_they_happened") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;

    // n from the newest wraps around to the oldest and looks
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    assert_eq!(session.activity(), "wrapped around to the first change");
    assert!(!session.following());
    assert!(session.watching());
    session.assert_quiet("after n");
    // the newest changes are counted as new again? no: they were seen when followed
    assert_eq!(session.new_count(), 0);

    session.keys("n").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    assert!(!session.following());
    session.keys("n").await;
    // the newest: following again
    assert_eq!(focus(&session.app), at("c.txt", 7));
    assert!(session.following());
    assert_eq!(session.activity(), "at the newest change: following again");
    session.wait_for_screen_text("WATCH ● following").await;
    session.assert_quiet("after n reached the newest change");

    // N goes back: an older change is looked at, not followed
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    assert!(!session.following());
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    // N from the oldest wraps to the newest, which follows again
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("c.txt", 7));
    assert!(session.following());

    // a new change while looking is counted, and n goes to it and follows again
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

// 8. Keys that edit or open something are held, Esc leaves, the changes stay

/// Three changes, then `keys`, which would edit or open something: nothing happened, watch
/// mode is on and the bar says that `Esc` leaves it first. Then `Esc`: watch mode is over, the
/// changes are still there.
async fn held_then_left(keys: &str) -> Session {
    let mut session = Session::new();
    session.three_changes().await;
    held(&mut session, keys).await;
    session.keys("<esc>").await;
    assert!(!session.watching(), "Esc didn't leave watch mode");
    session.assert_quiet("after Esc");
    kept(session)
}

/// `keys` change nothing in watch mode (following or looking): the bar says why.
async fn held(session: &mut Session, keys: &str) {
    let (text, cursor, at, following) = (
        session.text(),
        session.selection(),
        focus(&session.app),
        session.following(),
    );
    session.keys(keys).await;
    assert!(session.watching(), "{keys} ended watch mode");
    assert_eq!(session.following(), following, "{keys} changed following");
    assert_eq!(session.text(), text, "{keys} changed the text");
    assert_eq!(session.selection(), cursor);
    assert_eq!(focus(&session.app), at);
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    assert_eq!(session.activity(), LEAVE_TO_EDIT, "{keys}");
    assert!(session.on_screen(LEAVE_TO_EDIT));
    assert!(
        !session.rows().iter().any(|row| row.starts_with('┌')),
        "{keys} opened a picker"
    );
    session.assert_quiet(&format!("after {keys}"));
}

/// Three changes, then `keys`: watch mode is over, the changes are still there.
async fn ended_by(keys: &str) -> Session {
    let mut session = Session::new();
    session.three_changes().await;
    session.keys(keys).await;
    assert!(!session.watching(), "{keys} didn't end watch mode");
    kept(session)
}

/// Watch mode is over: its changes and gutter markers stay.
fn kept(session: Session) -> Session {
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
async fn typing_text_is_held_until_esc_leaves_watch_mode() {
    let Some(_) = enter("typing_text_is_held_until_esc_leaves_watch_mode") else {
        return;
    };
    let session = held_then_left("i").await;
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_a_file_with_a_typed_command_ends_watch_mode_but_keeps_the_changes() {
    let Some(_) =
        enter("opening_a_file_with_a_typed_command_ends_watch_mode_but_keeps_the_changes")
    else {
        return;
    };
    let session = ended_by(":open other.txt<ret>").await;
    assert_eq!(focus(&session.app).0, "other.txt");
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picker_is_held_until_esc_leaves_watch_mode() {
    let Some(_) = enter("a_picker_is_held_until_esc_leaves_watch_mode") else {
        return;
    };
    let session = held_then_left("<space>f").await;
    changes_stay_after_the_end(session).await;
}

/// Edit and open keys, in both states: held, and `Esc` is the way out.
async fn edit_keys_are_held(looking: bool) {
    let mut session = Session::new();
    session.three_changes().await;
    if looking {
        session.keys("v").await;
        assert!(!session.following());
    }
    for keys in ["i", "d", "o", "<space>f"] {
        held(&mut session, keys).await;
        // the bar's note is for the next key
        session.app.editor.watch.session.as_mut().unwrap().activity = None;
    }
    // watch mode is still on, with its state: a change is followed or counted as before
    assert_eq!(session.following(), !looking);
    session.keys("<esc>").await;
    assert!(!session.watching());
    // now the keys are the editor's
    session.keys("ix<esc>").await;
    assert!(session.text().contains('x'));
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_keys_are_held_while_following() {
    let Some(_) = enter("edit_keys_are_held_while_following") else {
        return;
    };
    edit_keys_are_held(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_keys_are_held_while_looking() {
    let Some(_) = enter("edit_keys_are_held_while_looking") else {
        return;
    };
    edit_keys_are_held(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_leaves_watch_mode_and_keeps_the_changes_and_says_nothing() {
    let Some(_) = enter("esc_leaves_watch_mode_and_keeps_the_changes_and_says_nothing") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;
    session.keys("<esc>").await;
    assert!(!session.watching());
    session.assert_quiet("after Esc");
    assert!(session.app.editor.watch.session.is_some());
    assert_eq!(session_of(&session.app).hunks_in_order().len(), 3);
    assert!(!session.on_screen("WATCH"));
    assert!(!session.on_screen(" v  look"));
    // the files are no longer watched
    let before = focus(&session.app);
    edit("other.txt", 3, "o3 agent");
    session.run_for(Duration::from_millis(800)).await;
    assert!(file(&session.app, "other.txt").is_none());
    assert_eq!(focus(&session.app), before);
    // the keys of the editor are back, `v` is select mode again
    session.keys("v").await;
    assert_eq!(session.app.editor.mode(), Mode::Select);
    session.keys("<esc>").await;
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
    assert_eq!(status(&session.app), "Watch ended: 3 changes");
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
    // looking around: the search moves the cursor
    session.keys("v/a<ret>").await;
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
    // opening it looks around
    assert!(!session.following());
    session.assert_quiet("when the picker opened");
    assert!(
        session.on_screen("this session: 3 change"),
        "{}",
        session.rows().join("\n")
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
    assert_eq!(is_error(&session.app).as_deref(), Some("no watch session"));
    session.start_watch().await;
    session.keys(":watch-changes<ret>").await;
    assert_eq!(status(&session.app), "No changes this session");
    assert!(!session.rows().iter().any(|row| row.starts_with('┌')));
}

// 10b. The mode bar

#[tokio::test(flavor = "multi_thread")]
async fn the_mode_bar_is_drawn_while_watching_and_gone_after_esc() {
    let Some(_) = enter("the_mode_bar_is_drawn_while_watching_and_gone_after_esc") else {
        return;
    };
    let mut session = Session::new();
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.wait_for_screen_text("WATCH ● following").await;
    for text in [
        " v  look",
        " n  next",
        " N  previous",
        " c  changes",
        " Esc  leave",
    ] {
        assert!(
            session.on_screen(text),
            "no {text:?}:\n{}",
            session.rows().join("\n")
        );
    }
    // the current change, and the activity
    assert!(session.on_screen("a.txt"));
    assert!(session.on_screen("1/1 change"));
    assert!(session.on_screen("1 file"));
    assert!(session.on_screen("followed a.txt:4"));
    // the default statusline doesn't repeat it as text
    assert!(!session.on_screen("WATCH following"));
    assert!(!session.on_screen("WATCH paused"));

    // looking: the state and the first badge change, and what is new is counted
    session.keys("v").await;
    assert!(session.on_screen("WATCH ◌ looking"));
    assert!(!session.on_screen("WATCH ● following"));
    assert!(session.on_screen(" v  follow"));
    assert!(!session.on_screen(" v  look "));
    edit("b.txt", 6, "b6 agent");
    session.wait_for_screen_text("1 new").await;

    session.keys("<esc>").await;
    assert!(!session.watching());
    for text in ["WATCH", " v  ", " n  next", " Esc  leave", "1 new"] {
        assert!(
            !session.on_screen(text),
            "{text:?} is still drawn:\n{}",
            session.rows().join("\n")
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn clicking_a_badge_runs_its_command() {
    let Some(_) = enter("clicking_a_badge_runs_its_command") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;
    session.run_for(Duration::from_millis(100)).await;

    // the label of the first badge looks around ...
    session.click_on(" v  look", 4).await;
    assert!(!session.following());
    assert!(session.watching());
    assert!(session.on_screen("WATCH ◌ looking"));
    // ... and then follows again, from the newest change
    session.click_on(" v  follow", 4).await;
    assert!(session.following());
    assert_eq!(focus(&session.app), at("c.txt", 7));

    // the key of a badge works as well as its label
    session.click_on(" n  next", 1).await;
    assert_eq!(focus(&session.app), at("a.txt", 1));
    assert!(!session.following());
    session.click_on(" N  previous", 6).await;
    assert_eq!(focus(&session.app), at("c.txt", 7));

    // leave
    session.click_on(" Esc  leave", 7).await;
    assert!(!session.watching());
    assert!(!session.on_screen("WATCH"));
    session.assert_quiet("after the clicks");
}

/// The `changes` badge opens the picker, as the `c` key does.
#[tokio::test(flavor = "multi_thread")]
async fn clicking_the_changes_badge_opens_the_picker() {
    let Some(_) = enter("clicking_the_changes_badge_opens_the_picker") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;
    session.run_for(Duration::from_millis(100)).await;
    session.click_on(" c  changes", 4).await;
    session.wait_for_screen_text("c.txt  1 hunk").await;
    assert_eq!(session.list().len(), 6);
    assert!(session.watching());
    assert!(!session.following());
    session.keys("<esc>").await;
    assert!(session.watching());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_remapped_in_keys_watch_shows_on_the_bar_and_works() {
    let Some(_) = enter("a_key_remapped_in_keys_watch_shows_on_the_bar_and_works") else {
        return;
    };
    let toml =
        "[keys.watch]\nl = \"watch_look\"\nv = \"no_op\"\nx = \"watch_leave\"\nesc = \"no_op\"\n";
    let loaded = vortex_term::config::Config::load(
        Ok(&toml.to_string()),
        Err(vortex_term::config::ConfigLoadError::default()),
    )
    .unwrap();
    let mut config = super::helpers::test_config();
    config.layers = loaded.layers;
    let mut session = Session::with_config(config);
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;

    assert!(
        session.on_screen(" l  look"),
        "{}",
        session.rows().join("\n")
    );
    assert!(!session.on_screen(" v  look"));
    assert!(session.on_screen(" x  leave"));
    // the defaults that weren't remapped are still there
    assert!(session.on_screen(" n  next"));

    // v is nothing now, l looks around
    session.keys("v").await;
    assert!(session.following());
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    session.keys("l").await;
    assert!(!session.following());
    assert!(session.on_screen(" l  follow"));
    session.keys("l").await;
    assert!(session.following());
    // and x leaves
    session.keys("x").await;
    assert!(!session.watching());
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_printed_to_the_status_line_during_a_watch_session() {
    let Some(_) = enter("nothing_is_printed_to_the_status_line_during_a_watch_session") else {
        return;
    };
    let mut session = Session::new();
    session.assert_quiet("before the watch");
    session.start_watch().await;
    session.assert_quiet("when the watch started");
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.assert_quiet("after following a change");
    session.change_and_follow("b.txt", 6, "b6 agent").await;
    session.keys("n").await;
    session.assert_quiet("after n");
    session.keys("N").await;
    session.assert_quiet("after N");
    session.keys("v").await;
    session.assert_quiet("after v");
    session.keys("j").await;
    session.assert_quiet("after a move");
    session.keys("v").await;
    session.assert_quiet("after v again");
    session.keys("d").await;
    session.assert_quiet("after a held key");
    session.keys("c").await;
    session.keys("<esc>").await;
    session.assert_quiet("after the picker");
    session.run_for(Duration::from_millis(300)).await;
    session.assert_quiet("a moment later");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_key_opens_the_picker_of_the_changes_and_looks() {
    let Some(_) = enter("the_c_key_opens_the_picker_of_the_changes_and_looks") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;
    session.keys("c").await;
    assert!(session.watching());
    assert!(!session.following());
    assert_eq!(session.list().len(), 6);
    // enter goes to the change (the newest first: c.txt)
    session.keys("j<ret>").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    assert!(session.watching());
}

#[tokio::test(flavor = "multi_thread")]
async fn n_to_the_newest_change_follows_and_n_to_an_older_one_looks() {
    let Some(_) = enter("n_to_the_newest_change_follows_and_n_to_an_older_one_looks") else {
        return;
    };
    let mut session = Session::new();
    session.three_changes().await;
    // N: an older change, looking
    session.keys("N").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    assert!(!session.following());
    assert!(session.on_screen("WATCH ◌ looking"));
    // n: the newest again, following
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("c.txt", 7));
    assert!(session.following());
    assert!(session.on_screen("WATCH ● following"));
    // a change is followed again
    session.change_and_follow("other.txt", 5, "o5 agent").await;
    assert!(session.following());
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
    assert!(
        is_error(&session.app).is_some_and(|error| error.ends_with("watch mode is off")),
        "{:?}",
        is_error(&session.app)
    );
    session.start_watch().await;
    session.change_and_follow("a.txt", 4, "a4 agent").await;
    session.keys(":watch<ret>").await;
    assert!(!session.watching());
    assert_eq!(status(&session.app), "Watch ended: 1 change");
    assert!(session.app.editor.watch.session.is_some());
    session.keys(":watch-stop<ret>").await;
    assert!(
        is_error(&session.app).is_some_and(|error| error.ends_with("watch mode is off")),
        "{:?}",
        is_error(&session.app)
    );
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
    assert_eq!(
        status(&session.app),
        "Watch ended and its changes forgotten"
    );
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
    assert_eq!(is_error(&session.app).as_deref(), Some("no watch session"));
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

    // ga is held while watching, and after Esc it has nowhere to go back to
    held(&mut session, "ga").await;
    session.keys("<esc>").await;
    assert!(!session.watching());
    assert_eq!(focus(&session.app), at("a.txt", 1));

    // n after the end still goes through the session's changes, without recording either
    session.keys("n").await;
    assert_eq!(focus(&session.app), at("b.txt", 4));
    assert_eq!(history_len(&session.app), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn setting_a_bookmark_neither_looks_nor_ends_watch_mode() {
    let Some(_) = enter("setting_a_bookmark_neither_looks_nor_ends_watch_mode") else {
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
async fn ga_is_held_until_esc_leaves_watch_mode() {
    let Some(_) = enter("ga_is_held_until_esc_leaves_watch_mode") else {
        return;
    };
    let session = held_then_left("ga").await;
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn going_round_the_bookmarks_is_held_until_esc_leaves_watch_mode() {
    let Some(_) = enter("going_round_the_bookmarks_is_held_until_esc_leaves_watch_mode") else {
        return;
    };
    let session = held_then_left("<C-i>").await;
    changes_stay_after_the_end(session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bookmark_picker_is_held_until_esc_leaves_watch_mode() {
    let Some(_) = enter("the_bookmark_picker_is_held_until_esc_leaves_watch_mode") else {
        return;
    };
    let session = held_then_left("<space>j").await;
    changes_stay_after_the_end(session).await;
}
