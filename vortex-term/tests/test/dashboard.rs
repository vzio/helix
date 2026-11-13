//! Tests of the dashboard (this fork): when it is up, which keys it takes, what makes it vanish,
//! and that it doesn't delay the first frame.
//!
//! The dashboard is drawn by the editor view, which the tests can't reach, so they look at the
//! screen: it is up when its menu is on it. The working directory is the one of the process,
//! which tests can't change, so the tests that need another (outside of a repository, in a
//! repository made for them, in this large repository) run themselves again in a process of
//! their own: see [`super::git_log::Repo::enter_with`].

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use vortex_loader::workspace_trust::WorkspaceTrust;
use vortex_term::{
    application::Application,
    args::Args,
    config::Config,
    dashboard::{
        stats::{tick, Stats},
        timeline::Timeline,
    },
};
use vortex_view::{doc, document::Mode, editor::DashboardBackground};

use super::git_log::{on_screen, Repo, Session};
use super::helpers::{temp_file_with_contents, test_config, test_syntax_loader};

/// A label of the menu of the dashboard.
const MENU: &str = "Start typing";
const HISTORY: &str = "Git history";

fn config(dashboard: bool) -> Config {
    let mut config = test_config();
    config.editor.dashboard = dashboard.into();
    config
}

/// A session on the dashboard, with the first frame drawn.
async fn session() -> Session {
    session_with(config(true)).await
}

async fn session_with(config: Config) -> Session {
    let mut session = Session::with_config(config);
    // (the picker previews files: the highlight scopes must be the theme's)
    session
        .app
        .editor
        .set_theme(vortex_view::Theme::from(
            toml::from_str::<toml::Value>(r##""ui.selection" = { bg = "#333333" }"##).unwrap(),
        ))
        .unwrap();
    session.first_frame().await;
    session
}

fn up(session: &Session) -> bool {
    on_screen(&session.rows(), MENU)
}

/// Whether a picker is on the screen.
fn picker(session: &Session) -> bool {
    session.rows().iter().any(|row| row.starts_with('┌'))
}

fn empty_scratch(session: &Session) -> bool {
    let doc = doc!(session.app.editor);
    doc.path().is_none() && !doc.is_modified() && doc.text().len_chars() <= 1
}

/// A new application with `files` as its arguments (a folder too, which the builder of the
/// tests doesn't take).
fn application(files: &[&Path], config: Config) -> Application {
    let mut args = Args::default();
    for file in files {
        args.files
            .insert(file.to_path_buf(), vec![Default::default()]);
    }
    Application::new(
        args,
        config,
        test_syntax_loader(None),
        WorkspaceTrust::fully_trusted(),
    )
    .unwrap()
}

/// A directory outside of any repository, as the working directory.
fn plain_directory(parent: &Path) -> PathBuf {
    let dir = parent.join("plain");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn it_is_up_after_startup() {
    let session = session().await;
    assert!(up(&session), "{:#?}", session.rows());
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    assert!(empty_scratch(&session));
}

#[tokio::test(flavor = "multi_thread")]
async fn i_closes_it_and_starts_inserting() {
    let mut session = session().await;
    session.keys("i").await;
    assert!(!up(&session));
    assert_eq!(session.app.editor.mode(), Mode::Insert);
    session.keys("ab<esc>").await;
    assert_eq!(doc!(session.app.editor).text().to_string(), "ab\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_closes_it_and_stays_in_normal_mode() {
    let mut session = session().await;
    session.keys("<esc>").await;
    assert!(!up(&session));
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    assert!(empty_scratch(&session));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_that_edits_or_changes_the_mode_makes_it_vanish() {
    for keys in ["o", "a", "v"] {
        let mut session = session().await;
        assert!(up(&session));
        session.keys(keys).await;
        assert!(!up(&session), "still up after {keys:?}");
        assert_ne!(session.app.editor.mode(), Mode::Normal, "{keys:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_of_the_keymap_that_changes_nothing_keeps_it() {
    let mut session = session().await;
    // (`l` is the commit log when the directory is in a repository)
    session.keys("hjkwb").await;
    assert!(up(&session));
    // nor does a command line over it, until it is run
    session.keys(":").await;
    assert!(up(&session));
    session.keys("<esc>").await;
    assert!(up(&session));
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_a_file_replaces_it() {
    let file = temp_file_with_contents("hello\n").unwrap();
    let mut session = session().await;
    session
        .keys(&format!(":open {}<ret>", file.path().display()))
        .await;
    assert!(!up(&session), "{:#?}", session.rows());
    assert_eq!(
        doc!(session.app.editor).path().map(|p| p.to_path_buf()),
        Some(file.path().to_path_buf())
    );
    assert!(on_screen(&session.rows(), "hello"));
}

#[tokio::test(flavor = "multi_thread")]
async fn f_opens_the_file_picker_and_the_dashboard_is_there_after_it() {
    let mut session = session().await;
    session.keys("f").await;
    assert!(picker(&session), "{:#?}", session.rows());
    session.keys("<esc>").await;
    assert!(!picker(&session));
    assert!(up(&session), "{:#?}", session.rows());
    assert_eq!(session.app.editor.mode(), Mode::Normal);
}

#[tokio::test(flavor = "multi_thread")]
async fn e_opens_the_file_tree() {
    let mut session = session().await;
    let before = session.rows();
    session.keys("e").await;
    assert_ne!(session.rows(), before);
    assert_eq!(session.app.editor.mode(), Mode::Normal);
}

#[tokio::test(flavor = "multi_thread")]
async fn q_quits() {
    // (`q` records a macro for the keymap)
    let mut session = session().await;
    assert!(session.quits_on("q").await);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_brings_it_back_over_a_new_buffer() {
    let file = temp_file_with_contents("hello\n").unwrap();
    let mut session = session().await;
    session
        .keys(&format!(":open {}<ret>ox<esc>", file.path().display()))
        .await;
    assert!(!up(&session));
    assert!(doc!(session.app.editor).is_modified());
    assert_eq!(session.app.editor.documents().count(), 1);

    session.keys(":dashboard<ret>").await;
    session
        .wait_for_screen("the dashboard", |rows| on_screen(rows, MENU))
        .await;
    assert!(empty_scratch(&session));
    // the edited buffer still exists
    assert_eq!(session.app.editor.documents().count(), 2);
    assert!(session
        .app
        .editor
        .documents()
        .any(|doc| doc.is_modified() && doc.path().is_some()));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_over_an_empty_buffer_does_not_add_one() {
    let mut session = session().await;
    session.keys("<esc>:dashboard<ret>").await;
    session
        .wait_for_screen("the dashboard", |rows| on_screen(rows, MENU))
        .await;
    assert_eq!(session.app.editor.documents().count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn turned_off_there_is_none_at_startup() {
    let mut session = Session::with_config(config(false));
    session.first_frame().await;
    assert!(!up(&session));
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    // but the command shows it
    session.keys(":dashboard<ret>").await;
    session
        .wait_for_screen("the dashboard", |rows| on_screen(rows, MENU))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_argument_never_shows_it() {
    let file = temp_file_with_contents("hello\n").unwrap();
    let mut session = Session::with_app(application(&[file.path()], config(true)));
    session.first_frame().await;
    assert!(!up(&session));
    assert!(on_screen(&session.rows(), "hello"));
}

#[tokio::test(flavor = "multi_thread")]
async fn two_arguments_never_show_it() {
    let one = temp_file_with_contents("one\n").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::with_app(application(&[dir.path(), one.path()], config(true)));
    session.first_frame().await;
    assert!(!up(&session));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_argument_shows_it_and_no_file_picker() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::with_app(application(&[dir.path()], config(true)));
    session.first_frame().await;
    assert!(up(&session), "{:#?}", session.rows());
    assert!(!picker(&session), "{:#?}", session.rows());
    assert!(empty_scratch(&session));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_argument_with_the_dashboard_off_opens_the_file_picker() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::with_app(application(&[dir.path()], config(false)));
    session.first_frame().await;
    assert!(!up(&session));
    assert!(picker(&session), "{:#?}", session.rows());
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_of_a_repository_l_is_the_keymaps_and_there_is_no_git_stat() {
    let Some(dir) = Repo::enter_with(
        module_path!(),
        "outside_of_a_repository_l_is_the_keymaps_and_there_is_no_git_stat",
        plain_directory,
    ) else {
        return;
    };
    assert!(!vortex_term::dashboard::stats::in_git_repo(&dir));

    let mut session = session().await;
    assert!(up(&session));
    assert!(
        !on_screen(&session.rows(), HISTORY),
        "{:#?}",
        session.rows()
    );
    session.keys("l").await;
    // moved right, as in any buffer: no picker, and the dashboard has nothing to say about it
    assert!(!picker(&session), "{:#?}", session.rows());
    assert_eq!(session.app.editor.mode(), Mode::Normal);
    assert!(up(&session));

    // the stats of this directory
    let stats = vortex_term::dashboard::stats::load(
        dir,
        true,
        Arc::new(test_syntax_loader(None)),
        Default::default(),
        vortex_term::dashboard::Timeline::new(
            Instant::now(),
            vortex_view::editor::DashboardBackground::Off,
        ),
    );
    assert!(!stats.git);
    assert!(!stats.shows_git());
}

#[tokio::test(flavor = "multi_thread")]
async fn in_a_repository_l_opens_the_commit_log() {
    let Some(dir) = Repo::enter_in(module_path!(), "in_a_repository_l_opens_the_commit_log") else {
        return;
    };
    assert!(vortex_term::dashboard::stats::in_git_repo(&dir));

    let mut session = session().await;
    assert!(up(&session));
    // the stats of the repository arrive, and the action to its history is there from the start
    session
        .wait_for_screen("the history action", |rows| on_screen(rows, HISTORY))
        .await;
    assert!(!picker(&session));

    session.keys("l").await;
    session
        .wait_for_screen("the log", |rows| on_screen(rows, "Initial commit"))
        .await;
    assert!(picker(&session));
    session.keys("<esc>").await;
    assert!(!picker(&session));
    assert!(up(&session), "{:#?}", session.rows());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stats_of_a_repository_load_and_settle() {
    let Some(dir) = Repo::enter_in(module_path!(), "the_stats_of_a_repository_load_and_settle")
    else {
        return;
    };
    let born = Instant::now();
    let timeline = vortex_term::dashboard::Timeline::new(
        born,
        vortex_view::editor::DashboardBackground::Static,
    );
    let stats = vortex_term::dashboard::stats::load(
        dir,
        true,
        Arc::new(test_syntax_loader(None)),
        Default::default(),
        timeline,
    );
    assert!(stats.git && stats.shows_git());
    let deadline = Instant::now() + Duration::from_secs(20);
    while stats.languages_done.get().is_none() {
        assert!(Instant::now() < deadline, "the languages never finished");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (head, _) = loop {
        match stats.head.get() {
            Some(head) => break head,
            None => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    };
    assert_eq!(head.unwrap().branch.as_deref(), Some("feature"));
    assert!(stats.languages.lock().unwrap().finished);
    // settled once every stat is in and the title is open
    let after = born + Duration::from_secs(3);
    let end = Instant::now() + Duration::from_secs(5);
    while !timeline.settled(&stats, after) {
        assert!(Instant::now() < end, "never settled");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The repository this crate is in, a large one.
fn this_repository(_parent: &Path) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn median(times: &mut [Duration]) -> Duration {
    times.sort();
    times[times.len() / 2]
}

/// The time until the first frame is drawn: the application is built, and the frame drawn.
/// Also whether the language stat of the dashboard was still loading on it.
async fn first_frame(dashboard: bool) -> (Duration, bool, Session) {
    let start = Instant::now();
    let app = application(&[], config(dashboard));
    let mut session = Session::with_app(app);
    session.first_frame().await;
    let elapsed = start.elapsed();
    let rows = session.rows();
    let loading = on_screen(&rows, "LANGUAGES")
        && (on_screen(&rows, "listing files") || on_screen(&rows, " files"));
    assert_eq!(on_screen(&rows, MENU), dashboard, "{rows:#?}");
    (elapsed, loading, session)
}

#[tokio::test(flavor = "multi_thread")]
async fn dashboard_does_not_delay_the_first_frame() {
    let Some(dir) = Repo::enter_with(
        module_path!(),
        "dashboard_does_not_delay_the_first_frame",
        this_repository,
    ) else {
        return;
    };
    assert!(vortex_term::dashboard::stats::in_git_repo(&dir));
    const RUNS: usize = 5;

    // warm up: grammars, config and the file cache
    let (_, _, session) = first_frame(false).await;
    close(session).await;

    let mut off = vec![];
    let mut on = vec![];
    let mut loading_frames = 0;
    let mut loaded_after = vec![];
    // interleaved, so that the load of the machine is the same for both
    for _ in 0..RUNS {
        let (time, _, session) = first_frame(false).await;
        off.push(time);
        close(session).await;

        let (time, loading, mut session) = first_frame(true).await;
        on.push(time);
        loading_frames += loading as usize;
        // how long the stats take to be all in, after the first frame
        let start = Instant::now();
        session
            .wait_for_screen("the languages", |rows| {
                rows.iter()
                    .any(|row| row.contains("LANGUAGES") && row.contains(" lines"))
            })
            .await;
        loaded_after.push(start.elapsed());
        close(session).await;
    }
    let off = median(&mut off);
    let on = median(&mut on);
    let loaded = median(&mut loaded_after);
    println!(
        "first frame in {}: dashboard off {off:?}, on {on:?} (median of {RUNS}); \
         its stats were all in {loaded:?} after the first frame; \
         still loading on the first frame in {loading_frames} of {RUNS} runs",
        dir.display()
    );

    let added = on.saturating_sub(off);
    assert!(
        added < Duration::from_millis(50),
        "the dashboard added {added:?} to the first frame"
    );
    // the stats were not waited for: they weren't in yet on the first frame (unless the machine
    // read this repository before the frame was drawn, in which case the frame still took
    // less than reading it would have)
    assert!(
        loading_frames > 0 || added < loaded,
        "every first frame had the stats already: {added:?} added, {loaded:?} to load them"
    );
}

/// A session on the dashboard with the vortex spinning (the mode that never settles), with
/// the first frame drawn: the keys of the tests below are typed well before the title opens.
async fn animated_session() -> Session {
    let mut config = config(true);
    config.editor.dashboard_background = DashboardBackground::On;
    session_with(config).await
}

#[tokio::test(flavor = "multi_thread")]
async fn f_opens_the_picker_at_once_while_it_animates() {
    let mut session = animated_session().await;
    assert!(up(&session));
    // (the spinning vortex asks for frames all the time, so the loop is never idle: the key
    // is handled at once, and the picker is there long before this stops waiting)
    let _ = tokio::time::timeout(Duration::from_millis(300), session.keys("f")).await;
    assert!(picker(&session), "{:#?}", session.rows());
}

#[tokio::test(flavor = "multi_thread")]
async fn i_starts_inserting_at_once_while_it_animates() {
    let mut session = animated_session().await;
    session.keys("i").await;
    assert!(!up(&session));
    assert_eq!(session.app.editor.mode(), Mode::Insert);
    session.keys("ab<esc>").await;
    assert_eq!(doc!(session.app.editor).text().to_string(), "ab\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn q_quits_at_once_while_it_animates() {
    let mut session = animated_session().await;
    assert!(session.quits_on("q").await);
}

/// Starts `tick` for a dashboard outside a repository in `background`, counting its frames.
fn ticking(
    background: DashboardBackground,
) -> (Arc<Stats>, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let stats = Arc::new(Stats::default());
    let frames = Arc::new(AtomicUsize::new(0));
    let timeline = Timeline::new(Instant::now(), background);
    let counter = frames.clone();
    let task = tokio::spawn(tick(stats.clone(), timeline, move || {
        counter.fetch_add(1, Ordering::Relaxed);
    }));
    (stats, frames, task)
}

fn frames(frames: &AtomicUsize) -> usize {
    frames.load(Ordering::Relaxed)
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_are_asked_for_as_long_as_the_mode_moves() {
    let (on, on_frames, on_task) = ticking(DashboardBackground::On);
    let (_, load_frames, load_task) = ticking(DashboardBackground::Settle);
    let (_, static_frames, static_task) = ticking(DashboardBackground::Static);
    let (_, off_frames, off_task) = ticking(DashboardBackground::Off);

    // static and off stop at about 1.7s (1s minimum and the title opening)
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert!(static_task.is_finished() && off_task.is_finished());
    let (still, none) = (frames(&static_frames), frames(&off_frames));
    assert!(still > 10 && none > 10, "{still} {none}");
    // load is still slowing down at 3s
    assert!(!load_task.is_finished());
    assert!(!on_task.is_finished());

    // load stops at about 4.9s
    tokio::time::sleep(Duration::from_millis(3200)).await;
    assert!(load_task.is_finished());
    assert!(frames(&load_frames) > still);
    assert!(!on_task.is_finished(), "the on mode never settles");
    let (load_done, on_before) = (frames(&load_frames), frames(&on_frames));

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(frames(&load_frames), load_done);
    assert_eq!(frames(&static_frames), still);
    assert_eq!(frames(&off_frames), none);
    assert!(
        frames(&on_frames) > on_before + 5,
        "on keeps asking at 6.7s"
    );

    // closing the dashboard stops it
    on.closed.store(true, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(on_task.is_finished());
    let closed = frames(&on_frames);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(frames(&on_frames), closed);
}

async fn close(mut session: Session) {
    // dropping the dashboard stops its background work
    session.keys("<esc>").await;
    let _ = session.app.close().await;
}

// ---- worktrees ----

mod worktree_rows {
    use super::super::worktrees::{
        flat, fleet, fleet_alive, one_and_a_missing, only_a_missing, plain, shown, Fleet,
    };
    use super::*;

    const ACTION: &str = "Worktrees";

    /// The flat text of the screen, its rows joined (the dashboard wraps a long worktree row).
    fn text(session: &Session) -> String {
        session
            .rows()
            .iter()
            .map(|row| flat(row))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Waits for the stats of a repository to be in, with a bit more time for the rest.
    async fn settled(session: &mut Session) {
        session
            .wait_for_screen("the branch", |rows| {
                rows.iter()
                    .any(|row| row.contains("branch") && row.contains("main"))
                    && on_screen(rows, "all pushed")
            })
            .await;
        session.run_for(Duration::from_millis(700)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_rows_are_newest_first_with_their_times() {
        let Some(main) = Repo::enter_with(
            module_path!(),
            "the_rows_are_newest_first_with_their_times",
            fleet_alive,
        ) else {
            return;
        };
        let fleet = Fleet::at(main.parent().unwrap());
        let mut session = session().await;
        session
            .wait_for_screen("the worktrees", |rows| {
                rows.iter().filter(|row| row.contains(" ago")).count() >= 3
            })
            .await;
        let id = super::super::git_log::git(&fleet.detached, "", &["rev-parse", "--short", "HEAD"]);
        let text = text(&session);
        let expected = [
            format!("{} dirty 2h ago", shown(&fleet.dirty)),
            format!("{} ahead 3d ago", shown(&fleet.ahead)),
            format!(
                "{} detached at {} 1mo ago",
                shown(&fleet.detached),
                id.trim()
            ),
        ];
        let mut last = 0;
        for expected in &expected {
            let at = text
                .find(expected)
                .unwrap_or_else(|| panic!("{expected:?} not in {text:?}"));
            assert!(at > last, "{expected:?} is out of order in {text:?}");
            last = at;
        }
        // the first row carries the label, and there is no "and more" for three
        assert!(
            text.contains(&format!("worktrees {}", shown(&fleet.dirty))),
            "{text:?}"
        );
        assert!(!text.contains("more"), "{text:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_worktree_shows_missing_instead_of_branch_and_time() {
        let Some(main) = Repo::enter_with(
            module_path!(),
            "a_missing_worktree_shows_missing_instead_of_branch_and_time",
            one_and_a_missing,
        ) else {
            return;
        };
        let fleet = Fleet::at(main.parent().unwrap());
        let mut session = session().await;
        session
            .wait_for_screen("the worktrees", |rows| on_screen(rows, "3d ago"))
            .await;
        let text = text(&session);
        assert!(
            text.contains(&format!("{} alive 3d ago", shown(&fleet.ahead))),
            "{text:?}"
        );
        // the missing one is last, with `missing` instead of its branch and time
        let gone = format!("{} missing", shown(&fleet.gone));
        let at = text
            .find(&gone)
            .unwrap_or_else(|| panic!("{gone:?} not in {text:?}"));
        assert!(at > text.find("alive").unwrap());
        assert!(
            !text.contains("gone gone") && !text.contains(&format!("{} gone", shown(&fleet.gone)))
        );
        assert!(!text[at..].contains("ago"), "{text:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn t_is_shown_with_other_worktrees_and_opens_the_picker() {
        let Some(_) = Repo::enter_with(
            module_path!(),
            "t_is_shown_with_other_worktrees_and_opens_the_picker",
            fleet,
        ) else {
            return;
        };
        let mut session = session().await;
        session
            .wait_for_screen("the action", |rows| on_screen(rows, ACTION))
            .await;
        assert!(on_screen(&session.rows(), ":wt"), "{:#?}", session.rows());
        assert!(!picker(&session));

        session.keys("t").await;
        session
            .wait_for_screen("the picker", |rows| on_screen(rows, "others/dirty"))
            .await;
        assert!(picker(&session), "{:#?}", session.rows());
        // the same picker as the command's
        assert!(on_screen(&session.rows(), "unpushed"));
        session.keys("<esc>").await;
        assert!(!picker(&session));
        assert!(up(&session), "{:#?}", session.rows());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn t_is_shown_when_one_of_the_worktrees_is_missing() {
        let Some(_) = Repo::enter_with(
            module_path!(),
            "t_is_shown_when_one_of_the_worktrees_is_missing",
            one_and_a_missing,
        ) else {
            return;
        };
        let mut session = session().await;
        session
            .wait_for_screen("the action", |rows| on_screen(rows, ACTION))
            .await;
    }

    /// `t` is not an action here: no label, and the key is the keymap's (`t` waits for a
    /// character to move to).
    async fn no_t(session: &mut Session) {
        settled_without_worktrees(session).await;
        assert!(!on_screen(&session.rows(), ACTION), "{:#?}", session.rows());
        session.keys("t").await;
        assert!(!picker(session), "{:#?}", session.rows());
        session.keys("<esc>").await;
        assert!(!picker(session));
    }

    async fn settled_without_worktrees(session: &mut Session) {
        session
            .wait_for_screen("the history action", |rows| on_screen(rows, HISTORY))
            .await;
        // (the worktrees are read after the head and the others)
        session.run_for(Duration::from_millis(1000)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn t_is_not_shown_without_other_worktrees() {
        let Some(_) = Repo::enter_in(module_path!(), "t_is_not_shown_without_other_worktrees")
        else {
            return;
        };
        let mut session = session().await;
        no_t(&mut session).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn t_is_not_shown_with_only_a_missing_worktree() {
        let Some(_) = Repo::enter_with(
            module_path!(),
            "t_is_not_shown_with_only_a_missing_worktree",
            only_a_missing,
        ) else {
            return;
        };
        let mut session = session().await;
        no_t(&mut session).await;
        // the missing worktree is still listed
        assert!(
            on_screen(&session.rows(), "missing"),
            "{:#?}",
            session.rows()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn t_is_not_shown_outside_of_a_repository() {
        let Some(_) = Repo::enter_with(
            module_path!(),
            "t_is_not_shown_outside_of_a_repository",
            plain,
        ) else {
            return;
        };
        let mut session = session().await;
        session.run_for(Duration::from_millis(500)).await;
        assert!(up(&session));
        assert!(!on_screen(&session.rows(), ACTION), "{:#?}", session.rows());
        session.keys("t<esc>").await;
        assert!(!picker(&session));
    }

    /// The text of the row of the dashboard labelled `label`.
    fn row_of(session: &Session, label: &str) -> String {
        session
            .rows()
            .iter()
            .map(|row| flat(row))
            .find(|row| row.starts_with(label) || row.contains(&format!(" {label} ")))
            .unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_dashboard_follows_cd() {
        let Some(main) = Repo::enter_with(module_path!(), "the_dashboard_follows_cd", fleet) else {
            return;
        };
        let fleet = Fleet::at(main.parent().unwrap());
        let mut session = session().await;
        settled(&mut session).await;
        assert!(row_of(&session, "branch").contains("main"));
        assert!(row_of(&session, "changes").contains("clean"));

        session
            .keys(&format!(":cd {}<ret>", fleet.dirty.display()))
            .await;
        session
            .wait_for_screen("the stats of dirty", |rows| {
                on_screen(rows, "1 staged · 1 unstaged · 1 untracked")
            })
            .await;
        assert!(up(&session), "{:#?}", session.rows());
        let branch = row_of(&session, "branch");
        assert!(
            branch.contains("dirty") && !branch.contains("main"),
            "{branch:?}"
        );
        // and back
        session
            .keys(&format!(":cd {}<ret>", fleet.main.display()))
            .await;
        session
            .wait_for_screen("the stats of main", |rows| {
                !on_screen(rows, "1 staged") && on_screen(rows, "all pushed")
            })
            .await;
        assert!(row_of(&session, "branch").contains("main"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_dashboard_follows_a_pick_in_the_picker() {
        let Some(_) = Repo::enter_with(
            module_path!(),
            "the_dashboard_follows_a_pick_in_the_picker",
            fleet,
        ) else {
            return;
        };
        let mut session = session().await;
        settled(&mut session).await;
        session.keys("t").await;
        session
            .wait_for_screen("every time", |rows| {
                rows.iter().filter(|row| row.contains(" ago")).count() >= 4
            })
            .await;
        // the second row: `dirty`
        session.keys("j<ret>").await;
        assert!(!picker(&session));
        session
            .wait_for_screen("the stats of dirty", |rows| {
                on_screen(rows, "1 staged · 1 unstaged · 1 untracked")
            })
            .await;
        assert!(up(&session), "{:#?}", session.rows());
        let branch = row_of(&session, "branch");
        assert!(
            branch.contains("dirty") && !branch.contains("main"),
            "{branch:?}"
        );
        // the worktrees it lists are the other ones now: from `dirty` there are four (main,
        // ahead, detached, gone), of which it has room for two
        session
            .wait_for_screen("the other worktrees", |rows| on_screen(rows, "and 2 more"))
            .await;
        let text = text(&session);
        let parent = vortex_stdx::env::current_working_dir();
        let listed_dirty = format!("{} dirty", shown(&parent));
        assert!(!text.contains(&listed_dirty), "{text:?}");
    }
}
