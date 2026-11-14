//! Debug mode (this fork) against the scripted fake adapter: entering and leaving, one mode at a
//! time, every key of the debug layer (live, in rewind and without a session), the three-row bar
//! and its clicks, the lineline, following and view mode, breakpoints (set in debug mode only,
//! their types), `vx --debug`, and that nothing but errors reaches the status line.
//!
//! The bar is read from the test backend's screen (the transport row starts with `DEBUG `, the
//! lineline is the row below it, the keys row the one below that), and through
//! [`modebar::current`] where the width matters.
//!
//! The fake runs in line mode (`Script::lines()`): its `"step"` stops are the lines the program
//! runs. The sessions are launched with `own_paths` set to the temporary folder holding `main.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use vortex_core::syntax::config::DebugAdapterConfig;
use vortex_term::{
    application::Application,
    debug,
    debug::{bar, launch, mode},
    keymap::Keymaps,
    ui::modebar,
};
use vortex_view::{
    current_ref,
    debug::{
        lineline::MarkKind,
        spectator::{Follow, Mode as Playback, Spectator},
        Section, Status,
    },
    editor::BreakpointKind,
    graphics::Rect,
    theme::DashboardCharset,
};

use super::{
    fake_adapter::{FakeAdapter, Script, Stop},
    harness::Harness,
};
use crate::test::{
    git_log::{git, Repo},
    helpers::AppBuilder,
};

// ---------------------------------------------------------------------------------------------
// The environment

struct Env {
    h: Harness,
    fake: FakeAdapter,
    /// The path the editor knows the file by.
    path: PathBuf,
    dir: PathBuf,
    _tmp: TempDir,
}

async fn env(script: impl FnOnce(&str) -> Script) -> anyhow::Result<Env> {
    env_of(30, script).await
}

/// A source file of `lines` lines.
async fn env_of(lines: usize, script: impl FnOnce(&str) -> Script) -> anyhow::Result<Env> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().canonicalize()?;
    let path = dir.join("main.rs");
    let source: String = (1..=lines).map(|i| format!("    step({i});\n")).collect();
    std::fs::write(&path, source)?;
    let h = Harness::new(AppBuilder::new().with_file(&path, None))?;
    let path = {
        let (_, doc) = current_ref!(h.app.editor);
        doc.path().expect("the file has a path").to_owned()
    };
    let fake = FakeAdapter::start(script(&path.to_string_lossy())).await?;
    Ok(Env {
        h,
        fake,
        path,
        dir,
        _tmp: tmp,
    })
}

fn adapter_config(dir: &Path) -> DebugAdapterConfig {
    DebugAdapterConfig {
        name: "fake".into(),
        transport: "tcp".into(),
        command: String::new(),
        args: Vec::new(),
        port_arg: None,
        templates: Vec::new(),
        quirks: Default::default(),
        // the engine puts its hidden breakpoints under these folders only
        own_paths: Some(vec![dir.to_owned()]),
        std_paths: None,
        lib_paths: None,
        capabilities: None,
    }
}

impl Env {
    fn launch(&mut self) -> anyhow::Result<()> {
        debug::start(
            &mut self.h.app.editor,
            Some(self.fake.addr),
            &adapter_config(&self.dir),
            "fake",
            "launch",
            json!({"program": self.dir.join("prog"), "cwd": self.dir}),
        )
    }

    /// Waits until `stops` stops are recorded, the program is stopped and no step runs.
    async fn settled(&mut self, stops: usize) {
        self.h
            .until(
                &format!("{stops} recorded stops and no step running"),
                |app| {
                    let spectator = &app.editor.debug.spectator;
                    spectator.history.stops.len() >= stops
                        && spectator.step.is_none()
                        && matches!(app.editor.debug.status, Status::Stopped { .. })
                },
            )
            .await;
    }

    /// Launches and waits for the program's first line.
    async fn start(&mut self) -> anyhow::Result<()> {
        self.launch()?;
        self.settled(1).await;
        Ok(())
    }

    /// `<space>G`.
    async fn enter(&mut self) -> anyhow::Result<()> {
        self.h.keys("<space>G").await?;
        assert!(on(&self.h.app), "debug mode is on");
        Ok(())
    }

    /// `l`: one playback step, and waits for it.
    async fn line(&mut self) -> anyhow::Result<()> {
        let next = stops(&self.h.app) + 1;
        self.h.keys("l").await?;
        self.settled(next).await;
        Ok(())
    }

    /// Your breakpoint on `line` (1-based), as the gutter's click makes it.
    fn breakpoint(&mut self, line: usize) {
        debug::breakpoints::toggle(&mut self.h.app.editor, self.path.clone(), line - 1);
    }

    /// Presses `key`, then `Esc`: when the key opened something (a picker, a prompt, the help)
    /// the Esc closed it and debug mode is still on (`true`); else the Esc left debug mode and
    /// this enters it again (`false`).
    async fn tap(&mut self, key: &str) -> anyhow::Result<bool> {
        self.h.keys(key).await?;
        self.h.pump_for(Duration::from_millis(100)).await;
        self.h.keys("<esc>").await?;
        if on(&self.h.app) {
            Ok(true)
        } else {
            self.h.keys("<space>G").await?;
            Ok(false)
        }
    }

    /// The execution requests, in order: (command, thread).
    fn motion(&self) -> Vec<(String, i64)> {
        self.fake
            .requests()
            .into_iter()
            .filter(|(_, c, _)| matches!(c.as_str(), "continue" | "next" | "stepIn" | "stepOut"))
            .map(|(_, c, a)| (c, a["threadId"].as_i64().unwrap_or(-1)))
            .collect()
    }
}

/// `n` line stops of the one thread at lines 2, 3, ...
fn run(p: &str, n: u64) -> Script {
    (0..n).fold(Script::new(p).lines(), |script, i| {
        script.stop(Stop::new(2 + i, "step"))
    })
}

fn queue(lines: &[u64]) -> Vec<Stop> {
    lines.iter().map(|l| Stop::new(*l, "step")).collect()
}

/// Two threads, one after the other (non-stop): the main thread (1) walks lines 2 to 9, thread 2
/// runs through lines 10 to 13 unseen.
fn two_threads(p: &str) -> Script {
    Script::new(p)
        .lines()
        .threads()
        .non_stop()
        .capability("supportsSingleThreadExecutionRequests", json!(true))
        .thread_program(1, queue(&[2, 3, 4, 5, 6, 7, 8, 9]))
        .thread_program(2, queue(&[10, 11, 12, 13]))
}

// ---------------------------------------------------------------------------------------------
// What the tests look at

fn on(app: &Application) -> bool {
    app.editor.debug.mode_on()
}

fn following(app: &Application) -> bool {
    app.editor.debug.following()
}

fn viewing(app: &Application) -> bool {
    app.editor.debug.viewing()
}

fn spectator(app: &Application) -> &Spectator {
    &app.editor.debug.spectator
}

fn stops(app: &Application) -> usize {
    spectator(app).history.stops.len()
}

fn rewinding(app: &Application) -> bool {
    app.editor.debug.is_rewinding()
}

/// The note on debug mode's bar.
fn note(app: &Application) -> String {
    app.editor
        .debug
        .mode
        .as_ref()
        .and_then(|mode| mode.note.clone())
        .unwrap_or_default()
}

fn clear_note(app: &mut Application) {
    if let Some(mode) = app.editor.debug.mode.as_mut() {
        mode.note = None;
    }
}

fn is_stopped(app: &Application, reason: &str) -> bool {
    matches!(&app.editor.debug.status, Status::Stopped { reason: r, .. } if r == reason)
}

fn ended(app: &Application) -> bool {
    matches!(app.editor.debug.status, Status::Ended { .. })
}

/// The line (from 1) of the cursor.
fn cursor_line(app: &Application) -> usize {
    let (view, doc) = current_ref!(app.editor);
    doc.selection(view.id)
        .primary()
        .cursor_line(doc.text().slice(..))
        + 1
}

/// The first line (from 1) the view shows.
fn top_line(app: &Application) -> usize {
    let (view, doc) = current_ref!(app.editor);
    doc.text()
        .char_to_line(doc.view_offset(view.id).anchor.min(doc.text().len_chars()))
        + 1
}

fn text(app: &Application) -> String {
    let (_, doc) = current_ref!(app.editor);
    doc.text().to_string()
}

/// Your breakpoints in `main.rs`: (line from 1, type).
fn breakpoints(e: &Env) -> Vec<(usize, BreakpointKind)> {
    e.h.app
        .editor
        .breakpoints
        .get(&e.path)
        .map(|list| list.iter().map(|b| (b.line + 1, b.kind)).collect())
        .unwrap_or_default()
}

fn thread_name(app: &Application, thread: &str) -> bool {
    spectator(app)
        .spectated
        .is_some_and(|key| key.thread.to_string() == thread)
}

fn marks(app: &Application) -> Vec<MarkKind> {
    app.editor
        .debug
        .engine
        .record
        .marks()
        .map(|mark| mark.kind.clone())
        .collect()
}

fn breakpoint_marks(app: &Application) -> usize {
    marks(app)
        .iter()
        .filter(|kind| matches!(kind, MarkKind::Breakpoint { .. }))
        .count()
}

/// The screen's rows, for conditions of `until`.
fn screen_rows(app: &Application) -> Vec<String> {
    let screen = app.screen();
    (0..screen.area.height)
        .map(|y| {
            (0..screen.area.width)
                .map(|x| screen.get(x, y).map_or(" ", |cell| cell.symbol.as_str()))
                .collect()
        })
        .collect()
}

fn dump(rows: &[String]) -> String {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| !row.trim().is_empty())
        .map(|(i, row)| format!("{i:3}|{}", row.trim_end()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The row of the screen the bar's transport row is on.
fn bar_top(rows: &[String]) -> usize {
    rows.iter()
        .rposition(|row| row.trim_start().starts_with("DEBUG "))
        .unwrap_or_else(|| panic!("no debug bar on the screen:\n{}", dump(rows)))
}

/// The bar's three rows, trailing spaces cut.
fn bar_rows(h: &Harness) -> [String; 3] {
    let rows = h.rows();
    let top = bar_top(&rows);
    [
        rows[top].trim_end().to_string(),
        rows[top + 1].trim_end().to_string(),
        rows[top + 2].trim_end().to_string(),
    ]
}

/// The lineline's cells: the bar's second row without its padding.
fn lineline(h: &Harness) -> Vec<char> {
    let rows = h.rows();
    let row = &rows[bar_top(&rows) + 1];
    let cells: Vec<char> = row.chars().collect();
    cells[1..cells.len() - 1].to_vec()
}

fn on_bar(h: &Harness, needle: &str) -> bool {
    bar_rows(h).iter().any(|row| row.contains(needle))
}

/// Clicks the first cell of `needle` in the bar's row `row` (0: transport, 2: keys), `skip`
/// cells in.
async fn click_on_bar(h: &mut Harness, row: usize, needle: &str, skip: u16) -> anyhow::Result<()> {
    let rows = h.rows();
    let y = bar_top(&rows) + row;
    let byte = rows[y]
        .find(needle)
        .unwrap_or_else(|| panic!("no {needle:?} on the bar row {row}:\n{}", dump(&rows)));
    let x = rows[y][..byte].chars().count() as u16 + skip;
    h.click(x, y as u16).await
}

/// The symbols of the lineline that show a hit's density.
const DENSITY: &[char] = &['·', '░', '▒', '▓', '█'];

fn keymaps() -> Keymaps {
    Keymaps::new(Box::new(arc_swap::access::Constant(
        vortex_term::keymap::default(),
    )))
}

// ---------------------------------------------------------------------------------------------
// 1. Entering and leaving

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn space_g_enters_and_esc_leaves_a_paused_session_as_it_was() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 12)).await?;
    e.start().await?;
    assert!(!on(&e.h.app));
    assert!(e.h.app.editor.debug.visible);

    e.h.keys("<space>G").await?;
    assert!(on(&e.h.app));
    assert!(following(&e.h.app), "a session: debug mode follows it");
    assert!(!viewing(&e.h.app));

    e.h.keys("<esc>").await?;
    assert!(!on(&e.h.app));
    assert_eq!(
        spectator(&e.h.app).mode,
        Playback::Paused,
        "paused stays paused"
    );
    assert!(matches!(
        e.h.app.editor.debug.status,
        Status::Stopped { .. }
    ));
    assert!(e.h.app.editor.debug.visible, "the panel stays");
    assert_eq!(stops(&e.h.app), 1);
    e.h.pump_for(Duration::from_millis(300)).await;
    assert_eq!(stops(&e.h.app), 1, "nothing played");
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn esc_leaves_a_playing_session_playing_with_the_panel_as_it_was() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 12)).await?;
    e.start().await?;
    e.h.keys("<space>G<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Playback::Playing);
    e.h.keys("<esc>").await?;
    assert!(!on(&e.h.app));
    assert_eq!(
        spectator(&e.h.app).mode,
        Playback::Playing,
        "playing stays playing"
    );
    assert!(e.h.app.editor.debug.visible);
    e.h.until("playback went on after Esc", |app| stops(app) >= 4)
        .await;
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_debug_mode_command_toggles_and_the_palette_lists_it() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 4)).await?;
    e.h.keys(":debug-mode<ret>").await?;
    assert!(on(&e.h.app));
    e.h.keys(":debug-mode<ret>").await?;
    assert!(!on(&e.h.app), "the command toggles");

    // a static command, so the command palette lists it ...
    assert!(vortex_term::commands::MappableCommand::STATIC_COMMAND_LIST
        .iter()
        .any(|command| command.name() == "debug_mode"));
    e.h.keys("<space>?debug_mode").await?;
    e.h.until("debug_mode in the palette", |app| {
        screen_rows(app)
            .iter()
            .any(|row| row.contains("debug_mode"))
    })
    .await;
    // ... and runs it
    e.h.keys("<ret>").await?;
    e.h.until("debug mode on", on).await;
    assert!(e
        .h
        .app
        .editor
        .recent_commands
        .iter()
        .any(|name| name == "debug_mode"));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_session_debug_mode_views_and_a_session_starting_follows() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 6)).await?;
    e.h.keys("<space>G").await?;
    assert!(viewing(&e.h.app), "nothing to follow");
    assert!(!following(&e.h.app));
    assert!(on_bar(&e.h, "DEBUG ○ no session"), "{}", dump(&e.h.rows()));

    e.launch()?;
    assert!(
        following(&e.h.app),
        "the session starting switches to following"
    );
    e.settled(1).await;
    assert!(on(&e.h.app));
    assert!(following(&e.h.app));
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 2. One mode at a time

fn make_repo(parent: &Path) -> PathBuf {
    let work = parent.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, "", &["init", "-q"]);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    git(&work, "", &["add", "-A"]);
    git(&work, "", &["commit", "-q", "-m", "Start"]);
    work
}

fn enter_repo(test: &str) -> Option<PathBuf> {
    Repo::enter_with(module_path!(), test, make_repo)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn space_g_ends_watch_mode() -> anyhow::Result<()> {
    let Some(_) = enter_repo("space_g_ends_watch_mode") else {
        return Ok(());
    };
    let mut h = Harness::new(AppBuilder::new())?;
    h.keys(":watch<ret>").await?;
    assert!(h.app.editor.watch.is_active());
    h.pump_for(Duration::from_millis(300)).await;

    h.keys("<space>G").await?;
    assert!(
        !h.app.editor.watch.is_active(),
        "entering debug mode ends watch mode (status: {:?})",
        h.app.editor.get_status()
    );
    assert!(on(&h.app), "debug mode is on");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_debug_mode_command_ends_watch_mode_and_watch_leaves_debug_mode() -> anyhow::Result<()>
{
    let Some(_) = enter_repo("the_debug_mode_command_ends_watch_mode_and_watch_leaves_debug_mode")
    else {
        return Ok(());
    };
    let mut h = Harness::new(AppBuilder::new())?;
    h.keys(":watch<ret>").await?;
    assert!(h.app.editor.watch.is_active());
    h.pump_for(Duration::from_millis(300)).await;

    h.keys(":debug-mode<ret>").await?;
    assert!(on(&h.app));
    assert!(!h.app.editor.watch.is_active(), "one mode at a time");
    // only the debug bar is on
    let rows = h.rows();
    assert!(rows.iter().any(|r| r.trim_start().starts_with("DEBUG ")));
    assert!(
        !rows.iter().any(|r| r.contains("WATCH ")),
        "{}",
        dump(&rows)
    );

    // starting watch mode leaves debug mode
    h.keys(":watch<ret>").await?;
    assert!(h.app.editor.watch.is_active());
    assert!(!on(&h.app), "watch mode leaves debug mode");
    let rows = h.rows();
    assert!(!rows.iter().any(|r| r.trim_start().starts_with("DEBUG ")));
    h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 3. The keys of the debug layer

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_session_the_playback_keys_wait_and_say_so() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 4)).await?;
    e.enter().await?;
    assert!(viewing(&e.h.app));

    // what each key says on the bar
    let cases = [
        ("l", "no session: start one first"),
        ("h", "no session: start one first"),
        ("<space>", "no session: start one first"),
        ("L", "no session: start one first"),
        ("H", "no breakpoint was hit before this"),
        ("v", "no session to follow: start one first"),
        ("X", "Time labels cleared"),
        ("k", "Playback speed: "),
        ("j", "Playback speed: "),
        ("e", "Playback follows: "),
    ];
    for (key, expected) in cases {
        clear_note(&mut e.h.app);
        e.h.keys(key).await?;
        assert!(
            note(&e.h.app).starts_with(expected),
            "{key:?}: the note is {:?}, not {expected:?}",
            note(&e.h.app)
        );
        assert!(on(&e.h.app), "{key:?} left debug mode");
    }
    // p goes live from rewind and has nothing to do otherwise: silent
    clear_note(&mut e.h.app);
    e.h.keys("p").await?;
    assert_eq!(note(&e.h.app), "");
    assert!(viewing(&e.h.app), "still looking around");
    assert_eq!(stops(&e.h.app), 0);
    assert_eq!(spectator(&e.h.app).mode, Playback::Paused);

    // the keys that need a session do nothing: no step, no request
    for key in [
        ".", "i", "o", "f", "r", "<gt>", "<lt>", "z", "a", "S", "R", "E",
    ] {
        e.tap(key).await?;
        assert!(on(&e.h.app), "{key:?}");
    }
    assert_eq!(
        e.fake.requests().len(),
        0,
        "nothing was sent: there is no session"
    );
    assert!(!e.h.app.editor.debug.is_focused());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn b_and_capital_b_set_and_cycle_the_breakpoint_type_at_the_cursor() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 4)).await?;
    e.enter().await?;
    // the cursor is on line 1 (view mode: your own); `<down>` moves it
    e.h.keys("<down><down>").await?;
    assert_eq!(cursor_line(&e.h.app), 3);
    e.h.keys("b").await?;
    assert_eq!(breakpoints(&e), [(3, BreakpointKind::Any)]);
    for (expected, label) in [
        (BreakpointKind::Main, "main thread only"),
        (BreakpointKind::Sub, "sub-threads only"),
        (BreakpointKind::Any, "any thread"),
    ] {
        e.h.keys("B").await?;
        assert_eq!(breakpoints(&e), [(3, expected)]);
        assert_eq!(note(&e.h.app), format!("breakpoint: {label}"));
    }
    // C and M edit the condition and the log message of the breakpoint: a prompt each
    assert!(e.tap("C").await?, "C opens the condition prompt");
    assert!(e.tap("M").await?, "M opens the log message prompt");
    // b again takes it away
    e.h.keys("b").await?;
    assert!(breakpoints(&e).is_empty());
    // no breakpoint, nothing to edit: an error
    e.h.keys("C").await?;
    assert!(
        e.h.errors.iter().any(|m| m.contains("No breakpoint")),
        "{:?}",
        e.h.errors
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_help_the_panel_keys_and_the_mode_keys_that_open_things() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 6)).await?;
    e.enter().await?;

    // ? pushes the key help, built from the layer; any key closes it
    e.h.keys("?").await?;
    e.h.pump_for(Duration::from_millis(100)).await;
    let rows = e.h.rows();
    assert!(
        rows.iter().any(|r| r.contains("Debug mode")),
        "{}",
        dump(&rows)
    );
    assert!(
        rows.iter().any(|r| r.contains("playback")),
        "{}",
        dump(&rows)
    );
    e.h.keys("<esc>").await?;
    assert!(on(&e.h.app), "the key closes the help and is not run");
    assert!(!e.h.rows().iter().any(|r| r.contains("any key closes")));

    // s opens the template picker
    assert!(e.tap("s").await?, "s opens the template picker");

    // P shows and hides the panel
    let visible = e.h.app.editor.debug.visible;
    e.h.keys("P").await?;
    assert_ne!(e.h.app.editor.debug.visible, visible);
    e.h.keys("P").await?;
    assert_eq!(e.h.app.editor.debug.visible, visible);

    // Tab focuses the panel, and Tab goes back to the editor while debug mode is on
    e.h.keys("<tab>").await?;
    assert!(e.h.app.editor.debug.is_focused());
    assert!(on(&e.h.app));
    e.h.keys("<tab>").await?;
    assert!(!e.h.app.editor.debug.is_focused());
    assert!(on(&e.h.app));
    // C-g does the same
    e.h.keys("<C-g>").await?;
    assert!(e.h.app.editor.debug.is_focused());
    e.h.keys("<tab>").await?;
    assert!(!e.h.app.editor.debug.is_focused());

    // V focuses the variables, c the capture settings
    e.h.keys("V").await?;
    assert!(e.h.app.editor.debug.is_focused());
    assert_eq!(e.h.app.editor.debug.section, Section::Variables);
    e.h.keys("<tab>").await?;
    assert!(!e.h.app.editor.debug.is_focused());
    e.h.keys("c").await?;
    assert_eq!(spectator(&e.h.app).settings, Some(0));
    e.h.app.editor.debug.spectator.settings = None;
    e.h.app.editor.debug.unfocus();
    assert!(on(&e.h.app));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_live_keys_play_step_change_speed_and_what_is_followed() -> anyhow::Result<()> {
    // lines 2 to 14; the stop at line 5 is inside a call (two frames), so that `o` has a way out
    let mut e = env(|p| {
        (0..13)
            .fold(Script::new(p).lines(), |script, i| {
                let line = 2 + i;
                let stop = Stop::new(line, "step");
                script.stop(if line == 5 {
                    stop.frame("helper", p, 5).frame("main", p, 4)
                } else {
                    stop
                })
            })
            .capability("supportsSingleThreadExecutionRequests", json!(true))
            .exception_filter("raised", "Raised")
    })
    .await?;
    e.start().await?;
    e.enter().await?;
    assert_eq!(stops(&e.h.app), 1);
    assert_eq!(cursor_line(&e.h.app), 2);

    // l: one playback step, the cursor goes with it
    e.line().await?;
    assert_eq!(cursor_line(&e.h.app), 3);
    assert_eq!(
        e.motion().iter().map(|m| m.0.as_str()).collect::<Vec<_>>(),
        ["continue"]
    );
    // . i o: step over, in and out: each ends at one line (a step is the engine's continues to
    // the line where it ends: the fake sees none of the step requests)
    for (key, line) in [(".", 4), ("i", 5), ("o", 6)] {
        let next = stops(&e.h.app) + 1;
        e.h.keys(key).await?;
        e.settled(next).await;
        assert_eq!(cursor_line(&e.h.app), line, "{key}");
    }
    assert_eq!(stops(&e.h.app), 5);

    // k and j: faster and slower
    e.h.app.editor.debug.spectator.speed = 5;
    let speed = spectator(&e.h.app).speed;
    e.h.keys("k").await?;
    assert_eq!(spectator(&e.h.app).speed, speed + 1);
    assert!(note(&e.h.app).starts_with("Playback speed: "));
    e.h.keys("jj").await?;
    assert_eq!(spectator(&e.h.app).speed, speed - 1);

    // e: own -> std -> lib -> own
    let first = spectator(&e.h.app).follow;
    let mut seen = vec![first];
    for _ in 0..3 {
        e.h.keys("e").await?;
        seen.push(spectator(&e.h.app).follow);
    }
    assert_eq!(seen[0], seen[3], "e cycles: {seen:?}");
    assert!(seen[1] != seen[0] && seen[2] != seen[1] && seen[2] != seen[0]);
    assert!(
        seen.contains(&Follow::Own) && seen.contains(&Follow::Std) && seen.contains(&Follow::Lib)
    );
    assert!(note(&e.h.app).starts_with("Playback follows: "));

    // Space plays, Space pauses
    e.settled(stops(&e.h.app)).await;
    e.h.app.editor.debug.spectator.speed = 0;
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Playback::Playing);
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Playback::Paused);
    e.h.until("no step in flight", |app| spectator(app).step.is_none())
        .await;

    // z freezes the spectated thread and thaws it, a solos it
    e.h.keys("z").await?;
    assert_eq!(spectator(&e.h.app).frozen.len(), 1);
    e.h.keys("z").await?;
    assert!(spectator(&e.h.app).frozen.is_empty());
    e.h.keys("a").await?;
    assert!(spectator(&e.h.app).solo);
    e.h.keys("a").await?;
    assert!(!spectator(&e.h.app).solo);

    // E toggles stopping on exceptions
    e.h.keys("E").await?;
    assert_eq!(
        e.h.app.editor.debug.exception_filters,
        Some(vec!["raised".to_string()])
    );
    assert_eq!(note(&e.h.app), "Stopping on exceptions");
    e.h.keys("E").await?;
    assert_eq!(e.h.app.editor.debug.exception_filters, Some(Vec::new()));
    assert_eq!(note(&e.h.app), "Not stopping on exceptions");
    e.h.until("both requests went out", |_| {
        e.fake.requests_named("setExceptionBreakpoints").len() >= 3
    })
    .await;

    // the pickers and prompts: F (stack frames), x (evaluate), w (add a watch); t (threads) is
    // with the other threads' keys
    for key in ["F", "x", "w"] {
        assert!(e.tap(key).await?, "{key} opens something");
    }
    // s: a session is running; R: this adapter can't restart
    e.h.keys("s").await?;
    assert!(
        e.h.errors.iter().any(|m| m.contains("already running")),
        "{:?}",
        e.h.errors
    );
    e.h.keys("R").await?;
    assert!(
        e.h.errors.iter().any(|m| m.contains("does not support")),
        "{:?}",
        e.h.errors
    );
    // m: the memory of the value under the panel's cursor: nothing to show here, debug mode stays
    e.h.keys("m").await?;
    assert!(on(&e.h.app));
    // X: the time labels
    e.h.keys("X").await?;
    assert_eq!(note(&e.h.app), "Time labels cleared");
    e.h.finish().await
}

/// A session with breakpoints on lines 5 and 9, sent to line 9 (stops: lines 2, 5, 9).
async fn at_the_second_breakpoint() -> anyhow::Result<Env> {
    let mut e = env(|p| run(p, 14)).await?;
    e.breakpoint(5);
    e.breakpoint(9);
    e.start().await?;
    e.enter().await?;
    // L: at full speed to your next breakpoint (twice)
    e.h.keys("L").await?;
    e.settled(2).await;
    assert!(is_stopped(&e.h.app, "breakpoint"));
    assert_eq!(cursor_line(&e.h.app), 5);
    e.h.keys("L").await?;
    e.settled(3).await;
    assert_eq!(cursor_line(&e.h.app), 9);
    assert!(!rewinding(&e.h.app));
    assert_eq!(breakpoint_marks(&e.h.app), 2);
    Ok(e)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_rewind_h_and_l_move_one_stop_and_p_goes_live() -> anyhow::Result<()> {
    let mut e = at_the_second_breakpoint().await?;
    // h goes back one recorded stop, into rewind from live
    e.h.keys("h").await?;
    assert!(rewinding(&e.h.app));
    assert_eq!(spectator(&e.h.app).history.at, Some(1));
    assert_eq!(
        cursor_line(&e.h.app),
        5,
        "the camera follows the stop shown"
    );
    e.h.keys("h").await?;
    assert_eq!(spectator(&e.h.app).history.at, Some(0));
    assert_eq!(cursor_line(&e.h.app), 2);
    e.h.keys("h").await?;
    assert_eq!(
        spectator(&e.h.app).history.at,
        Some(0),
        "nothing before the first stop"
    );
    // l goes forward one stop; past the one before the last it is live again
    e.h.keys("l").await?;
    assert_eq!(spectator(&e.h.app).history.at, Some(1));
    e.h.keys("l").await?;
    assert!(!rewinding(&e.h.app), "the last stop is the live one");
    assert_eq!(cursor_line(&e.h.app), 9);
    // nothing was executed by any of it
    assert_eq!(e.motion().len(), 2, "only the two L");

    // p goes live from rewind, and says so when it is
    e.h.keys("hhp").await?;
    assert!(!rewinding(&e.h.app));
    assert_eq!(cursor_line(&e.h.app), 9);
    assert_eq!(note(&e.h.app), "Live");
    // (p does nothing when it is live: the note stays)
    e.h.keys("p").await?;
    assert!(!rewinding(&e.h.app));
    assert_eq!(note(&e.h.app), "Live");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_rewind_capital_h_and_l_go_to_the_breakpoint_marks() -> anyhow::Result<()> {
    let mut e = at_the_second_breakpoint().await?;
    e.h.keys("hh").await?;
    assert_eq!(cursor_line(&e.h.app), 2);

    // L in rewind: the next breakpoint mark recorded, else the present
    e.h.keys("L").await?;
    assert!(rewinding(&e.h.app));
    assert_eq!(cursor_line(&e.h.app), 5, "the first breakpoint");
    // H in rewind: the previous one; there is none before the first
    e.h.keys("H").await?;
    assert_eq!(note(&e.h.app), "no breakpoint was hit before this");
    assert_eq!(cursor_line(&e.h.app), 5);
    e.h.keys("L").await?;
    assert!(!rewinding(&e.h.app), "the next breakpoint is the live stop");
    assert_eq!(cursor_line(&e.h.app), 9);

    // from live, at the second breakpoint: H goes back to the first one
    e.h.keys("H").await?;
    assert!(
        rewinding(&e.h.app),
        "H from live goes back to a breakpoint mark"
    );
    assert_eq!(cursor_line(&e.h.app), 5);
    assert_eq!(e.motion().len(), 2, "rewind executed nothing");
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 4. The three-row bar

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bar_has_three_rows_in_debug_mode_only() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 6)).await?;
    let keys = keymaps();
    assert!(modebar::current(&e.h.app.editor, &keys, 120).is_none());
    assert_eq!(modebar::height(&e.h.app.editor), 0);

    e.enter().await?;
    let bar = modebar::current(&e.h.app.editor, &keys, 120).expect("a bar in debug mode");
    assert_eq!(bar.rows.len(), 3);
    assert_eq!(bar.height(), 3);
    assert_eq!(modebar::height(&e.h.app.editor), 3);
    assert_eq!(bar::BAR_ROWS, 3);
    // the middle row is a line as wide as the bar inside its padding
    let line = bar.rows[1].line.as_ref().expect("the lineline");
    assert_eq!(line.cells.len(), 118);
    // and on the screen: three rows
    let rows = bar_rows(&e.h);
    assert!(rows[0].trim_start().starts_with("DEBUG "), "{rows:#?}");
    assert!(!rows[2].trim().is_empty(), "{rows:#?}");

    e.h.keys("<esc>").await?;
    assert!(modebar::current(&e.h.app.editor, &keys, 120).is_none());
    assert!(!e
        .h
        .rows()
        .iter()
        .any(|r| r.trim_start().starts_with("DEBUG ")));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_state_on_the_bar_follows_the_session() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 8)).await?;
    e.enter().await?;
    assert!(on_bar(&e.h, "DEBUG ○ no session"), "{}", dump(&e.h.rows()));

    e.launch()?;
    e.settled(1).await;
    e.h.keys("<esc><space>G").await?;
    assert!(on_bar(&e.h, "DEBUG ❚❚ paused"), "{}", dump(&e.h.rows()));

    e.h.keys("<space>").await?;
    assert!(on_bar(&e.h, "DEBUG ▶ playing"), "{}", dump(&e.h.rows()));
    e.h.keys("<space>").await?;
    e.h.until("paused", |app| spectator(app).step.is_none())
        .await;
    assert!(on_bar(&e.h, "DEBUG ❚❚ paused"));

    e.h.keys("h").await?;
    assert!(on_bar(&e.h, "DEBUG ◀◀ rewind"), "{}", dump(&e.h.rows()));
    e.h.keys("p").await?;
    assert!(on_bar(&e.h, "DEBUG ❚❚ paused"));

    // S ends the program: exited
    e.h.keys("S").await?;
    e.h.until("the end", ended).await;
    e.h.pump_for(Duration::from_millis(100)).await;
    let rows = bar_rows(&e.h);
    assert!(
        rows[0].contains("DEBUG ○ exited 0") || rows[0].contains("DEBUG ○ ended"),
        "{rows:#?}"
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_transport_badges_show_their_keys_and_the_keys_row_follows_the_situation(
) -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 8)).await?;
    e.enter().await?;
    // no session: start; the transport is there all the same
    let rows = bar_rows(&e.h);
    for badge in [" H  |◀", " h  ◀◀", " Space  ▶", " l  ▶▶", " L  ▶|"] {
        assert!(rows[0].contains(badge), "{badge:?} in {:?}", rows[0]);
    }
    assert!(!rows[0].contains("present"));
    assert!(rows[0].contains("lines/s · own"), "{rows:#?}");
    for badge in [
        " s  start",
        " v  follow",
        " b  breakpoint",
        " B  type",
        " Tab  panel",
        " ?  keys",
        " Esc  leave",
    ] {
        assert!(rows[2].contains(badge), "{badge:?} in {:?}", rows[2]);
    }
    assert!(!rows[2].contains("faster"));

    // a session: following, so v looks; the session's keys (at 120 columns they are down to
    // their keys next to the note: the labels show on a wide bar)
    e.launch()?;
    e.settled(1).await;
    e.h.pump_for(Duration::from_millis(100)).await;
    let rows = bar_rows(&e.h);
    assert!(!rows[2].contains(" s "), "{rows:#?}");
    assert!(rows[2].contains(" v "), "{rows:#?}");
    assert!(!rows[2].contains(" B "), "{rows:#?}");
    let (_, wide, _) = narrow(&e.h.app, 300);
    for badge in [
        " v  view",
        " b  breakpoint",
        " e  own",
        " f  change",
        " r  cursor",
        " >  thread",
        " S  stop",
    ] {
        assert!(wide[2].contains(badge), "{badge:?} in {:?}", wide[2]);
    }
    // the speed sits between its keys on the transport bar
    assert!(
        wide[0].contains(" j  −") && wide[0].contains(" k  +"),
        "{:?}",
        wide[0]
    );
    assert!(!wide[2].contains(" s  start") && !wide[2].contains(" B  type"));

    e.h.keys("<space>").await?;
    assert!(on_bar(&e.h, " Space  ❚❚"), "{}", dump(&e.h.rows()));
    assert!(!on_bar(&e.h, " Space  ▶"));
    e.h.keys("<space>").await?;
    e.h.until("paused", |app| spectator(app).step.is_none())
        .await;

    // rewind: p present
    e.h.keys("h").await?;
    assert!(on_bar(&e.h, " p  present"), "{}", dump(&e.h.rows()));
    e.h.keys("p").await?;
    assert!(!on_bar(&e.h, "present"));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_click_on_a_badge_runs_its_command() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 10)).await?;
    e.start().await?;
    e.enter().await?;
    e.line().await?;
    e.line().await?;
    assert_eq!(stops(&e.h.app), 3);

    // ◀◀ (h): into rewind; the key of a badge works as well as its label
    click_on_bar(&mut e.h, 0, " h  ◀◀", 5).await?;
    assert!(rewinding(&e.h.app));
    assert_eq!(spectator(&e.h.app).history.at, Some(1));
    click_on_bar(&mut e.h, 0, " h  ◀◀", 1).await?;
    assert_eq!(spectator(&e.h.app).history.at, Some(0));
    // ▶▶ (l): forward, then p present
    click_on_bar(&mut e.h, 0, " l  ▶▶", 4).await?;
    assert_eq!(spectator(&e.h.app).history.at, Some(1));
    click_on_bar(&mut e.h, 0, " p  present", 6).await?;
    assert!(!rewinding(&e.h.app));
    assert_eq!(cursor_line(&e.h.app), 4);

    // ▶ (Space): plays, and the badge turns to ❚❚
    click_on_bar(&mut e.h, 0, " Space  ▶", 7).await?;
    assert_eq!(spectator(&e.h.app).mode, Playback::Playing);
    e.h.pump_for(Duration::from_millis(50)).await;
    click_on_bar(&mut e.h, 0, " Space  ❚❚", 7).await?;
    assert_eq!(spectator(&e.h.app).mode, Playback::Paused);
    e.h.until("no step in flight", |app| spectator(app).step.is_none())
        .await;

    // a badge of the keys row: v looks around, and then follows
    click_on_bar(&mut e.h, 2, " v ", 1).await?;
    assert!(viewing(&e.h.app));
    click_on_bar(&mut e.h, 2, " v ", 1).await?;
    assert!(following(&e.h.app));
    // Esc leave
    click_on_bar(&mut e.h, 2, " Esc ", 2).await?;
    assert!(!on(&e.h.app));
    e.h.finish().await
}

/// The bar of debug mode drawn on `width` columns.
fn narrow(app: &Application, width: u16) -> (modebar::Bar, Vec<String>, modebar::Drawn) {
    let bar = modebar::current(&app.editor, &keymaps(), width).expect("a bar");
    let area = Rect::new(0, 0, width, 3);
    let mut surface = tui::buffer::Buffer::empty(area);
    let drawn = modebar::render(&bar, &app.editor.theme, area, &mut surface);
    let rows = (0..3)
        .map(|y| {
            (0..width)
                .map(|x| {
                    surface
                        .get(x, y)
                        .map_or(" ", |cell| cell.symbol.as_str())
                        .to_string()
                })
                .collect::<String>()
        })
        .collect();
    (bar, rows, drawn)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_a_narrow_bar_the_keys_collapse_first_and_nothing_is_wider_than_the_bar(
) -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 6)).await?;
    e.start().await?;
    e.enter().await?;
    e.line().await?;

    let labels = |rows: &[String]| {
        [
            "change", "cursor", "thread", "stop", "panel", "keys", "leave",
        ]
        .iter()
        .filter(|label| rows[2].contains(*label))
        .count()
    };
    let (_, wide, _) = narrow(&e.h.app, 300);
    assert!(wide[0].contains("DEBUG ❚❚ paused"));
    assert!(wide[2].contains(" f  change"), "{wide:#?}");
    assert_eq!(
        labels(&wide),
        7,
        "every label fits on a wide bar: {wide:#?}"
    );

    let mut previous = labels(&wide);
    for width in [200u16, 160, 120, 100, 80, 60, 50, 40, 30, 20, 12] {
        let (bar, rows, drawn) = narrow(&e.h.app, width);
        // the lineline is laid out for the bar's width inside its padding
        assert_eq!(
            bar.rows[1].line.as_ref().unwrap().cells.len(),
            width as usize - 2,
            "width {width}"
        );
        // every badge and spot is inside the bar
        for (rect, _) in &drawn.badges {
            assert!(rect.right() <= width, "width {width}: {rect:?}");
        }
        for ((x, y), _) in &drawn.spots {
            assert!(*x < width && *y < 3);
        }
        for row in &rows {
            assert_eq!(row.chars().count(), width as usize);
        }
        // the labels give way before the state does
        let shown = labels(&rows);
        assert!(shown <= previous, "width {width}: {rows:#?}");
        // a 120-column terminal shows every key with its label (the note gives way first)
        if width >= 120 {
            assert_eq!(shown, 7, "width {width}: {rows:#?}");
        }
        previous = shown;
        if width >= 50 {
            assert!(
                rows[0].contains("DEBUG ❚❚ paused"),
                "the state is shortened last (width {width}): {rows:#?}"
            );
        }
    }
    assert_eq!(previous, 0, "no label is left at 12 columns");
    // at 60 columns the keys are down to their keys: no labels but some keys still show
    let (_, rows, _) = narrow(&e.h.app, 60);
    assert!(!rows[2].contains("change"), "{rows:#?}");
    assert!(rows[2].contains(" f "), "{rows:#?}");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_remapped_in_keys_debug_shows_remapped_on_the_bar_and_works() -> anyhow::Result<()> {
    let toml = "[keys.debug]\nx = \"debug_line_back\"\nh = \"no_op\"\nz = \"debug_leave\"\n";
    let loaded = vortex_term::config::Config::load(
        Ok(&toml.to_string()),
        Err(vortex_term::config::ConfigLoadError::default()),
    )
    .unwrap();
    let mut config = crate::test::helpers::test_config();
    config.layers = loaded.layers;

    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().canonicalize()?;
    let path = dir.join("main.rs");
    std::fs::write(
        &path,
        (1..=30)
            .map(|i| format!("    step({i});\n"))
            .collect::<String>(),
    )?;
    let h = Harness::new(AppBuilder::new().with_config(config).with_file(&path, None))?;
    let p = {
        let (_, doc) = current_ref!(h.app.editor);
        doc.path().unwrap().to_owned()
    };
    let fake = FakeAdapter::start(run(&p.to_string_lossy(), 8)).await?;
    let mut e = Env {
        h,
        fake,
        path: p,
        dir,
        _tmp: tmp,
    };
    e.start().await?;
    e.enter().await?;
    e.line().await?;

    let rows = bar_rows(&e.h);
    assert!(rows[0].contains(" x  ◀◀"), "{rows:#?}");
    assert!(!rows[0].contains(" h  ◀◀"), "{rows:#?}");
    // the keys that weren't remapped are still there
    assert!(rows[0].contains(" l  ▶▶"), "{rows:#?}");

    // h is nothing now; x goes back
    e.h.keys("h").await?;
    assert!(!rewinding(&e.h.app));
    e.h.keys("x").await?;
    assert!(rewinding(&e.h.app));
    // and z leaves
    e.h.keys("z").await?;
    assert!(!on(&e.h.app));
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 5. The lineline in a real session

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lineline_has_density_cells_and_the_live_playhead_in_the_last_column(
) -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 12)).await?;
    e.start().await?;
    e.enter().await?;
    for _ in 0..6 {
        e.line().await?;
    }
    e.h.pump_for(Duration::from_millis(50)).await;
    let cells = lineline(&e.h);
    assert_eq!(cells.len(), 118);
    assert_eq!(
        cells[117],
        '┃',
        "the live playhead: {}",
        cells.iter().collect::<String>()
    );
    assert!(
        cells[..117].iter().any(|c| DENSITY.contains(c)),
        "density cells: {}",
        cells.iter().collect::<String>()
    );
    assert!(!cells.contains(&'▕'));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_rewind_the_playhead_is_inside_the_lineline_and_the_last_column_is_the_end(
) -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 12)).await?;
    e.start().await?;
    e.enter().await?;
    for _ in 0..6 {
        e.line().await?;
    }
    e.h.keys("hhh").await?;
    let cells = lineline(&e.h);
    let at = cells
        .iter()
        .position(|c| *c == '┃')
        .unwrap_or_else(|| panic!("a playhead: {}", cells.iter().collect::<String>()));
    assert!(at < 117, "the playhead is inside, not at the end");
    assert_eq!(cells[117], '▕', "the end of the record");
    assert_eq!(cells.iter().filter(|c| **c == '┃').count(), 1);

    e.h.keys("p").await?;
    let cells = lineline(&e.h);
    assert_eq!(cells[117], '┃');
    assert_eq!(cells.iter().filter(|c| **c == '┃').count(), 1);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_breakpoint_hit_shows_a_dot_and_a_full_speed_run_a_gap() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 14)).await?;
    e.breakpoint(6);
    e.start().await?;
    e.enter().await?;
    e.line().await?;
    let before = lineline(&e.h);
    assert!(!before.contains(&'●'));
    assert!(!before.contains(&'┄'));

    // L: at full speed to the breakpoint
    e.h.keys("L").await?;
    e.h.until("the breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.settled(3).await;
    e.h.pump_for(Duration::from_millis(50)).await;
    let cells = lineline(&e.h);
    let row: String = cells.iter().collect();
    assert!(cells.contains(&'●'), "a breakpoint hit shows ●: {row}");
    assert!(
        cells.contains(&'┄'),
        "a run at full speed is a gap ┄: {row}"
    );
    assert_eq!(breakpoint_marks(&e.h.app), 1);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_click_on_the_lineline_seeks_and_a_click_on_the_last_column_goes_live(
) -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 12)).await?;
    e.start().await?;
    e.enter().await?;
    for _ in 0..7 {
        e.line().await?;
    }
    assert_eq!(stops(&e.h.app), 8);
    e.h.pump_for(Duration::from_millis(50)).await;

    // the column that holds the hit with counter 3 (the 4th line shown: line 5)
    let debug = &e.h.app.editor.debug;
    let slots = bar::layout(
        &debug.engine.record,
        debug.spectator.spectated,
        None,
        false,
        118,
    );
    let column = slots
        .iter()
        .position(|slot| matches!(slot, bar::Slot::Span { from, to, .. } if *from <= 3 && 3 < *to))
        .expect("a column for counter 3");
    let rows = e.h.rows();
    let y = (bar_top(&rows) + 1) as u16;
    // (the lineline starts one column in: the bar's padding)
    e.h.click(1 + column as u16, y).await?;
    assert!(rewinding(&e.h.app), "a click seeks: {}", dump(&e.h.rows()));
    let shown = spectator(&e.h.app)
        .history
        .current()
        .unwrap()
        .counter
        .unwrap();
    assert!(
        shown.abs_diff(3) <= 1,
        "the stop nearest the click: counter {shown}"
    );
    assert_eq!(cursor_line(&e.h.app), 2 + shown as usize);

    // the last column: the present
    e.h.click(1 + 117, y).await?;
    assert!(!rewinding(&e.h.app), "a click on the last column goes live");
    assert_eq!(cursor_line(&e.h.app), 9);
    // and when it is live already, it says so
    e.h.click(1 + 117, y).await?;
    assert!(!rewinding(&e.h.app));
    assert_eq!(note(&e.h.app), "live already");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_the_matrix_theme_the_lineline_is_ascii() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 12)).await?;
    e.start().await?;
    e.enter().await?;
    let matrix =
        e.h.app
            .editor
            .theme_loader
            .load(vortex_view::theme::MATRIX_THEME_NAME)?;
    e.h.app.editor.set_theme(matrix)?;
    assert_eq!(
        e.h.app.editor.theme.effects().dashboard_charset,
        DashboardCharset::Ascii
    );
    for _ in 0..6 {
        e.line().await?;
    }
    e.h.pump_for(Duration::from_millis(50)).await;
    let cells = lineline(&e.h);
    let row: String = cells.iter().collect();
    assert_eq!(cells[117], '|', "the live playhead is ASCII: {row}");
    assert!(
        cells[..117].iter().any(|c| ".:=#&@".contains(*c)),
        "the density ramp is ASCII: {row}"
    );
    assert!(row.is_ascii(), "{row}");

    e.h.keys("hh").await?;
    let cells = lineline(&e.h);
    let row: String = cells.iter().collect();
    assert!(row.is_ascii(), "{row}");
    assert_eq!(cells[117], ']', "{row}");
    assert!(cells[..117].contains(&'|'), "{row}");
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 6. Follow and view

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn following_keeps_the_line_in_the_middle_third_and_centers_it_when_it_leaves(
) -> anyhow::Result<()> {
    // stops in the middle of the first screen (lines 50 to 52), then one far below it
    let (middle, far) = (50, 300);
    let mut e = env_of(800, |p| {
        Script::new(p)
            .lines()
            .stop(Stop::new(middle, "step"))
            .stop(Stop::new(middle + 1, "step"))
            .stop(Stop::new(middle + 2, "step"))
            .stop(Stop::new(far, "step"))
            .stop(Stop::new(far + 1, "step"))
    })
    .await?;
    e.enter().await?;
    e.launch()?;
    e.settled(1).await;
    let height = {
        let (view, _) = current_ref!(e.h.app.editor);
        view.inner_height()
    };
    // (the first screen's middle third holds lines 50 to 52)
    assert!(
        height / 3 < middle as usize - 1 && (middle as usize + 2) < height - height / 3,
        "a view of {height} rows has lines {middle}-{} in its middle third",
        middle + 2
    );
    assert!(
        far as usize > 2 * height,
        "line {far} is well below the first screen"
    );
    let (middle, far) = (middle as usize, far as usize);
    assert_eq!(cursor_line(&e.h.app), middle);
    assert_eq!(
        top_line(&e.h.app),
        1,
        "a line in the middle third doesn't scroll"
    );

    e.line().await?;
    e.line().await?;
    assert_eq!(cursor_line(&e.h.app), middle + 2);
    assert_eq!(
        top_line(&e.h.app),
        1,
        "playback steps within the middle third don't scroll"
    );

    // a line far outside: the view scrolls and centers it
    e.line().await?;
    assert_eq!(cursor_line(&e.h.app), far);
    let top = top_line(&e.h.app);
    assert!(top > 1, "the view scrolled");
    assert!(
        (far - top).abs_diff(height / 2) <= 2,
        "centered: line {far} is {} rows below the top of a {height}-row view",
        far - top
    );
    // the next line stays within the middle third: no scroll
    e.line().await?;
    assert_eq!(cursor_line(&e.h.app), far + 1);
    assert_eq!(top_line(&e.h.app), top);

    // rewind follows the recorded stop the same way
    e.h.keys("hhhh").await?;
    assert!(rewinding(&e.h.app));
    assert_eq!(cursor_line(&e.h.app), middle);
    assert!(top_line(&e.h.app) <= middle && middle < top_line(&e.h.app) + height);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_view_mode_the_stops_dont_move_the_view_and_r_and_b_use_the_cursor() -> anyhow::Result<()>
{
    let mut e = env(|p| run(p, 14)).await?;
    e.start().await?;
    e.enter().await?;
    assert_eq!(cursor_line(&e.h.app), 2);

    e.h.keys("v").await?;
    assert!(viewing(&e.h.app));
    assert!(
        narrow(&e.h.app, 300).1[2].contains(" v  follow"),
        "viewing: v follows"
    );
    // the stops go on, the cursor and the view stay
    e.line().await?;
    e.line().await?;
    assert_eq!(stops(&e.h.app), 3);
    assert_eq!(cursor_line(&e.h.app), 2, "your own cursor");
    assert_eq!(top_line(&e.h.app), 1);

    // your own cursor moves (the arrows: h j k l are the mode's own keys)
    e.h.keys("<down><down><down><down>").await?;
    assert_eq!(cursor_line(&e.h.app), 6);
    // r runs to the cursor's line
    e.h.keys("r").await?;
    e.h.until("the stop at line 6", |_| {
        e.fake.stops_sent().iter().any(|s| s.1 == 6)
    })
    .await;
    e.h.until("playback ended there", |app| {
        spectator(app).mode == Playback::Paused
            && spectator(app).run_to.is_none()
            && spectator(app).step.is_none()
    })
    .await;
    assert_eq!(e.fake.stops_sent().last().map(|s| s.1), Some(6));
    assert_eq!(cursor_line(&e.h.app), 6, "still your cursor");
    assert!(viewing(&e.h.app));

    // b sets the breakpoint at the cursor's line
    e.h.keys("<down><down>b").await?;
    assert_eq!(breakpoints(&e), [(8, BreakpointKind::Any)]);

    // v follows again: the cursor jumps to the stop
    e.h.keys("v").await?;
    assert!(following(&e.h.app));
    assert_eq!(cursor_line(&e.h.app), 6);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn while_following_movement_and_edits_wait_and_the_bar_says_why() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 8)).await?;
    e.start().await?;
    e.enter().await?;
    let original = text(&e.h.app);
    assert_eq!(cursor_line(&e.h.app), 2);

    // a move or a search waits (h j k l are the mode's own keys, the others are the editor's)
    for keys in ["gg", "<down>", "<up>", "<right>", "W", "<C-d>", "/"] {
        clear_note(&mut e.h.app);
        e.h.keys(keys).await?;
        assert_eq!(note(&e.h.app), mode::VIEW_TO_MOVE, "{keys:?}");
        assert_eq!(cursor_line(&e.h.app), 2, "{keys:?} didn't move the cursor");
        if keys == "/" {
            // the search prompt did not open: Esc leaves debug mode only when nothing took it
            assert!(on(&e.h.app));
        }
    }
    assert_eq!(top_line(&e.h.app), 1);
    assert_eq!(e.h.app.editor.mode(), vortex_view::document::Mode::Normal);

    // j is the mode's own key (slower), but after a count it is the editor's: it moves, so it waits
    e.h.app.editor.debug.spectator.speed = 5;
    clear_note(&mut e.h.app);
    e.h.keys("2j").await?;
    assert_eq!(note(&e.h.app), mode::VIEW_TO_MOVE);
    assert_eq!(spectator(&e.h.app).speed, 5);
    assert_eq!(cursor_line(&e.h.app), 2);
    e.h.keys("j").await?;
    assert_eq!(
        spectator(&e.h.app).speed,
        4,
        "alone it is the mode's: slower"
    );

    // an edit waits for Esc
    for keys in ["d", "I", "u"] {
        clear_note(&mut e.h.app);
        e.h.keys(keys).await?;
        assert_eq!(note(&e.h.app), mode::LEAVE_TO_EDIT, "{keys:?}");
        assert_eq!(text(&e.h.app), original, "{keys:?} changed the text");
    }
    assert_eq!(e.h.app.editor.mode(), vortex_view::document::Mode::Normal);
    let (_, doc) = current_ref!(e.h.app.editor);
    assert!(!doc.is_modified());

    // after Esc the same keys edit
    e.h.keys("<esc>").await?;
    assert!(!on(&e.h.app));
    e.h.keys("d").await?;
    assert_ne!(text(&e.h.app), original);
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 7. Breakpoints only in debug mode

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_breakpoint_commands_error_outside_debug_mode() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 4)).await?;
    for command in [
        "dap_toggle_breakpoint",
        "dap_edit_condition",
        "dap_edit_log",
        "debug_breakpoint_kind",
    ] {
        e.h.errors.clear();
        e.h.keys(&format!("<space>?{command}")).await?;
        e.h.pump_for(Duration::from_millis(150)).await;
        e.h.keys("<ret>").await?;
        e.h.until(&format!("{command} to run"), |app| {
            app.editor
                .recent_commands
                .iter()
                .any(|name| name == command)
        })
        .await;
        assert!(
            e.h.errors
                .iter()
                .any(|m| m == "breakpoints are set in debug mode"),
            "{command}: {:?}",
            e.h.errors
        );
    }
    assert!(breakpoints(&e).is_empty());

    // in debug mode the same command sets one
    e.enter().await?;
    e.h.errors.clear();
    e.h.keys("<space>?dap_toggle_breakpoint").await?;
    e.h.pump_for(Duration::from_millis(150)).await;
    e.h.keys("<ret>").await?;
    e.h.until("a breakpoint", |app| !app.editor.breakpoints.is_empty())
        .await;
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gutter_click_sets_a_breakpoint_in_debug_mode_only() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 4)).await?;
    e.h.pump_for(Duration::from_millis(50)).await;
    let rows = e.h.rows();
    let y = rows
        .iter()
        .position(|row| row.contains("step(1);"))
        .unwrap_or_else(|| panic!("the text on the screen:\n{}", dump(&rows))) as u16;

    // outside debug mode: nothing
    e.h.click(0, y).await?;
    assert!(
        breakpoints(&e).is_empty(),
        "outside debug mode a gutter click sets nothing"
    );

    // in it: the click toggles
    e.enter().await?;
    e.h.click(0, y).await?;
    assert_eq!(breakpoints(&e), [(1, BreakpointKind::Any)]);
    e.h.click(0, y).await?;
    assert!(breakpoints(&e).is_empty());
    e.h.finish().await
}

/// What the gutter column shows for `line` (from 0).
fn gutter(app: &Application, line: usize) -> String {
    let editor = &app.editor;
    let (view, doc) = current_ref!(editor);
    let mut column =
        vortex_view::gutter::diagnostics_or_breakpoints(editor, doc, view, &editor.theme, true);
    let mut out = String::new();
    column(line, false, true, &mut out);
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bookmark_and_a_breakpoint_on_a_line_the_gutter_shows_the_one_of_the_mode(
) -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 4)).await?;
    // a bookmark on line 1 (outside debug mode), then a breakpoint on it (inside)
    e.h.keys("<C-s>").await?;
    assert_eq!(gutter(&e.h.app, 0), "1", "the bookmark's number");
    e.enter().await?;
    e.h.keys("b").await?;
    assert_eq!(breakpoints(&e), [(1, BreakpointKind::Any)]);
    assert_eq!(
        gutter(&e.h.app, 0),
        "◯",
        "in debug mode the breakpoint's glyph (hollow: no session verified it)"
    );
    // other kinds, other glyphs
    e.h.keys("B").await?;
    assert_eq!(gutter(&e.h.app, 0), "△");
    e.h.keys("B").await?;
    assert_eq!(gutter(&e.h.app, 0), "▽");
    e.h.keys("B").await?;
    assert_eq!(gutter(&e.h.app, 0), "◯");

    // outside debug mode: the bookmark again, the breakpoint isn't drawn
    e.h.keys("<esc>").await?;
    assert_eq!(gutter(&e.h.app, 0), "1");
    assert_eq!(breakpoints(&e).len(), 1, "the breakpoint is still there");
    // and in debug mode the bookmarks aren't drawn
    e.h.keys("<space>G<down>").await?;
    assert_eq!(gutter(&e.h.app, 0), "◯");
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 8. Breakpoint types

/// Your breakpoint on `line` of type `kind`, set in debug mode as a person does: the cursor
/// there, `b`, and `B` for the type.
async fn typed_breakpoint(e: &mut Env, line: usize, kind: BreakpointKind) -> anyhow::Result<()> {
    e.h.keys(&format!(":goto {line}<ret>")).await?;
    assert_eq!(cursor_line(&e.h.app), line);
    e.h.keys("b").await?;
    let presses = match kind {
        BreakpointKind::Any => 0,
        BreakpointKind::Main => 1,
        BreakpointKind::Sub => 2,
    };
    e.h.keys(&"B".repeat(presses)).await?;
    assert_eq!(breakpoints(e), [(line, kind)]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_main_only_breakpoint_hit_by_a_sub_thread_is_recorded_and_continued() -> anyhow::Result<()>
{
    let mut e = env(two_threads).await?;
    e.enter().await?;
    typed_breakpoint(&mut e, 12, BreakpointKind::Main).await?;
    e.launch()?;
    e.settled(1).await;
    assert!(
        thread_name(&e.h.app, "1"),
        "the camera is on the main thread"
    );

    // thread 2 runs through its lines, 12 included, and is continued at each
    e.h.until("thread 2 ran out", |_| e.fake.advanced(2) == 4)
        .await;
    e.h.until("the hit recorded", |app| breakpoint_marks(app) == 1)
        .await;
    e.h.pump_for(Duration::from_millis(200)).await;
    let record = &e.h.app.editor.debug.engine.record;
    assert_eq!(
        record.main_thread.map(|k| k.thread.to_string()),
        Some("1".into())
    );
    // a continue for thread 2 per line, the one at your breakpoint included
    let continues = e.motion().into_iter().filter(|m| m.1 == 2).count();
    assert_eq!(continues, 4, "{:?}", e.motion());
    // nothing paused: the session is where it was, the camera stays
    assert_eq!(stops(&e.h.app), 1);
    assert!(thread_name(&e.h.app, "1"));
    assert!(matches!(
        e.h.app.editor.debug.status,
        Status::Stopped { .. }
    ));
    assert!(!is_stopped(&e.h.app, "breakpoint"));
    assert_eq!(cursor_line(&e.h.app), 2);
    // the hit is on the lineline
    e.h.pump_for(Duration::from_millis(50)).await;
    assert!(
        lineline(&e.h).contains(&'●'),
        "{}",
        lineline(&e.h).iter().collect::<String>()
    );

    // the gutter shows the type
    assert_eq!(gutter(&e.h.app, 11), "▲");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_main_only_breakpoint_hit_by_the_main_thread_pauses() -> anyhow::Result<()> {
    let mut e = env(two_threads).await?;
    e.enter().await?;
    typed_breakpoint(&mut e, 5, BreakpointKind::Main).await?;
    e.launch()?;
    e.settled(1).await;
    // L: the main thread runs to its breakpoint (line 5) at full speed and pauses there
    e.h.keys("L").await?;
    e.h.until("the breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.settled(2).await;
    assert_eq!(cursor_line(&e.h.app), 5);
    assert!(thread_name(&e.h.app, "1"));
    assert_eq!(breakpoint_marks(&e.h.app), 1);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_only_breakpoint_hit_by_a_sub_thread_pauses_and_takes_the_camera(
) -> anyhow::Result<()> {
    let mut e = env(two_threads).await?;
    e.enter().await?;
    typed_breakpoint(&mut e, 12, BreakpointKind::Sub).await?;
    assert_eq!(gutter(&e.h.app, 11), "▽", "not verified yet");
    e.launch()?;
    e.h.until("the sub-thread's breakpoint", |app| {
        is_stopped(app, "breakpoint")
    })
    .await;
    e.h.until("the camera on thread 2", |app| thread_name(app, "2"))
        .await;
    assert_eq!(breakpoint_marks(&e.h.app), 1);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_only_breakpoint_hit_by_the_main_thread_is_recorded_and_continued(
) -> anyhow::Result<()> {
    let mut e = env(two_threads).await?;
    e.enter().await?;
    typed_breakpoint(&mut e, 5, BreakpointKind::Sub).await?;
    e.launch()?;
    e.settled(1).await;
    e.h.keys("L").await?;
    // the main thread runs through line 5 without pausing, on to the end of the program
    e.h.until("the end of the program", ended).await;
    assert_eq!(breakpoint_marks(&e.h.app), 1, "the hit is recorded");
    // the camera never moved (the session is over, so nothing is spectated now)
    assert!(
        !marks(&e.h.app)
            .iter()
            .any(|kind| matches!(kind, MarkKind::Switch { from: Some(_), .. })),
        "{:?}",
        marks(&e.h.app)
    );
    assert!(
        e.motion().iter().filter(|m| m.1 == 1).count() >= 2,
        "the hit at your breakpoint was continued: {:?}",
        e.motion()
    );
    assert_eq!(stops(&e.h.app), 1, "nothing paused");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_panels_breakpoint_list_follows_the_type() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 8)).await?;
    e.enter().await?;
    typed_breakpoint(&mut e, 3, BreakpointKind::Any).await?;
    let list = |app: &Application| -> Vec<String> {
        debug::panel::rows(&app.editor, Section::Breakpoints)
            .iter()
            .map(|row| row.text())
            .collect()
    };
    let any = list(&e.h.app);
    assert_eq!(any.len(), 1);
    // (no session verified them: the hollow glyphs)
    assert!(any[0].contains('◯'), "{any:?}");
    assert!(!any[0].contains("only"), "{any:?}");

    e.h.keys("B").await?;
    let main = list(&e.h.app);
    assert!(
        main[0].contains('△') && main[0].contains("main thread only"),
        "{main:?}"
    );
    e.h.keys("B").await?;
    let sub = list(&e.h.app);
    assert!(
        sub[0].contains('▽') && sub[0].contains("sub-threads only"),
        "{sub:?}"
    );
    e.h.keys("B").await?;
    assert_eq!(list(&e.h.app), any);
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 9. vx --debug

/// Languages with a debugger each: `alpha` (`true` is installed), `beta` (an adapter that isn't),
/// `gamma` (`false` is installed too). Their one template launches a program: a file prompt. None
/// of them is a DAP server: a launch fails to start the adapter.
fn languages(with_gamma: bool) -> String {
    let language = |name: &str, command: &str| {
        format!(
            r#"
[[language]]
name = "{name}"
scope = "source.{name}"
file-types = ["{name}"]

[language.debugger]
name = "{name}-dap"
transport = "stdio"
command = "{command}"
args = ["5"]

[[language.debugger.templates]]
name = "binary"
request = "launch"
completion = [ {{ name = "binary", completion = "filename" }} ]
args = {{ program = "{{0}}" }}
"#
        )
    };
    let mut out =
        language("alpha", "true") + language("beta", "no-such-debugger-adapter-xyz").as_str();
    if with_gamma {
        out += language("gamma", "false").as_str();
    }
    out
}

fn loader(languages: &str) -> vortex_core::syntax::Loader {
    let value: toml::Value = toml::from_str(languages).unwrap();
    vortex_core::syntax::Loader::new(value.try_into().unwrap()).unwrap()
}

/// A harness whose editor knows only the languages `toml` defines, and the files `prog` (no
/// extension), `x.alpha` and `x.beta` in a folder.
fn launch_env(toml: &str) -> anyhow::Result<(Harness, TempDir)> {
    let tmp = tempfile::tempdir()?;
    for name in ["prog", "x.alpha", "x.beta"] {
        std::fs::write(tmp.path().join(name), "x\n")?;
    }
    let h = Harness::new(AppBuilder::new().with_lang_loader(loader(toml)))?;
    Ok((h, tmp))
}

/// The adapters of the test languages aren't DAP servers: a launch is an attempt that fails to
/// start the adapter, and says so on the status line (an error).
fn launch_was_attempted(h: &Harness) {
    let (message, severity) = h
        .app
        .editor
        .get_status()
        .expect("an error for the adapter that didn't start");
    assert_eq!(*severity, vortex_core::diagnostic::Severity::Error);
    assert!(
        message.starts_with("Failed to start the debug adapter"),
        "{message}"
    );
}

fn target(dir: &Path, program: &str, args: &[&str]) -> launch::Target {
    launch::Target {
        program: Some(dir.join(program)),
        args: args.iter().map(|a| a.to_string()).collect(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_one_installed_debugger_that_fits_launches_directly() -> anyhow::Result<()> {
    let (mut h, tmp) = launch_env(&languages(false))?;
    // an executable: alpha's adapter is installed, beta's is not
    let picker = launch::start_cli(&mut h.app.editor, target(tmp.path(), "prog", &[]));
    assert!(picker.is_none(), "exactly one fits: no picker");
    assert!(on(&h.app), "vx --debug is debug mode");
    launch_was_attempted(&h);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_source_file_gets_its_languages_templates() -> anyhow::Result<()> {
    let (mut h, tmp) = launch_env(&languages(true))?;
    // x.alpha: the templates of alpha only
    assert_eq!(
        launch::source_language(&h.app.editor, &tmp.path().join("x.alpha")).as_deref(),
        Some("alpha")
    );
    let picker = launch::start_cli(&mut h.app.editor, target(tmp.path(), "x.alpha", &[]));
    assert!(picker.is_none(), "one language, one template: launched");
    launch_was_attempted(&h);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn several_debuggers_that_fit_open_a_picker_and_none_is_an_error() -> anyhow::Result<()> {
    let (mut h, tmp) = launch_env(&languages(true))?;
    // an executable fits alpha and gamma (both installed): the picker of those
    let picker = launch::start_cli(&mut h.app.editor, target(tmp.path(), "prog", &[]));
    assert!(picker.is_some(), "two fit: a picker");
    assert!(on(&h.app), "debug mode is on with the picker");
    assert_eq!(h.app.editor.debug.session_name, "", "nothing launched yet");
    assert!(h.app.editor.debug_adapters.iter().next().is_none());

    // the fitting ones, by the module's own rule
    let all = launch::all(&h.app.editor);
    assert_eq!(all.len(), 3);
    let fitting = launch::fitting(all, None, false, |config| config.name != "beta-dap");
    let labels: Vec<_> = fitting.iter().map(|c| c.label()).collect();
    assert_eq!(labels, ["alpha-dap: binary", "gamma-dap: binary"]);

    // a source file of a language whose adapter isn't installed
    h.app.editor.clear_status();
    let none = launch::start_cli(&mut h.app.editor, target(tmp.path(), "x.beta", &[]));
    assert!(none.is_none());
    let status = h.app.editor.get_status().map(|(m, s)| (m.to_string(), *s));
    assert_eq!(
        status,
        Some((
            "no installed debugger of beta launches a source file".to_string(),
            vortex_core::diagnostic::Severity::Error
        ))
    );

    // arguments, and no template takes them
    h.app.editor.clear_status();
    let none = launch::start_cli(
        &mut h.app.editor,
        target(tmp.path(), "prog", &["--port", "1"]),
    );
    assert!(none.is_none());
    assert_eq!(
        h.app.editor.get_status().map(|(m, _)| m.to_string()),
        Some("no installed debugger launches an executable with arguments".to_string())
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_program_the_template_picker_opens_and_a_missing_program_is_an_error(
) -> anyhow::Result<()> {
    let (mut h, tmp) = launch_env(&languages(true))?;
    let picker = launch::start_cli(&mut h.app.editor, launch::Target::default());
    assert!(picker.is_some(), "vx --debug alone: the template picker");
    assert!(on(&h.app));
    assert!(h.app.editor.debug_adapters.iter().next().is_none());

    h.app.editor.clear_status();
    let none = launch::start_cli(&mut h.app.editor, target(tmp.path(), "does-not-exist", &[]));
    assert!(none.is_none());
    let (message, severity) = h.app.editor.get_status().expect("an error");
    assert!(message.contains("doesn't exist"), "{message}");
    assert_eq!(*severity, vortex_core::diagnostic::Severity::Error);

    // the answers to a template's prompts
    let all = launch::all(&h.app.editor);
    let template = &all[0].template;
    assert_eq!(
        launch::params(template, &tmp.path().join("prog"), &[]),
        [tmp.path().join("prog").to_string_lossy().into_owned()]
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// 10. Nothing on the status line

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_whole_session_in_debug_mode_prints_nothing_but_errors_to_the_status_line(
) -> anyhow::Result<()> {
    let mut e =
        env(|p| run(p, 9).capability("supportsSingleThreadExecutionRequests", json!(true))).await?;
    e.breakpoint(7);
    e.enter().await?;
    e.h.app.editor.clear_status();
    e.h.infos.clear();

    // a check after every step: nothing in the status line is an info message
    macro_rules! quiet {
        ($e:expr, $what:expr) => {
            if let Some((message, severity)) = $e.h.app.editor.get_status() {
                assert_eq!(
                    *severity,
                    vortex_core::diagnostic::Severity::Error,
                    "{}: an info message on the status line: {message:?}",
                    $what
                );
            }
            assert!($e.h.infos.is_empty(), "{}: {:?}", $what, $e.h.infos);
        };
    }
    quiet!(e, "debug mode without a session");

    // launch
    e.launch()?;
    e.settled(1).await;
    quiet!(e, "launch");
    // play, pause
    e.h.keys("<space>").await?;
    e.h.until("two stops", |app| stops(app) >= 3).await;
    quiet!(e, "play");
    e.h.keys("<space>").await?;
    e.h.until("paused", |app| spectator(app).step.is_none())
        .await;
    quiet!(e, "pause");
    // rewind with h, live with p
    e.h.keys("h").await?;
    quiet!(e, "h");
    e.h.keys("h").await?;
    quiet!(e, "h again");
    e.h.keys("l").await?;
    quiet!(e, "l in rewind");
    e.h.keys("p").await?;
    quiet!(e, "p");
    e.h.keys("p").await?;
    quiet!(e, "p when live");
    // speed, follow
    for key in ["k", "j", "e", "e", "e", "v", "v", "X"] {
        e.h.keys(key).await?;
        quiet!(e, key);
    }
    assert_eq!(
        note(&e.h.app),
        "Time labels cleared",
        "the notes went to the bar"
    );
    // a step
    let next = stops(&e.h.app) + 1;
    e.h.keys(".").await?;
    e.settled(next).await;
    quiet!(e, ".");
    // a breakpoint hit, at full speed
    e.h.keys("L").await?;
    e.h.until("the breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.settled(stops(&e.h.app)).await;
    quiet!(e, "a breakpoint hit");
    // H and L through the marks
    e.h.keys("H").await?;
    quiet!(e, "H");
    e.h.keys("L").await?;
    quiet!(e, "L in rewind");
    // the panel, back
    e.h.keys("<tab><tab>").await?;
    quiet!(e, "tab");
    // the end of the program
    e.h.keys("L").await?;
    e.h.until("the end", ended).await;
    e.h.pump_for(Duration::from_millis(200)).await;
    quiet!(e, "the end");
    e.h.keys("<esc>").await?;
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// 11. The spectator's flows through the new keys

fn x_scope(x: &str) -> super::fake_adapter::Scope {
    super::fake_adapter::Scope::new("Locals").var(super::fake_adapter::Var::new("x", x))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f_fast_forwards_to_the_next_change_of_a_watched_value() -> anyhow::Result<()> {
    let mut e = env(|p| {
        ["1", "1", "1", "1", "2", "2", "2"]
            .iter()
            .enumerate()
            .fold(Script::new(p).lines(), |s, (i, x)| {
                s.stop(Stop::new(2 + i as u64, "step").scope(x_scope(x)))
            })
    })
    .await?;
    e.start().await?;
    e.enter().await?;
    // w adds a watch
    // (the prompt opens through a callback of the editor's: wait for it)
    e.h.keys("w").await?;
    e.h.pump_for(Duration::from_millis(200)).await;
    e.h.keys("x<ret>").await?;
    e.h.until("the watch", |app| {
        debug::panel::rows(&app.editor, Section::Watch)
            .iter()
            .any(|row| row.text().starts_with("x = 1"))
    })
    .await;
    e.h.keys("f").await?;
    e.h.until("the change", |app| !spectator(app).changes.is_empty())
        .await;
    e.h.until("paused", |app| spectator(app).mode == Playback::Paused)
        .await;
    let changes = &spectator(&e.h.app).changes;
    assert_eq!(
        (changes[0].old.as_str(), changes[0].new.as_str()),
        ("1", "2")
    );
    assert_eq!(e.fake.stops_sent().len(), 5);
    assert!(on(&e.h.app));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r_in_view_mode_runs_to_the_cursor_and_the_line_leaves_the_engines_run_to_alone(
) -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 12)).await?;
    e.start().await?;
    e.enter().await?;
    e.h.keys("v").await?;
    e.h.keys(":goto 7<ret>").await?;
    assert_eq!(cursor_line(&e.h.app), 7);
    e.h.keys("r").await?;
    e.h.until("the stop at line 7", |_| {
        e.fake.stops_sent().iter().any(|s| s.1 == 7)
    })
    .await;
    e.h.until("playback paused", |app| {
        spectator(app).mode == Playback::Paused && spectator(app).run_to.is_none()
    })
    .await;
    assert_eq!(e.fake.stops_sent().last().map(|s| s.1), Some(7));
    // lines 3 to 6 were passed on a continue each, not shown
    assert_eq!(e.fake.stops_sent().len(), 6);
    assert_eq!(stops(&e.h.app), 2, "the first line and the one run to");
    assert!(viewing(&e.h.app));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn greater_and_less_spectate_the_other_threads_z_freezes_and_x_clears_the_labels(
) -> anyhow::Result<()> {
    // thread 2's first line is your breakpoint: it stays there, and the camera ends on it
    let mut e = env(two_threads).await?;
    e.breakpoint(10);
    e.launch()?;
    e.h.until("thread 2's breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.enter().await?;
    assert!(thread_name(&e.h.app, "2"));

    // t opens the picker of the threads
    assert!(e.tap("t").await?, "t opens the thread picker");
    // < spectates the previous thread, > the next
    e.h.keys("<lt>").await?;
    assert!(thread_name(&e.h.app, "1"));
    e.h.keys("<gt>").await?;
    assert!(thread_name(&e.h.app, "2"));

    // z freezes it: it never runs; a step of the other thread goes alone
    e.h.keys("z").await?;
    let frozen = |app: &Application| {
        spectator(app)
            .frozen
            .iter()
            .any(|key| key.thread.to_string() == "2")
    };
    assert!(frozen(&e.h.app));
    e.h.keys("<lt>").await?;
    let next = stops(&e.h.app) + 1;
    e.h.keys("l").await?;
    e.settled(next).await;
    assert!(
        e.motion().iter().any(|m| m.1 == 1),
        "thread 1 stepped: {:?}",
        e.motion()
    );
    assert_eq!(e.fake.advanced(2), 1, "the frozen thread didn't advance");
    e.h.keys("<gt>z<lt>").await?;
    assert!(!frozen(&e.h.app), "z thaws it");

    // X clears the time labels
    e.h.keys("X").await?;
    assert_eq!(note(&e.h.app), "Time labels cleared");
    e.h.finish().await
}
