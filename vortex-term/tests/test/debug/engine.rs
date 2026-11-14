//! The per-line breakpoint engine against the scripted fake adapter: how it finds the executable
//! lines of the code, installs its hidden breakpoints merged with yours, decides at each hit
//! whether to pause or continue, records every hit, runs at full speed, follows the follow mode
//! and steps by hand.
//!
//! The fake runs in line mode (`Script::lines()`): its `"step"` stops are the lines the program
//! runs, and a `continue` stops only at the lines with a breakpoint, hidden or yours. The session
//! is launched with `own_paths` set to a temporary folder holding `main.rs` (30 lines), so the
//! hidden breakpoints go there and nowhere else.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use vortex_core::syntax::config::DebugAdapterConfig;
use vortex_dap::StepKind;
use vortex_term::{application::Application, debug, debug::panel};
use vortex_view::{
    current_ref,
    debug::{
        engine::{Engine, EngineKind, Found, SessionEngine, SessionState, LINE_REASON},
        lineline::{MarkKind, Why},
        spectator::{Follow, Mode},
        Section, Status,
    },
};

use super::{
    fake_adapter::{FakeAdapter, Scope, Script, Stop, Var},
    harness::Harness,
};
use crate::test::helpers::AppBuilder;

struct Env {
    h: Harness,
    fake: FakeAdapter,
    /// The path the editor knows the file by.
    path: PathBuf,
    dir: PathBuf,
    _tmp: TempDir,
}

async fn env(script: impl FnOnce(&str) -> Script) -> anyhow::Result<Env> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().canonicalize()?;
    let path = dir.join("main.rs");
    let source: String = (1..=30).map(|i| format!("    step({i});\n")).collect();
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

fn adapter_config(
    dir: &std::path::Path,
    capabilities: Option<serde_json::Map<String, Value>>,
) -> DebugAdapterConfig {
    DebugAdapterConfig {
        name: "fake".into(),
        transport: "tcp".into(),
        command: String::new(),
        args: Vec::new(),
        port_arg: None,
        templates: Vec::new(),
        quirks: Default::default(),
        // the engine puts its hidden breakpoints under these folders only (else the whole
        // workspace, that is the whole repository when the tests run)
        own_paths: Some(vec![dir.to_owned()]),
        std_paths: None,
        lib_paths: None,
        capabilities,
    }
}

impl Env {
    fn launch(&mut self) -> anyhow::Result<()> {
        debug::start(
            &mut self.h.app.editor,
            Some(self.fake.addr),
            &adapter_config(&self.dir, None),
            "fake",
            "launch",
            json!({"program": self.dir.join("prog"), "cwd": self.dir}),
        )
    }

    /// Your breakpoint on `line` (1-based), as debug mode's `b` makes it.
    fn breakpoint(&mut self, line: usize) {
        debug::breakpoints::toggle(&mut self.h.app.editor, self.path.clone(), line - 1);
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

    /// Debug mode on, the editor focused: playback, speed and thread keys are its own.
    async fn debug_mode(&mut self) -> anyhow::Result<()> {
        if !self.h.app.editor.debug.mode_on() {
            self.h.keys("<space>G").await?;
        }
        assert!(self.h.app.editor.debug.mode_on());
        Ok(())
    }

    /// Speed 25 lines a second.
    async fn fast(&mut self) -> anyhow::Result<()> {
        self.h.keys("kkkkkkkk").await?;
        assert_eq!(spectator(&self.h.app).speed_label(), "25 l/s");
        Ok(())
    }

    /// The `setBreakpoints` requests for `main.rs`: the lines asked for (1-based), in order.
    fn sets(&self) -> Vec<Vec<u64>> {
        sets_of(&self.fake, &self.path)
    }

    /// The execution requests, in order: (command, thread, singleThread).
    fn motion(&self) -> Vec<(String, i64, bool)> {
        self.fake
            .requests()
            .into_iter()
            .filter(|(_, c, _)| matches!(c.as_str(), "continue" | "next" | "stepIn" | "stepOut"))
            .map(|(_, c, a)| {
                (
                    c,
                    a["threadId"].as_i64().unwrap_or(-1),
                    a["singleThread"].as_bool().unwrap_or(false),
                )
            })
            .collect()
    }

    fn motion_names(&self) -> Vec<String> {
        self.motion().into_iter().map(|(c, ..)| c).collect()
    }

    /// The lines of the stops the fake sent, in order.
    fn stop_lines(&self) -> Vec<u64> {
        self.fake.stops_sent().iter().map(|s| s.1).collect()
    }

    /// The lines of `main.rs` the hits of the record are on (1-based), in order.
    fn hit_lines(&self) -> Vec<u32> {
        engine(&self.h.app)
            .record
            .iter()
            .map(|hit| hit.line + 1)
            .collect()
    }
}

fn sets_of(fake: &FakeAdapter, path: &std::path::Path) -> Vec<Vec<u64>> {
    let path = path.to_string_lossy().to_string();
    fake.requests_named("setBreakpoints")
        .into_iter()
        .filter(|(_, args)| args["source"]["path"] == path.as_str())
        .map(|(_, args)| lines_of(&args))
        .collect()
}

fn lines_of(args: &Value) -> Vec<u64> {
    args["breakpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["line"].as_u64().unwrap())
        .collect()
}

fn spectator(app: &Application) -> &vortex_view::debug::spectator::Spectator {
    &app.editor.debug.spectator
}

fn engine(app: &Application) -> &Engine {
    &app.editor.debug.engine
}

/// The engine's state in the session (there is one).
fn session(app: &Application) -> &SessionEngine {
    engine(app)
        .sessions
        .values()
        .next()
        .expect("the engine has a session")
}

fn is_ready(app: &Application) -> bool {
    engine(app)
        .sessions
        .values()
        .all(|s| s.state == SessionState::Ready)
        && engine(app).task.is_none()
}

fn texts(app: &Application, section: Section) -> Vec<String> {
    panel::rows(&app.editor, section)
        .iter()
        .map(|row| row.text())
        .collect()
}

/// Whether the debugger said `text`: on debug mode's note while it is on, else on the status line.
fn said(app: &Application, text: &str) -> bool {
    match &app.editor.debug.mode {
        Some(mode) => mode.note.as_ref().is_some_and(|note| note.contains(text)),
        None => app
            .editor
            .get_status()
            .is_some_and(|(message, _)| message.contains(text)),
    }
}

fn is_stopped(app: &Application, reason: &str) -> bool {
    matches!(&app.editor.debug.status, Status::Stopped { reason: r, .. } if r == reason)
}

fn thread_name(app: &Application, thread: u16) -> String {
    engine(app)
        .record
        .thread(thread)
        .unwrap()
        .key
        .thread
        .to_string()
}

/// `n` line stops of one thread at lines 2, 3, ...
fn lines(p: &str, n: u64) -> Script {
    (0..n).fold(Script::new(p).lines(), |script, i| {
        script.stop(Stop::new(2 + i, "step"))
    })
}

fn single_thread_cap(script: Script) -> Script {
    script.capability("supportsSingleThreadExecutionRequests", json!(true))
}

/// A queue of line stops.
fn queue(lines: &[u64]) -> Vec<Stop> {
    lines.iter().map(|l| Stop::new(*l, "step")).collect()
}

fn all_lines() -> Vec<u64> {
    (1..=30).collect()
}

// ---------------------------------------------------------------------------------------------
// Which lines get a hidden breakpoint

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_executable_lines_come_from_breakpoint_locations_when_the_adapter_has_them(
) -> anyhow::Result<()> {
    let wanted = [2, 3, 4, 5, 10, 12];
    let mut e = env(|p| lines(p, 6).breakpoint_locations(p, &wanted)).await?;
    e.start().await?;
    e.h.until("the engine ready", is_ready).await;

    // one request for the one file, for all its lines
    let locations = e.fake.requests_named("breakpointLocations");
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert_eq!(
        locations[0].1["source"]["path"],
        e.path.to_string_lossy().as_ref()
    );
    assert_eq!(locations[0].1["line"], 1);
    assert_eq!(locations[0].1["endLine"], 30);
    // the breakpoints are exactly those lines, 1-based, in one request
    assert_eq!(e.sets(), [wanted.to_vec()]);
    let files = &session(&e.h.app).files;
    let file = &files[&e.path];
    assert_eq!(file.lines, [1, 2, 3, 4, 9, 11], "0-based");
    assert_eq!(file.found, Found::Locations);
    assert_eq!(files.len(), 1, "only main.rs is under the own paths");
    assert_eq!(session(&e.h.app).count(), 6);
    e.h.finish().await
}

/// A breakpoint on every line first, then again without the ones the adapter moved or didn't
/// verify.
async fn verify_and_drop(
    script: impl FnOnce(&str) -> Script,
    locations: bool,
) -> anyhow::Result<()> {
    let mut e = env(script).await?;
    e.start().await?;
    e.h.until("the engine ready", is_ready).await;

    assert_eq!(
        e.fake.requests_named("breakpointLocations").len(),
        usize::from(locations),
        "asked once when the adapter says it has the request, never else"
    );
    let sets = e.sets();
    assert_eq!(sets.len(), 2, "{sets:?}");
    assert_eq!(sets[0], all_lines(), "a breakpoint on every line first");
    assert_eq!(
        sets[1],
        [2, 3, 5, 8],
        "then only the lines the adapter kept where asked"
    );
    let file = &session(&e.h.app).files[&e.path];
    assert_eq!(file.lines, [1, 2, 4, 7], "0-based");
    assert_eq!(file.found, Found::Verified);
    assert!(file.strays.is_empty());
    assert_eq!(session(&e.h.app).count(), 4);
    // the first stop is the first line of the code that ran
    assert!(
        is_stopped(&e.h.app, LINE_REASON),
        "{:?}",
        e.h.app.editor.debug.status
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_breakpoint_locations_the_engine_keeps_the_lines_the_adapter_verified(
) -> anyhow::Result<()> {
    verify_and_drop(|p| lines(p, 4).executable_lines(p, &[2, 3, 5, 8]), false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_answer_to_breakpoint_locations_falls_back_to_verifying_each_line(
) -> anyhow::Result<()> {
    verify_and_drop(
        |p| {
            lines(p, 4)
                .executable_lines(p, &[2, 3, 5, 8])
                .breakpoint_locations_empty()
        },
        true,
    )
    .await
}

// ---------------------------------------------------------------------------------------------
// Merged with your breakpoints

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn your_breakpoints_and_the_hidden_ones_go_in_one_request_yours_first() -> anyhow::Result<()>
{
    let mut e = env(|p| lines(p, 8)).await?;
    e.breakpoint(5);
    e.start().await?;
    e.h.until("the engine ready", is_ready).await;

    let sets = e.sets();
    assert_eq!(sets.len(), 1, "{sets:?}");
    let mut expected = vec![5];
    expected.extend(all_lines().into_iter().filter(|line| *line != 5));
    assert_eq!(
        sets[0], expected,
        "yours first, and no hidden one on your line"
    );
    assert_eq!(
        session(&e.h.app).files[&e.path].lines.len(),
        30,
        "the lines found"
    );
    assert_eq!(
        session(&e.h.app).count(),
        29,
        "the hidden breakpoints in place"
    );

    // the hidden ones are never shown: not in the editor's list, not in the panel
    let list = &e.h.app.editor.breakpoints[&e.path];
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].line, 4);
    assert!(list[0].verified, "the answer reached it");
    let rows = texts(&e.h.app, Section::Breakpoints);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].ends_with(":5"), "{rows:?}");

    // playing reaches it: playback pauses there, as for any breakpoint
    e.debug_mode().await?;
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("your breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.h.until("playback paused", |app| {
        spectator(app).mode == Mode::Paused && spectator(app).step.is_none()
    })
    .await;
    assert_eq!(e.stop_lines(), [2, 3, 4, 5]);
    assert!(said(&e.h.app, "Playback paused: breakpoint"));
    let hits: Vec<_> = engine(&e.h.app).record.iter().map(|h| h.why).collect();
    assert_eq!(hits, [Why::Line, Why::Line, Why::Line, Why::Breakpoint]);
    e.h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(e.fake.stops_sent().len(), 4, "nothing after it");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_false_condition_skips_and_a_logpoint_prints_and_neither_stops_playback(
) -> anyhow::Result<()> {
    let mut e = env(|p| {
        let script = Script::new(p)
            .lines()
            .conditional_breakpoints()
            .log_points();
        (0..8).fold(script, |script, i| {
            script
                .stop(Stop::new(2 + i, "step").scope(Scope::new("Locals").var(Var::new("x", "0"))))
        })
    })
    .await?;
    use vortex_view::editor::Breakpoint;
    e.h.app.editor.breakpoints.insert(
        e.path.clone(),
        vec![
            Breakpoint {
                line: 3, // line 4: x is 0
                condition: Some("x".into()),
                ..Default::default()
            },
            Breakpoint {
                line: 5, // line 6
                log_message: Some("reached six".into()),
                ..Default::default()
            },
        ],
    );
    e.start().await?;
    e.h.until("the engine ready", is_ready).await;
    let sets = e.sets();
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!(&sets[0][..2], [4, 6], "yours first");
    assert_eq!(sets[0].len(), 30);

    e.debug_mode().await?;
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("the end", |app| {
        matches!(app.editor.debug.status, Status::Ended { .. })
    })
    .await;
    // never paused by them
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    assert!(
        !e.h.errors.iter().any(|m| m.contains("paused")),
        "{:?}",
        e.h.errors
    );
    // the logpoint printed, once
    let printed =
        e.h.app
            .editor
            .debug
            .console
            .lines
            .iter()
            .filter(|l| l.text.contains("reached six"))
            .count();
    assert_eq!(printed, 1);
    // A line of yours that doesn't stop has no hidden breakpoint either, so the program runs
    // through it unseen: playback shows the other lines only (by design of the engine).
    assert_eq!(e.stop_lines(), [2, 3, 5, 7, 8, 9]);
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// At each hit

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_spectated_thread_is_paced_and_every_line_is_recorded() -> anyhow::Result<()> {
    let mut e = env(|p| lines(p, 12)).await?;
    // the 11th line stops at your breakpoint, so playback ends there
    e.breakpoint(12);
    e.start().await?;
    // the first line of the code that ran: paused, spectated, shown as a line
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    assert_eq!(e.fake.stops_sent().len(), 1);
    assert!(is_stopped(&e.h.app, LINE_REASON));
    assert!(e
        .h
        .app
        .editor
        .get_status()
        .is_some_and(|(m, _)| m.contains("Stopped at the first line of your code that ran")));
    assert_eq!(
        spectator(&e.h.app).spectated.map(|k| k.thread.to_string()),
        Some("1".into())
    );
    assert_eq!(spectator(&e.h.app).history.stops.len(), 1);

    e.debug_mode().await?;
    e.fast().await?;
    let start = Instant::now();
    e.h.keys("<space>").await?;
    e.h.until("your breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    let took = start.elapsed();
    // 10 more lines: the first at once, the others 40 ms after the one before
    assert!(
        took >= Duration::from_millis(9 * 40),
        "10 lines took only {took:?}"
    );
    assert_eq!(e.stop_lines(), (2..=12).collect::<Vec<_>>());
    e.h.until("playback paused", |app| spectator(app).mode == Mode::Paused)
        .await;
    // each shown, and each recorded
    assert_eq!(spectator(&e.h.app).history.stops.len(), 11);
    assert_eq!(engine(&e.h.app).record.len(), 11);
    assert_eq!(engine(&e.h.app).record.counter(), 11);
    assert_eq!(e.hit_lines(), (2..=12).collect::<Vec<_>>());
    // all of them continued by `continue`, never stepped
    assert_eq!(e.motion_names(), ["continue"; 10]);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hits_of_another_thread_are_continued_at_once_and_only_recorded() -> anyhow::Result<()>
{
    // two threads that stop one by one (non-stop): thread 2 runs through its queue unseen
    let mut e = env(|p| {
        single_thread_cap(
            Script::new(p)
                .lines()
                .threads()
                .non_stop()
                .thread_program(1, queue(&[2, 3, 4, 5, 6]))
                .thread_program(2, queue(&[10, 11, 12, 13])),
        )
    })
    .await?;
    e.start().await?;
    assert_eq!(
        spectator(&e.h.app).spectated.map(|k| k.thread.to_string()),
        Some("1".into())
    );
    e.h.until("thread 2 ran out", |_| e.fake.advanced(2) == 4)
        .await;
    e.h.until("its last hits recorded", |app| {
        engine(app).record.len() >= 5
    })
    .await;
    e.h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(e.fake.advanced(1), 1, "thread 1 waits at its first line");
    assert_eq!(
        spectator(&e.h.app).history.stops.len(),
        1,
        "thread 2 was never shown"
    );
    assert!(is_stopped(&e.h.app, LINE_REASON));
    // thread 2's continues are the engine's: one per hit but the last
    let continues: Vec<_> = e.motion().into_iter().filter(|m| m.1 == 2).collect();
    assert_eq!(continues.len(), 4, "{continues:?}");

    // one counter for both threads
    let record = &engine(&e.h.app).record;
    let hits: Vec<_> = record
        .iter()
        .map(|hit| {
            (
                hit.at,
                thread_name(&e.h.app, 0) == hit.thread.thread.to_string(),
                hit.line + 1,
                hit.why,
            )
        })
        .collect();
    assert_eq!(record.counter(), 5);
    assert_eq!(hits.len(), 5, "{hits:?}");
    assert!(hits.iter().enumerate().all(|(i, hit)| hit.0 == i as u64));
    assert_eq!(hits[0].2, 2, "thread 1's first line");
    assert_eq!(hits.iter().filter(|hit| hit.1).count(), 1);
    assert_eq!(
        hits[1..].iter().map(|h| h.2).collect::<Vec<_>>(),
        [10, 11, 12, 13]
    );
    assert!(hits.iter().all(|hit| hit.3 == Why::Line));

    // playing thread 1 goes on being paced and shown, and thread 2 has nothing left to say
    e.debug_mode().await?;
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("thread 1 ran out", |app| {
        matches!(app.editor.debug.status, Status::Ended { .. }) || engine(app).record.len() >= 9
    })
    .await;
    assert_eq!(e.fake.advanced(1), 5);
    assert_eq!(e.fake.advanced(2), 4);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_all_stop_continue_runs_the_other_thread_along_unseen() -> anyhow::Result<()> {
    // lldb style: a continue resumes every thread, and only one of them reports each stop
    let mut e = env(|p| {
        Script::new(p)
            .lines()
            .threads()
            .all_stop_continue()
            .thread_program(1, queue(&[2, 3, 4, 5]))
            .thread_program(2, queue(&[10, 11, 12, 13, 14]))
    })
    .await?;
    e.start().await?;
    assert_eq!(
        e.fake.advanced(2),
        1,
        "thread 2 is stopped at its first line, silently"
    );
    e.debug_mode().await?;
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("thread 1's last line", |app| engine(app).record.len() >= 4)
        .await;
    e.h.until("four shown", |app| spectator(app).history.stops.len() >= 4)
        .await;
    // thread 2 moved along with each of thread 1's lines (it started at its first), and only
    // thread 1 was shown
    assert_eq!(e.fake.advanced(1), 4);
    assert!(e.fake.advanced(2) >= 4);
    // (the program may already have ended: a last continue past thread 1's end)
    assert_eq!(
        e.motion()[..3],
        vec![("continue".to_string(), 1, false); 3],
        "plain continues of the spectated thread"
    );
    let record = &engine(&e.h.app).record;
    assert!(record.len() >= 4);
    // thread 2 only shows up once thread 1 is out of lines (it then reports its own)
    assert!(record
        .iter()
        .take(4)
        .all(|hit| hit.thread.thread.to_string() == "1"));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_breakpoint_of_yours_on_another_thread_cuts_the_camera_to_it() -> anyhow::Result<()> {
    // thread 2's third line (12) is your breakpoint, and the program takes a while to get there
    let mut e = env(|p| {
        single_thread_cap(
            Script::new(p)
                .lines()
                .threads()
                .non_stop()
                .thread_program(1, queue(&[2, 3, 4, 5, 6, 7, 8, 9]))
                .thread_program(
                    2,
                    vec![
                        Stop::new(10, "step"),
                        Stop::new(11, "step"),
                        Stop::new(12, "step").delay(1500),
                    ],
                ),
        )
    })
    .await?;
    e.breakpoint(12);
    e.start().await?;
    assert_eq!(
        spectator(&e.h.app).spectated.map(|k| k.thread.to_string()),
        Some("1".into())
    );
    e.debug_mode().await?;
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Playing);
    // thread 2 reaches your breakpoint: playback pauses and the camera goes to it
    e.h.until("thread 2's breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.h.until("playback paused", |app| spectator(app).mode == Mode::Paused)
        .await;
    assert_eq!(
        spectator(&e.h.app).spectated.map(|k| k.thread.to_string()),
        Some("2".into())
    );
    assert!(said(&e.h.app, "Playback paused: breakpoint"));
    let record = &engine(&e.h.app).record;
    let last = record.iter().last().unwrap();
    assert_eq!(
        (last.thread.thread.to_string(), last.line + 1, last.why),
        ("2".into(), 12, Why::Breakpoint)
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frozen_thread_is_respected_continues_are_single_thread_and_it_never_advances(
) -> anyhow::Result<()> {
    let mut e = env(|p| {
        single_thread_cap(
            Script::new(p)
                .lines()
                .threads()
                .non_stop()
                .thread_program(1, queue(&[2, 3, 4, 5, 6]))
                // its first line is your breakpoint: it stays there
                .thread_program(2, queue(&[10, 11, 12])),
        )
    })
    .await?;
    e.breakpoint(10);
    e.launch()?;
    // the camera ends on thread 2 (your breakpoint), the other thread is stopped at its line
    e.h.until("thread 2's breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.debug_mode().await?;
    assert_eq!(e.fake.advanced(2), 1);
    // freeze it and watch thread 1 again
    e.h.keys("z<lt>").await?;
    assert_eq!(spectator(&e.h.app).frozen.len(), 1);
    assert_eq!(
        spectator(&e.h.app).spectated.map(|k| k.thread.to_string()),
        Some("1".into())
    );
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("thread 1 ran out", |app| {
        matches!(app.editor.debug.status, Status::Ended { .. })
            || engine(app)
                .record
                .iter()
                .filter(|h| h.line + 1 == 6)
                .count()
                > 0
    })
    .await;
    // every continue carries singleThread, and went to thread 1
    let motion = e.motion();
    assert!(!motion.is_empty());
    assert!(
        motion
            .iter()
            .all(|(c, t, single)| c == "continue" && *t == 1 && *single),
        "{motion:?}"
    );
    assert_eq!(e.fake.advanced(2), 1, "the frozen thread didn't advance");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frozen_thread_stays_put_in_an_all_stop_session_while_another_threads_hits_continue(
) -> anyhow::Result<()> {
    let mut e = env(|p| {
        single_thread_cap(
            Script::new(p)
                .lines()
                .threads()
                .all_stop_continue()
                .thread_program(1, queue(&[2, 3, 4, 5]))
                .thread_program(2, queue(&[10, 11, 12, 13]))
                .thread_program(3, queue(&[20, 21, 22, 23, 24])),
        )
    })
    .await?;
    e.start().await?;
    e.debug_mode().await?;
    assert_eq!(
        spectator(&e.h.app).spectated.map(|k| k.thread.to_string()),
        Some("1".into())
    );
    assert_eq!(e.fake.advanced(1), 1);

    // freeze thread 2 (spectate it, z), and watch thread 1 again
    e.h.keys("<gt>").await?;
    assert_eq!(
        spectator(&e.h.app).spectated.map(|k| k.thread.to_string()),
        Some("2".into())
    );
    e.h.keys("z<lt>").await?;
    assert_eq!(spectator(&e.h.app).frozen.len(), 1);
    assert_eq!(
        spectator(&e.h.app).spectated.map(|k| k.thread.to_string()),
        Some("1".into())
    );

    // continue thread 3, stopped silently at its first line, while thread 1 stays spectated
    let mut key = spectator(&e.h.app).spectated.unwrap();
    key.thread = serde_json::from_value(json!(3))?;
    debug::threads::continue_thread(&mut e.h.app.editor, key);
    // (a plain continue of thread 3's hit would resume every thread: the frozen one moves, and
    // thread 1 reports the next stop while thread 3 stays silent)
    let fake = &e.fake;
    e.h.until("thread 3 ran on, or the frozen thread moved", move |app| {
        fake.advanced(2) > 1
            || engine(app)
                .record
                .iter()
                .any(|h| h.thread.thread.to_string() == "3" && h.line + 1 >= 22)
    })
    .await;
    assert_eq!(e.fake.advanced(2), 1, "the frozen thread didn't move");
    let motion = e.motion();
    let of_three: Vec<_> = motion.iter().filter(|(_, t, _)| *t == 3).collect();
    assert!(!of_three.is_empty(), "{motion:?}");
    assert!(
        of_three
            .iter()
            .all(|(c, _, single)| c == "continue" && *single),
        "every continue of thread 3 is alone: {motion:?}"
    );
    assert_eq!(e.fake.advanced(2), 1, "the frozen thread didn't move");
    assert_eq!(e.fake.advanced(1), 1, "thread 1 stayed where it was");
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// The record

/// One queue of line stops of two threads, interleaved, with a thread joining and leaving.
fn interleaved(p: &str) -> Script {
    Script::new(p)
        .lines()
        .stop(Stop::new(2, "step"))
        .stop(Stop::new(10, "step").thread(2).thread_started(2, "worker"))
        .stop(Stop::new(3, "step"))
        .stop(Stop::new(11, "step").thread(2))
        .stop(Stop::new(4, "step"))
        .stop(Stop::new(12, "step").thread(2).thread_exited(2))
        .stop(Stop::new(5, "step"))
        .stop(Stop::new(6, "step"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_record_counts_every_hit_of_every_thread_and_marks_what_happened() -> anyhow::Result<()>
{
    let mut e = env(interleaved).await?;
    e.breakpoint(6);
    e.start().await?;
    e.debug_mode().await?;
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("the breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.h.until("paused", |app| spectator(app).mode == Mode::Paused)
        .await;
    e.h.keys("<space>").await?;
    e.h.until("the end", |app| {
        matches!(app.editor.debug.status, Status::Ended { .. })
    })
    .await;

    let record = &engine(&e.h.app).record;
    // every hit, on one counter
    assert_eq!(record.counter(), 8);
    let hits: Vec<(u64, String, u32, Why)> = record
        .iter()
        .map(|hit| (hit.at, hit.thread.thread.to_string(), hit.line + 1, hit.why))
        .collect();
    let thread = |t: &str| t.to_string();
    assert_eq!(
        hits,
        [
            (0, thread("1"), 2, Why::Line),
            (1, thread("2"), 10, Why::Line),
            (2, thread("1"), 3, Why::Line),
            (3, thread("2"), 11, Why::Line),
            (4, thread("1"), 4, Why::Line),
            (5, thread("2"), 12, Why::Line),
            (6, thread("1"), 5, Why::Line),
            (7, thread("1"), 6, Why::Breakpoint),
        ]
    );
    assert!(record.iter().all(|hit| hit.path == Some(e.path.as_path())));
    // the first thread reported is the main thread
    assert_eq!(
        record.main_thread.map(|k| k.thread.to_string()),
        Some("1".into())
    );

    // threads and their lifetimes
    let threads = record.threads();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0].key.thread.to_string(), "1");
    assert_eq!(
        (threads[0].first, threads[0].last, threads[0].exited),
        (Some(0), Some(7), None)
    );
    assert_eq!(threads[1].key.thread.to_string(), "2");
    assert_eq!(
        threads[1].started, 1,
        "reported when its first line was hit"
    );
    assert_eq!((threads[1].first, threads[1].last), (Some(1), Some(5)));
    assert_eq!(
        threads[1].exited,
        Some(5),
        "it left just before its last line was reported"
    );

    // marks: the camera, the thread coming and going, your breakpoint, the end
    let marks: Vec<_> = record.marks().map(|m| (m.at, m.kind.clone())).collect();
    assert!(
        marks.contains(&(0, MarkKind::Switch { from: None, to: 0 })),
        "{marks:?}"
    );
    assert!(
        marks.contains(&(1, MarkKind::ThreadStarted { thread: 1 })),
        "{marks:?}"
    );
    assert!(
        marks.contains(&(5, MarkKind::ThreadExited { thread: 1 })),
        "{marks:?}"
    );
    assert!(
        marks.contains(&(7, MarkKind::Breakpoint { thread: 0 })),
        "{marks:?}"
    );
    assert_eq!(marks.last(), Some(&(8, MarkKind::End)), "{marks:?}");
    assert_eq!(
        marks
            .iter()
            .filter(|(_, k)| matches!(k, MarkKind::Switch { .. }))
            .count(),
        1,
        "the camera never moved"
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_the_spectated_thread_is_marked() -> anyhow::Result<()> {
    // thread 2's line is your breakpoint: it takes the camera from thread 1
    let mut e = env(|p| {
        Script::new(p)
            .lines()
            .stop(Stop::new(2, "step"))
            .stop(Stop::new(3, "step"))
            .stop(Stop::new(10, "step").thread(2).thread_started(2, "worker"))
            .stop(Stop::new(4, "step"))
    })
    .await?;
    e.breakpoint(10);
    e.start().await?;
    e.debug_mode().await?;
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("thread 2's breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    let record = &engine(&e.h.app).record;
    let switches: Vec<_> = record
        .marks()
        .filter_map(|m| match m.kind {
            MarkKind::Switch { from, to } => Some((m.at, from, to)),
            _ => None,
        })
        .collect();
    // FAILS (engine): the Mine path records the hit with the old spectated thread, so the switch
    // to thread 2 is only marked at its next hit, not at its breakpoint. See the report.
    assert_eq!(switches, [(0, None, 0), (2, Some(0), 1)]);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_last_hits_are_kept_within_the_cap() -> anyhow::Result<()> {
    let mut e = env(|p| lines(p, 12)).await?;
    e.h.app.editor.debug.engine.record.set_cap(Some(5));
    e.start().await?;
    e.debug_mode().await?;
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("the end", |app| {
        matches!(app.editor.debug.status, Status::Ended { .. })
    })
    .await;
    let record = &engine(&e.h.app).record;
    assert_eq!(
        record.counter(),
        12,
        "the counter doesn't forget what it dropped"
    );
    assert_eq!(record.len(), 5);
    assert_eq!(record.first_kept(), 7);
    assert_eq!(e.hit_lines(), [9, 10, 11, 12, 13]);
    assert_eq!(record.iter().next().unwrap().at, 7);
    assert!(record.get(6).is_none());
    assert!(record.get(7).is_some());
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Full speed

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn continuing_at_full_speed_lifts_the_hidden_breakpoints_and_puts_them_back(
) -> anyhow::Result<()> {
    let mut e = env(|p| lines(p, 19)).await?;
    e.breakpoint(12);
    e.start().await?;
    e.h.until("the engine ready", is_ready).await;
    assert_eq!(e.sets().len(), 1);
    assert_eq!(engine(&e.h.app).record.counter(), 1, "the first line");

    // Space G c: only your breakpoints stay while the program runs to the next one
    e.h.keys("<space>GL<esc>").await?;
    e.h.until("your breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.h.until("the engine ready again", is_ready).await;
    let sets = e.sets();
    assert_eq!(sets.len(), 3, "{sets:?}");
    assert_eq!(sets[0].len(), 30);
    assert_eq!(sets[1], [12], "lifted: yours alone");
    assert_eq!(sets[2].len(), 30, "restored");
    assert_eq!(sets[2][0], 12, "yours first");
    assert!(!sets[2][1..].contains(&12));
    let names: Vec<String> = e.fake.requests().into_iter().map(|(_, c, _)| c).collect();
    let nth = |name: &str, n: usize| {
        names
            .iter()
            .enumerate()
            .filter(|(_, c)| *c == name)
            .map(|(i, _)| i)
            .nth(n)
            .unwrap()
    };
    assert!(
        nth("setBreakpoints", 1) < nth("continue", 0),
        "lifted before it continued"
    );
    assert!(
        nth("continue", 0) < nth("setBreakpoints", 2),
        "restored after it stopped"
    );
    assert_eq!(e.motion_names(), ["continue"]);
    assert_eq!(
        e.stop_lines(),
        [2, 12],
        "the lines between weren't stopped at"
    );

    // the gap is marked where it started, and the lines it skipped didn't count
    let record = &engine(&e.h.app).record;
    assert_eq!(record.counter(), 2);
    let marks: Vec<_> = record.marks().map(|m| (m.at, m.kind.clone())).collect();
    let full: Vec<_> = marks
        .iter()
        .filter(|(_, kind)| matches!(kind, MarkKind::FullSpeed { .. }))
        .collect();
    assert_eq!(full.len(), 1, "{marks:?}");
    assert_eq!(full[0].0, 1, "where it started");
    assert_eq!(session(&e.h.app).state, SessionState::Ready);
    assert_eq!(session(&e.h.app).count(), 29);
    assert!(engine(&e.h.app).full_speed.is_none());
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// The follow mode

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changing_the_follow_mode_installs_the_set_again() -> anyhow::Result<()> {
    let mut e = env(|p| lines(p, 6)).await?;
    // lib and std themselves are work in progress and not tested here: this only starts there
    e.h.app.editor.debug.spectator.follow = Follow::Lib;
    e.start().await?;
    e.h.until("the engine ready", is_ready).await;
    assert_eq!(session(&e.h.app).follow, Follow::Lib);
    assert_eq!(e.sets().len(), 1);

    // lib -> own is one press: the set for own is sent again
    e.debug_mode().await?;
    e.h.keys("e").await?;
    assert_eq!(spectator(&e.h.app).follow, Follow::Own);
    let (fake, path) = (&e.fake, e.path.clone());
    e.h.until("the new set", |_| sets_of(fake, &path).len() >= 2)
        .await;
    e.h.until("the engine ready", is_ready).await;
    let sets = e.sets();
    assert_eq!(sets.len(), 2, "{sets:?}");
    assert_eq!(sets[1], all_lines(), "the whole set, nothing found twice");
    assert_eq!(e.fake.requests_named("breakpointLocations").len(), 0);
    assert_eq!(session(&e.h.app).follow, Follow::Own);
    assert_eq!(session(&e.h.app).state, SessionState::Ready);
    assert_eq!(session(&e.h.app).count(), 30);
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// The stepping engine, and manual steps

/// Entry stops of one thread at lines 2, 3, ...: any step reaches the next.
fn entries(p: &str, n: u64) -> Script {
    (0..n).fold(Script::new(p), |script, i| {
        script.stop(Stop::new(2 + i, "entry"))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_stepping_engine_sends_no_hidden_breakpoints_and_steps_in() -> anyhow::Result<()> {
    let mut e = env(|p| entries(p, 8)).await?;
    e.h.app.editor.debug.engine.kind = EngineKind::Stepping;
    e.breakpoint(30);
    e.launch()?;
    e.settled(1).await;
    assert!(!is_stopped(&e.h.app, LINE_REASON), "an entry, not a line");
    assert!(
        engine(&e.h.app).sessions.is_empty(),
        "no engine in the session"
    );
    // yours alone
    assert_eq!(e.sets(), [vec![30]]);

    e.debug_mode().await?;
    e.h.keys("l").await?;
    e.settled(2).await;
    e.fast().await?;
    e.h.keys("<space>").await?;
    e.h.until("the end", |app| {
        matches!(app.editor.debug.status, Status::Ended { .. })
    })
    .await;
    assert_eq!(e.motion_names().first().map(String::as_str), Some("stepIn"));
    assert!(
        e.motion_names().iter().all(|c| c == "stepIn"),
        "{:?}",
        e.motion_names()
    );
    assert_eq!(e.sets().len(), 1, "never sent again");
    assert!(engine(&e.h.app)
        .record
        .iter()
        .all(|hit| hit.why != Why::Line));
    e.h.finish().await
}

/// Frames of a stop `depth` deep whose top frame is at `line` of `p`.
fn deep(p: &str, line: u64, depth: usize) -> Stop {
    let mut stop = Stop::new(line, "step");
    for level in 0..depth {
        let (name, at) = if level == 0 {
            ("callee", line)
        } else {
            ("main", 2)
        };
        stop = stop.frame(name, p, at);
    }
    stop
}

/// A call: main at line 2, into a callee (10, 11), back in main (3), the next call, and so on.
fn calls(p: &str) -> Script {
    Script::new(p)
        .lines()
        .stop(Stop::new(2, "step").frame("main", p, 2))
        .stop(deep(p, 10, 2))
        .stop(deep(p, 11, 2))
        .stop(Stop::new(3, "step").frame("main", p, 3))
        .stop(Stop::new(4, "step").frame("main", p, 4))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_step_in_ends_at_the_next_line_wherever_it_is() -> anyhow::Result<()> {
    let mut e = env(calls).await?;
    e.start().await?;
    e.h.until("its frames", |app| {
        app.editor
            .debug_adapters
            .get_active_client()
            .is_some_and(|c| {
                c.thread_id
                    .is_some_and(|t| c.stack_frames.get(&t).is_some_and(|f| !f.is_empty()))
            })
    })
    .await;
    // Space G i
    e.h.keys("<space>Gi<esc>").await?;
    e.settled(2).await;
    assert_eq!(e.stop_lines(), [2, 10], "into the call");
    assert_eq!(
        e.motion_names(),
        ["continue"],
        "the engine continues, it doesn't step"
    );
    assert!(
        is_stopped(&e.h.app, LINE_REASON),
        "{:?}",
        e.h.app.editor.debug.status
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_step_over_ends_at_the_next_line_not_deeper() -> anyhow::Result<()> {
    let mut e = env(calls).await?;
    e.start().await?;
    e.h.until("its frames", |app| {
        app.editor
            .debug_adapters
            .get_active_client()
            .is_some_and(|c| {
                c.thread_id
                    .is_some_and(|t| c.stack_frames.get(&t).is_some_and(|f| !f.is_empty()))
            })
    })
    .await;
    // Space G n: the two lines of the call are passed over
    e.h.keys("<space>G.<esc>").await?;
    e.settled(2).await;
    e.h.until("the step ended", |app| engine(app).manual.is_none())
        .await;
    assert_eq!(e.stop_lines(), [2, 10, 11, 3]);
    assert_eq!(e.motion_names(), ["continue"; 3]);
    assert_eq!(
        spectator(&e.h.app).history.stops.len(),
        2,
        "the lines of the call were passed quietly"
    );
    assert_eq!(e.hit_lines(), [2, 10, 11, 3], "but they were counted");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_step_out_ends_at_the_next_line_above_the_call() -> anyhow::Result<()> {
    let mut e = env(calls).await?;
    e.start().await?;
    // into the call first
    e.h.until("its frames", |app| {
        app.editor
            .debug_adapters
            .get_active_client()
            .is_some_and(|c| {
                c.thread_id
                    .is_some_and(|t| c.stack_frames.get(&t).is_some_and(|f| !f.is_empty()))
            })
    })
    .await;
    e.h.keys("<space>Gi<esc>").await?;
    e.settled(2).await;
    e.h.until("its frames", |app| {
        app.editor
            .debug_adapters
            .get_active_client()
            .is_some_and(|c| {
                c.thread_id
                    .is_some_and(|t| c.stack_frames.get(&t).is_some_and(|f| f.len() == 2))
            })
    })
    .await;
    // Space G o: line 11 is as deep as line 10 was; the step ends back in main
    e.h.keys("<space>Go<esc>").await?;
    e.settled(3).await;
    assert_eq!(e.stop_lines(), [2, 10, 11, 3]);
    assert_eq!(e.motion_names(), ["continue"; 3]);
    assert_eq!(spectator(&e.h.app).history.stops.len(), 3);
    assert_eq!(StepKind::Out as u8, StepKind::Out as u8);
    e.h.finish().await
}
