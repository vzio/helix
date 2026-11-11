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
    sync::Arc,
    time::{Duration, Instant},
};

use vortex_loader::workspace_trust::WorkspaceTrust;
use vortex_term::{application::Application, args::Args, config::Config};
use vortex_view::{doc, document::Mode};

use super::git_log::{on_screen, Repo, Session};
use super::helpers::{temp_file_with_contents, test_config, test_syntax_loader};

/// A label of the menu of the dashboard.
const MENU: &str = "Start typing";
const HISTORY: &str = "Git history";

fn config(dashboard: bool) -> Config {
    let mut config = test_config();
    config.editor.dashboard = dashboard;
    config
}

/// A session on the dashboard, with the first frame drawn.
async fn session() -> Session {
    let mut session = Session::with_config(config(true));
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
    let stats = vortex_term::dashboard::stats::load(
        dir,
        true,
        Arc::new(test_syntax_loader(None)),
        Default::default(),
    );
    assert!(stats.git && stats.shows_git());
    let born = Instant::now();
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
    // settled once the intro is over and every stat is in and revealed
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = born + vortex_term::dashboard::stats::INTRO + Duration::from_secs(1);
    let end = Instant::now() + Duration::from_secs(5);
    while !stats.settled(born, after) {
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

async fn close(mut session: Session) {
    // dropping the dashboard stops its background work
    session.keys("<esc>").await;
    let _ = session.app.close().await;
}
