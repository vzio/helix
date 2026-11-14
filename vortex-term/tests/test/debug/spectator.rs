//! Spectator mode against the scripted fake adapter: playback and its timing, the follow modes,
//! threads (freeze, solo, forced interleavings), blocked steps, time labels, the recorded
//! history and its caps, rewind, fast forward and run to cursor.
//!
//! Frames are classified by their source path: the session is launched with `cwd` set to a
//! temporary folder, so `<folder>/main.rs` is own code, a path under `.cargo/registry` is a
//! library's and one under `/rustc/` the runtime's.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use vortex_core::syntax::config::DebugAdapterConfig;
use vortex_term::{application::Application, debug, debug::panel, debug::rewind};
use vortex_view::{
    current_ref,
    debug::{spectator::Mode, Section, Status, Tree, VarNode, VarPath},
    graphics::Rect,
};

use super::{
    fake_adapter::{FakeAdapter, Frame, Scope, Script, Stop, Var},
    harness::Harness,
};
use crate::test::helpers::AppBuilder;

const REGISTRY: &str = "/home/u/.cargo/registry/src/x/lib.rs";
const SYSROOT: &str = "/rustc/abc/library/core/src/iter.rs";

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
    capabilities: Option<serde_json::Map<String, Value>>,
    dir: &Path,
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
        self.launch_with(None)
    }

    fn launch_with(
        &mut self,
        capabilities: Option<serde_json::Map<String, Value>>,
    ) -> anyhow::Result<()> {
        debug::start(
            &mut self.h.app.editor,
            Some(self.fake.addr),
            &adapter_config(capabilities, &self.dir),
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

    /// Debug mode on, the editor focused: playback, speed, thread and rewind keys are its own.
    async fn focus(&mut self) -> anyhow::Result<()> {
        if !self.h.app.editor.debug.mode_on() {
            self.h.keys("<space>G").await?;
        }
        assert!(self.h.app.editor.debug.mode_on());
        Ok(())
    }

    /// The panel focused (`Tab` in debug mode): its data keys, `jjl`, `{`, `p`...
    async fn to_panel(&mut self) -> anyhow::Result<()> {
        if !self.h.app.editor.debug.is_focused() {
            self.h.keys("<tab>").await?;
        }
        assert!(self.h.app.editor.debug.is_focused());
        Ok(())
    }

    /// The editor focused again (`Tab` from the panel in debug mode).
    async fn to_editor(&mut self) -> anyhow::Result<()> {
        if self.h.app.editor.debug.is_focused() {
            self.h.keys("<tab>").await?;
        }
        assert!(!self.h.app.editor.debug.is_focused());
        Ok(())
    }

    /// `l` (one playback step) in debug mode, and waits for the step to be recorded.
    async fn advance(&mut self, steps: usize) -> anyhow::Result<()> {
        for _ in 0..steps {
            let next = self.h.app.editor.debug.spectator.history.stops.len() + 1;
            self.h.keys("l").await?;
            self.settled(next).await;
        }
        Ok(())
    }

    /// The execution requests, in order: (command, arguments).
    fn motion(&self) -> Vec<(String, Value)> {
        self.fake
            .requests()
            .into_iter()
            .filter(|(_, c, _)| matches!(c.as_str(), "continue" | "next" | "stepIn" | "stepOut"))
            .map(|(_, c, a)| (c, a))
            .collect()
    }

    fn motion_names(&self) -> Vec<String> {
        self.motion().into_iter().map(|(c, _)| c).collect()
    }

    /// (command, thread, singleThread) of the execution requests.
    fn motion_threads(&self) -> Vec<(String, i64, bool)> {
        self.motion()
            .into_iter()
            .map(|(c, a)| {
                (
                    c,
                    a["threadId"].as_i64().unwrap_or(-1),
                    a["singleThread"].as_bool().unwrap_or(false),
                )
            })
            .collect()
    }

    /// Your breakpoint on `line` (1-based), set before the launch.
    fn breakpoint(&mut self, line: usize) {
        debug::breakpoints::toggle(&mut self.h.app.editor, self.path.clone(), line - 1);
    }

    /// The stepping engine (no hidden breakpoints): for the tests of the stepping fallback.
    fn stepping(&mut self) {
        self.h.app.editor.debug.engine.kind = vortex_view::debug::engine::EngineKind::Stepping;
    }

    fn errors_have(&self, needle: &str) -> bool {
        self.h.errors.iter().any(|e| e.contains(needle))
    }
}

fn spectator(app: &Application) -> &vortex_view::debug::spectator::Spectator {
    &app.editor.debug.spectator
}

fn texts(app: &Application, section: Section) -> Vec<String> {
    panel::rows(&app.editor, section)
        .iter()
        .map(|row| row.text())
        .collect()
}

fn is_stopped(app: &Application, reason: &str) -> bool {
    matches!(&app.editor.debug.status, Status::Stopped { reason: r, .. } if r == reason)
}

/// `n` line stops of the one thread at lines 2, 3, ... (line mode: the engine's hidden
/// breakpoints stop the program at each).
fn run(p: &str, n: u64) -> Script {
    (0..n).fold(Script::new(p).lines(), |script, i| {
        script.stop(Stop::new(2 + i, "step"))
    })
}

fn single_thread_cap(script: Script) -> Script {
    script.capability("supportsSingleThreadExecutionRequests", json!(true))
}

// ---------------------------------------------------------------------------------------------
// Playback

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn space_plays_and_pauses_in_debug_mode_and_opens_the_menu_outside_it() -> anyhow::Result<()>
{
    let mut e = env(|p| run(p, 12)).await?;
    e.launch()?;
    e.settled(1).await;

    // outside debug mode Space opens the space menu: nothing plays until `G Space`
    e.h.keys("<space><esc>").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    e.h.keys("<space>G<space>").await?;
    assert_eq!(
        spectator(&e.h.app).mode,
        Mode::Playing,
        "Space G Space plays"
    );
    e.h.until("a step", |app| spectator(app).steps >= 1).await;

    // in debug mode Space toggles; two in a row prove it never opened a menu
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Playing);
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    // and through the focused panel, which gives it to debug mode
    e.h.keys("<tab>").await?;
    assert!(e.h.app.editor.debug.is_focused());
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Playing);
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playback_at_25_lines_a_second_takes_40_ms_a_step_and_pauses_at_a_breakpoint_and_the_end(
) -> anyhow::Result<()> {
    let mut e = env(|p| {
        // the first line and 9 more (lines 2-11), then line 12 (your breakpoint) and 13
        run(p, 10)
            .stop(Stop::new(12, "step"))
            .stop(Stop::new(13, "step"))
    })
    .await?;
    e.breakpoint(12);
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.h.keys("kkkkkkkk").await?;
    assert_eq!(spectator(&e.h.app).speed_label(), "25 l/s");

    let start = Instant::now();
    e.h.keys("<space>").await?;
    e.h.until("the breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    let took = start.elapsed();
    // 10 steps: the first at once, the others 40 ms after the one before
    assert!(
        took >= Duration::from_millis(9 * 40),
        "10 steps took only {took:?}"
    );
    assert_eq!(e.fake.stops_sent().len(), 11);
    e.h.until("playback paused", |app| {
        spectator(app).mode == Mode::Paused && spectator(app).step.is_none()
    })
    .await;
    e.h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(e.fake.stops_sent().len(), 11, "no step after a breakpoint");

    // play on: one more stop, then the program ends and playback with it
    e.h.keys("<space>").await?;
    e.h.until("the end", |app| {
        matches!(app.editor.debug.status, Status::Ended { .. })
    })
    .await;
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    assert_eq!(e.fake.stops_sent().len(), 12);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn l_makes_exactly_one_playback_step() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 6)).await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.h.keys("l").await?;
    e.settled(2).await;
    e.h.pump_for(Duration::from_millis(400)).await;
    // on the engine a playback step is a continue to the next line
    assert_eq!(e.motion_names(), ["continue"]);
    assert_eq!(e.fake.stops_sent().len(), 2);
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Follow modes

/// Frames: own code, a library and the runtime. From the entry in own code, one step goes into
/// the library, then the runtime, then lands back in own code.
async fn follow_case(presses: usize, steps: usize) -> anyhow::Result<Vec<String>> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(2, "entry"))
            .stop(
                Stop::new(3, "entry")
                    .frame("helper", REGISTRY, 3)
                    .frame("main", p, 2),
            )
            .stop(
                Stop::new(3, "entry")
                    .frame("next", SYSROOT, 10)
                    .frame("helper", REGISTRY, 3)
                    .frame("main", p, 2),
            )
            .stop(Stop::new(3, "entry"))
            .stop(Stop::new(4, "entry"))
    })
    .await?;
    // the follow modes' stepping fallback: libraries and the runtime have no hidden breakpoints
    e.stepping();
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    for _ in 0..presses {
        e.h.keys("e").await?;
    }
    e.advance(steps).await?;
    let names = e.motion_names();
    e.h.finish().await?;
    Ok(names)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn own_steps_out_of_libraries_and_the_runtime_until_own_code() -> anyhow::Result<()> {
    // own is the default: stepIn lands in the registry, stepOut in the sysroot, stepOut in main
    assert_eq!(follow_case(0, 1).await?, ["stepIn", "stepOut", "stepOut"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lib_steps_out_of_the_runtime_only() -> anyhow::Result<()> {
    // e: own -> std -> lib
    assert_eq!(
        follow_case(2, 2).await?,
        ["stepIn", "stepIn", "stepOut"],
        "the registry is followed, the sysroot left"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn std_never_steps_out() -> anyhow::Result<()> {
    assert_eq!(follow_case(1, 4).await?, ["stepIn"; 4]);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Threads

/// Two threads with a stop queue each; the adapter can run one thread alone when `cap`.
fn two_threads(p: &str, cap: bool, a: Vec<Stop>, b: Vec<Stop>) -> Script {
    let script = Script::new(p)
        .lines()
        .thread_program(1, a)
        .thread_program(2, b);
    if cap {
        single_thread_cap(script)
    } else {
        script
    }
}

fn queue(lines: &[u64]) -> Vec<Stop> {
    lines.iter().map(|l| Stop::new(*l, "step")).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frozen_thread_never_runs_and_its_neighbour_steps_alone() -> anyhow::Result<()> {
    let mut e = env(|p| two_threads(p, true, queue(&[2, 3, 4, 5]), queue(&[10, 11, 12]))).await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    assert_eq!(e.fake.advanced(2), 1, "thread 2 is at its first stop");

    // spectate 2, freeze it, back to 1
    e.h.keys("<gt>z<lt>").await?;
    let thread2 = |app: &Application| {
        spectator(app)
            .frozen
            .iter()
            .any(|k| k.thread.to_string() == "2")
    };
    assert!(thread2(&e.h.app));
    assert!(texts(&e.h.app, Section::Stack)
        .iter()
        .any(|t| t.starts_with("2 worker") && t.contains("❄ frozen")));

    e.advance(1).await?;
    assert_eq!(
        e.motion_threads(),
        [("continue".to_string(), 1, true)],
        "singleThread because a neighbour is frozen"
    );
    assert_eq!(e.fake.advanced(1), 2);
    assert_eq!(e.fake.advanced(2), 1, "the frozen thread didn't advance");

    // the frozen thread can't be stepped or continued
    e.h.keys("<gt>").await?;
    e.h.keys("l").await?;
    e.h.keys("<tab>p<tab>").await?; // the panel's `p` pauses or continues the thread
    e.h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(
        e.motion().len(),
        1,
        "nothing was sent for the frozen thread"
    );
    assert_eq!(e.fake.advanced(2), 1);

    // solo: the spectated thread runs alone although nothing is frozen
    e.h.keys("z<lt>a").await?; // thaw 2, spectate 1, solo
    assert!(!thread2(&e.h.app));
    assert!(spectator(&e.h.app).solo);
    e.advance(1).await?;
    assert_eq!(e.motion_threads()[1], ("continue".to_string(), 1, true));
    assert!(texts(&e.h.app, Section::Stack)
        .iter()
        .any(|t| t.starts_with("1 main") && t.contains("alone")));

    // solo off and nothing frozen: no singleThread
    e.h.keys("a").await?;
    assert!(!spectator(&e.h.app).solo);
    e.advance(1).await?;
    assert_eq!(e.motion_threads()[2], ("continue".to_string(), 1, false));
    assert!(e.motion()[2].1.get("singleThread").is_none());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_single_thread_support_freeze_and_solo_are_refused() -> anyhow::Result<()> {
    let mut e = env(|p| two_threads(p, false, queue(&[2, 3, 4]), queue(&[10, 11]))).await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.h.keys("<gt>z").await?;
    assert!(e.errors_have("no singleThread support"), "{:?}", e.h.errors);
    assert!(spectator(&e.h.app).frozen.is_empty());
    e.h.errors.clear();
    e.h.keys("a").await?;
    assert!(e.errors_have("no singleThread support"), "{:?}", e.h.errors);
    assert!(!spectator(&e.h.app).solo);
    e.h.keys("<lt>").await?;
    e.advance(1).await?;
    assert!(e
        .motion()
        .iter()
        .all(|(_, a)| a.get("singleThread").is_none()));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_configured_capability_makes_freeze_work_and_steps_carry_single_thread(
) -> anyhow::Result<()> {
    // the fake doesn't advertise supportsSingleThreadExecutionRequests: the config assumes it
    let mut e = env(|p| two_threads(p, false, queue(&[2, 3, 4]), queue(&[10, 11]))).await?;
    let caps = json!({"supportsSingleThreadExecutionRequests": true});
    e.launch_with(caps.as_object().cloned())?;
    e.settled(1).await;
    e.focus().await?;
    e.h.keys("<gt>z<lt>").await?;
    assert!(!e.errors_have("singleThread"), "{:?}", e.h.errors);
    assert_eq!(spectator(&e.h.app).frozen.len(), 1);
    e.advance(1).await?;
    assert_eq!(e.motion_threads(), [("continue".to_string(), 1, true)]);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forced_interleaving_gives_the_stops_in_the_order_it_was_driven() -> anyhow::Result<()> {
    // A: ... check (3) ... ; B: 11, 12 while A waits at the check; then A goes on
    let mut e = env(|p| two_threads(p, true, queue(&[2, 3, 4, 5]), queue(&[10, 11, 12]))).await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;

    // freeze B, step A to the check
    e.h.keys("<gt>z<lt>").await?;
    e.advance(1).await?;
    // thaw B, freeze A, run B through
    e.h.keys("<gt>z<lt>z<gt>").await?;
    e.advance(2).await?;
    // thaw A and continue it
    e.h.keys("<lt>z<tab>p<tab>").await?;
    e.h.until("A's last stop", |_| e.fake.stops_sent().len() >= 6)
        .await;
    e.settled(5).await;

    let order: Vec<(i64, u64)> = e.fake.stops_sent().iter().map(|s| (s.0, s.1)).collect();
    assert_eq!(order, [(1, 2), (2, 10), (1, 3), (2, 11), (2, 12), (1, 4)]);
    assert_eq!(
        e.motion_threads(),
        [
            ("continue".to_string(), 1, true),
            ("continue".to_string(), 2, true),
            ("continue".to_string(), 2, true),
            ("continue".to_string(), 1, false),
        ]
    );
    assert!(spectator(&e.h.app).frozen.is_empty());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn threads_joining_and_leaving_are_announced_and_can_be_spectated() -> anyhow::Result<()> {
    let mut e = env(|p| {
        single_thread_cap(
            Script::new(p)
                .stop(Stop::new(2, "entry"))
                .stop(Stop::new(3, "entry").thread_started(2, "worker"))
                .stop(Stop::new(4, "entry").thread_exited(2)),
        )
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.advance(1).await?;
    e.h.until("two threads", |app| {
        app.editor.debug.all_threads().len() == 2
    })
    .await;
    let names: Vec<_> =
        e.h.app
            .editor
            .debug
            .all_threads()
            .iter()
            .map(|(_, t)| t.name.clone())
            .collect();
    assert_eq!(names, ["main", "worker"]);

    // > and < cycle the spectated thread, which the call stack marks
    let spectated = |app: &Application| {
        spectator(app)
            .spectated
            .map(|k| k.thread.to_string())
            .unwrap_or_default()
    };
    assert_eq!(spectated(&e.h.app), "1");
    e.h.keys("<gt>").await?;
    assert_eq!(spectated(&e.h.app), "2");
    let rows = texts(&e.h.app, Section::Stack);
    assert!(
        rows.iter()
            .any(|t| t.starts_with("2 worker") && t.contains("◉ spectating")),
        "{rows:?}"
    );
    assert!(!rows
        .iter()
        .any(|t| t.starts_with("1 main") && t.contains("◉")));
    e.h.keys("<gt>").await?;
    assert_eq!(spectated(&e.h.app), "1", "wraps around");
    e.h.keys("<lt>").await?;
    assert_eq!(spectated(&e.h.app), "2", "and back");

    // a frozen thread shows it
    e.h.keys("z").await?;
    assert!(texts(&e.h.app, Section::Stack)
        .iter()
        .any(|t| t.starts_with("2 worker") && t.contains("❄ frozen")));
    e.h.keys("<lt>").await?;

    // it leaves: gone from the list and from the frozen threads
    e.advance(1).await?;
    e.h.until("one thread", |app| {
        app.editor.debug.all_threads().len() == 1
    })
    .await;
    assert!(spectator(&e.h.app).frozen.is_empty());
    assert_eq!(spectated(&e.h.app), "1");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thread_joining_is_listed_and_the_status_line_says_nothing() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(2, "entry"))
            .stop(Stop::new(3, "entry").thread_started(2, "worker"))
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.h.keys("l").await?;
    e.h.until("two threads", |app| {
        app.editor.debug.all_threads().len() == 2
    })
    .await;
    // no notice, and none with keys: the panel lists the thread
    assert!(
        e.h.app
            .editor
            .get_status()
            .is_none_or(|(m, _)| !m.contains("joined") && !m.contains("to spectate")),
        "{:?}",
        e.h.app.editor.get_status()
    );
    let note = e.h.app.editor.debug.mode.as_ref().unwrap().note.clone();
    assert!(
        note.as_ref()
            .is_none_or(|n| !n.contains("joined") && !n.contains("to spectate")),
        "{note:?}"
    );
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Blocked steps and time labels

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocking_step_shows_blocked_and_space_pauses_the_thread() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(2, "entry"))
            .stop(Stop::new(3, "entry").delay(1500))
            .stop(Stop::new(4, "entry"))
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.h.keys("<space>").await?;
    e.h.pump_for(Duration::from_millis(700)).await;
    let label = spectator(&e.h.app)
        .label(Instant::now())
        .unwrap_or_default();
    assert!(label.starts_with("blocked in main"), "{label:?}");

    // Space: the thread is paused where it waits
    e.h.keys("<space>").await?;
    e.fake
        .wait_for_request("pause", Duration::from_secs(2))
        .await?;
    e.h.until("the pause stop", |app| is_stopped(app, "pause"))
        .await;
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    assert!(spectator(&e.h.app).step.is_none());
    assert_eq!(e.fake.requests_named("pause").len(), 1);
    assert_eq!(e.fake.requests_named("pause")[0].1["threadId"], 1);
    e.h.pump_for(Duration::from_millis(300)).await;
    assert_eq!(e.fake.stops_sent().len(), 2, "nothing went on after it");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_step_labels_the_line_it_left_and_capital_x_clears_the_labels() -> anyhow::Result<()>
{
    // lines 2..: 8 quick steps, then the step off line 10 takes 250 ms
    let mut e = env(|p| {
        let script = run(p, 9);
        // a long tail, so the program is still there when playback is paused
        (12..80).fold(script.stop(Stop::new(11, "step").delay(250)), |s, l| {
            s.stop(Stop::new(l, "step"))
        })
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    // noise from the test machine is not a slow line
    e.h.app.editor.debug.spectator.timing.threshold = Duration::from_millis(15);
    e.focus().await?;
    e.h.keys("kkkkkkkk").await?;
    e.h.keys("<space>").await?;
    let path = e.path.clone();
    e.h.until("the label", |app| {
        spectator(app).timing.label(&path, 9).is_some()
    })
    .await;
    e.h.keys("<space>").await?;

    let timing = &spectator(&e.h.app).timing;
    let slow = timing.file(&path).unwrap()[&9];
    assert!(slow.total >= Duration::from_millis(120), "{slow:?}");
    assert!(slow.total < Duration::from_millis(1000), "{slow:?}");
    assert_eq!(
        timing
            .file(&path)
            .unwrap()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [9],
        "the cheap lines have no label"
    );
    assert!(timing.label(&path, 4).is_none());

    e.h.until("the step in flight ends", |app| {
        spectator(app).step.is_none()
    })
    .await;
    e.h.keys("X").await?;
    assert!(spectator(&e.h.app).timing.lines.is_empty());
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// History

fn deep_scope() -> Scope {
    Scope::new("Locals")
        .var(Var::new("x", "1"))
        .var(Var::new("v1", "V").child(
            Var::new("v2", "V").child(
                Var::new("v3", "V").child(Var::new("v4", "V").child(Var::new("v5", "leaf"))),
            ),
        ))
        .var(Var::new("big", "Vec").many(250).indexed())
}

fn deep_stop(p: &str, line: u64) -> Stop {
    Stop::new(line, "entry")
        .frame_with(Frame::new("main", p, line).scope(deep_scope()))
        .frame_with(Frame::new("caller", p, 20).scope(Scope::new("Locals").var(Var::new("y", "7"))))
}

fn child<'a>(node: &'a VarNode, name: &str) -> &'a VarNode {
    node.children
        .as_ref()
        .unwrap_or_else(|| panic!("{} has no children loaded", node.name))
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no child {name} in {}", node.name))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_stop_is_recorded_whole_within_the_caps() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(deep_stop(p, 2))
            .stop(deep_stop(p, 3))
            .stop(deep_stop(p, 4))
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.advance(2).await?;
    let history = &spectator(&e.h.app).history;
    assert_eq!(history.stops.len(), 3, "every shown stop");
    assert_eq!(history.last_step, 3);
    let snapshot = history.stops.back().unwrap();
    assert_eq!(snapshot.frames.len(), 2);
    assert_eq!(snapshot.scopes.len(), 2, "the variables of every frame");
    let main = snapshot.scopes[0].as_ref().unwrap();
    let caller = snapshot.scopes[1].as_ref().unwrap();
    assert_eq!(child(&caller[0], "y").value, "7");

    // depth 3: v1, v2, v3 are loaded, v4 is not
    let v3 = child(child(child(&main[0], "v1"), "v2"), "v3");
    assert!(v3.reference > 0);
    assert!(v3.children.is_none(), "level 4 is not loaded");
    assert!(snapshot.truncated);

    // collection cap 100 of 250, and a last row that says so
    let big = child(&main[0], "big");
    let kids = big.children.as_ref().unwrap();
    assert_eq!(kids.len(), 101);
    assert_eq!(kids[99].name, "[99]");
    assert_eq!(kids[100].name, "…");
    assert_eq!(kids[100].value, "150 more of 250 not recorded");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_capture_settings_change_the_caps_and_a_smaller_history_trims() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 6)).await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.advance(4).await?;
    assert_eq!(spectator(&e.h.app).history.stops.len(), 5);

    e.h.keys("c").await?;
    assert_eq!(spectator(&e.h.app).settings, Some(0));
    e.h.keys("l").await?; // depth 3 -> 4
    assert_eq!(spectator(&e.h.app).caps.depth, Some(4));
    e.h.keys("jh").await?; // collection 100 -> 50
    assert_eq!(spectator(&e.h.app).settings, Some(1));
    assert_eq!(spectator(&e.h.app).caps.collection, Some(50));
    e.h.keys("jhhh").await?; // history 2000 -> 1000 -> 500 -> 100
    assert_eq!(spectator(&e.h.app).caps.history, Some(100));
    assert_eq!(
        spectator(&e.h.app).history.stops.len(),
        5,
        "not trimmed yet"
    );
    e.h.keys("h").await?; // -> off
    assert_eq!(spectator(&e.h.app).caps.history, Some(0));
    assert_eq!(
        spectator(&e.h.app).history.stops.len(),
        0,
        "trimmed at once"
    );
    e.h.keys("<esc>").await?;
    assert_eq!(spectator(&e.h.app).settings, None);
    assert!(e.h.app.editor.debug.is_focused());
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Rewind

fn point_scope() -> Scope {
    Scope::new("Locals").var(Var::new("x", "1")).var(
        Var::new("p", "Point")
            .child(Var::new("a", "5"))
            .child(Var::new("inner", "Inner").child(Var::new("z", "9"))),
    )
}

fn marker(app: &Application) -> Option<String> {
    spectator(app).history.marker()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_value_never_expanded_live_shows_in_rewind_without_a_request() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(2, "entry").scope(point_scope()))
            .stop(Stop::new(3, "entry").scope(point_scope()))
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.advance(1).await?;
    e.h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(
        texts(&e.h.app, Section::Variables),
        ["Locals", "x = 1", "p = Point"],
        "live: p was never expanded"
    );
    let before = e.fake.requests_named("variables").len();

    e.h.keys("h").await?;
    assert!(rewind_active(&e.h.app));
    e.to_panel().await?;
    assert_eq!(
        texts(&e.h.app, Section::Variables),
        ["Locals", "x = 1", "p = Point"]
    );
    e.h.keys("jjl").await?;
    assert_eq!(
        texts(&e.h.app, Section::Variables),
        ["Locals", "x = 1", "p = Point", "a = 5", "inner = Inner"]
    );
    e.h.keys("jjl").await?;
    assert!(texts(&e.h.app, Section::Variables).contains(&"z = 9".to_string()));
    e.h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(
        e.fake.requests_named("variables").len(),
        before,
        "recorded values need no request"
    );
    e.h.finish().await
}

fn rewind_active(app: &Application) -> bool {
    app.editor.debug.is_rewinding()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expanding_beyond_the_capture_depth_in_rewind_sends_nothing() -> anyhow::Result<()> {
    let mut e = env(|p| Script::new(p).stop(deep_stop(p, 2)).stop(deep_stop(p, 3))).await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.advance(1).await?;
    e.h.pump_for(Duration::from_millis(200)).await;
    let before = e.fake.requests_named("variables").len();

    e.h.keys("h").await?;
    e.to_panel().await?;
    e.h.keys("jjl").await?; // v1
    e.h.keys("jl").await?; // v2
    e.h.keys("jl").await?; // v3: its children weren't recorded
    e.h.pump_for(Duration::from_millis(300)).await;
    assert!(
        texts(&e.h.app, Section::Variables).contains(&"not recorded (capture depth)".to_string()),
        "{:?}",
        texts(&e.h.app, Section::Variables)
    );
    assert_eq!(
        e.fake.requests_named("variables").len(),
        before,
        "nothing is asked of a program that isn't executing this stop"
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scrubbing_shows_the_marker_and_refuses_edits_until_live() -> anyhow::Result<()> {
    let mut e = env(|p| {
        (0..6).fold(Script::new(p).set_variable(), |s, i| {
            s.stop(Stop::new(2 + i, "entry").scope(point_scope()))
        })
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.advance(5).await?;
    assert_eq!(spectator(&e.h.app).history.stops.len(), 6);
    assert_eq!(marker(&e.h.app), None);

    let marker_is = |app: &Application, step: usize| {
        marker(app).as_deref() == Some(&format!("REWIND · step {step}/6 · nothing is executing"))
    };
    e.h.keys("h").await?;
    assert!(marker_is(&e.h.app, 5), "{:?}", marker(&e.h.app));
    e.h.keys("h").await?;
    assert!(marker_is(&e.h.app, 4));
    e.h.keys("l").await?;
    assert!(marker_is(&e.h.app, 5));
    // the focused panel rewinds by ten
    e.to_panel().await?;
    e.h.keys("{").await?;
    e.to_editor().await?;
    assert!(marker_is(&e.h.app, 1), "10 back stops at the first");
    e.h.keys("ll").await?;
    assert!(marker_is(&e.h.app, 3));
    // the panel's tab bar shows it
    let bar = render_panel_top(&mut e.h.app);
    assert!(bar.contains("REWIND · step 3/6"), "{bar:?}");
    assert!(texts(&e.h.app, Section::Stack)[0].contains("(recorded)"));

    // nothing is executed in rewind
    e.h.errors.clear();
    e.to_panel().await?;
    e.h.keys("c").await?;
    assert_eq!(
        e.h.errors.last().map(String::as_str),
        Some(rewind::READ_ONLY)
    );
    e.h.errors.clear();
    e.h.keys("i").await?;
    assert_eq!(
        e.h.errors.last().map(String::as_str),
        Some(rewind::READ_ONLY)
    );
    let path = VarPath {
        tree: Tree::Scopes,
        names: vec!["Locals".into(), "x".into()],
    };
    let err = debug::load::set_value(&mut e.h.app.editor, path, "42".into()).unwrap_err();
    assert_eq!(err.to_string(), rewind::READ_ONLY);
    assert!(e.fake.requests_named("setVariable").is_empty());

    // } past the end, and p, go back to live
    e.h.keys("}").await?;
    assert_eq!(marker(&e.h.app), None, "}} goes forward to live");
    e.to_editor().await?;
    e.h.keys("hh").await?;
    assert!(rewind_active(&e.h.app));
    e.h.keys("p").await?;
    assert_eq!(marker(&e.h.app), None);
    assert!(!render_panel_top(&mut e.h.app).contains("REWIND"));

    // Space plays, which also returns to live
    e.h.keys("h").await?;
    assert!(rewind_active(&e.h.app));
    e.h.keys("<space>").await?;
    assert_eq!(marker(&e.h.app), None);
    e.h.keys("<space>").await?;
    e.h.finish().await
}

/// The first row of the panel drawn in 120 x 12 cells.
fn render_panel_top(app: &mut Application) -> String {
    let area = Rect::new(0, 0, 120, 12);
    let mut surface = tui::buffer::Buffer::empty(area);
    debug::render(&mut app.editor, area, &mut surface);
    (0..area.width)
        .map(|x| {
            surface
                .get(x, 0)
                .map(|c| c.symbol.to_string())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .concat()
}

// ---------------------------------------------------------------------------------------------
// Fast forward

fn x_scope(x: &str) -> Scope {
    Scope::new("Locals").var(Var::new("x", x))
}

/// A watch on `x`, with its value shown.
async fn watch_x(e: &mut Env, value: &str) -> anyhow::Result<()> {
    e.to_editor().await?;
    // the prompt opens when the callback ran: the text goes in a call of its own
    e.h.keys("w").await?;
    e.h.keys("x<ret>").await?;
    let prefix = format!("x = {value}");
    e.h.until("the watch", |app| {
        texts(app, Section::Watch)
            .iter()
            .any(|t| t.starts_with(&prefix))
    })
    .await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_forward_stops_on_the_first_change_of_a_watched_value() -> anyhow::Result<()> {
    let mut e = env(|p| {
        ["1", "1", "1", "1", "2", "2", "2"]
            .iter()
            .enumerate()
            .fold(Script::new(p).lines(), |s, (i, x)| {
                s.stop(Stop::new(2 + i as u64, "step").scope(x_scope(x)))
            })
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    watch_x(&mut e, "1").await?;
    e.h.keys("f").await?;
    e.h.until("the change", |app| !spectator(app).changes.is_empty())
        .await;
    e.h.until("paused", |app| spectator(app).mode == Mode::Paused)
        .await;
    let changes = &spectator(&e.h.app).changes;
    assert_eq!(changes.len(), 1);
    assert_eq!(
        (
            changes[0].name.as_str(),
            changes[0].old.as_str(),
            changes[0].new.as_str()
        ),
        ("watch: x", "1", "2")
    );
    let text =
        e.h.app
            .editor
            .debug
            .console
            .lines
            .iter()
            .map(|l| l.text.clone())
            .find(|t| t.contains("Fast forward stopped"))
            .expect("a console line");
    assert!(text.contains("1 -> 2"), "{text}");
    // debug mode is on: the note says it, not the status line
    let note = e.h.app.editor.debug.mode.as_ref().unwrap().note.clone();
    assert!(note.is_some_and(|note| note.contains("1 -> 2")));
    // 4 steps to the change (three without one), no more
    assert_eq!(e.fake.stops_sent().len(), 5);
    e.h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(e.fake.stops_sent().len(), 5);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_forward_stops_on_a_breakpoint_and_f_or_space_cancels_it() -> anyhow::Result<()> {
    let mut e = env(|p| {
        let script = (0..3).fold(Script::new(p).lines(), |s, i| {
            s.stop(Stop::new(2 + i, "step").scope(x_scope("1")))
        });
        script
            .stop(Stop::new(5, "step").scope(x_scope("1")))
            .stop(Stop::new(6, "step").scope(x_scope("1")).delay(500))
            .stop(Stop::new(7, "step").scope(x_scope("1")).delay(500))
            .stop(Stop::new(8, "step").scope(x_scope("1")))
    })
    .await?;
    // line 5 is your breakpoint
    e.breakpoint(5);
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    watch_x(&mut e, "1").await?;
    e.h.keys("f").await?;
    e.h.until("the breakpoint", |app| is_stopped(app, "breakpoint"))
        .await;
    e.h.until("paused", |app| spectator(app).mode == Mode::Paused)
        .await;
    assert!(spectator(&e.h.app).changes.is_empty());
    assert_eq!(e.fake.stops_sent().len(), 4);

    // into the slow steps; f cancels
    e.h.keys("f").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::FastForward);
    e.h.pump_for(Duration::from_millis(150)).await;
    e.h.keys("f").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    e.h.until("the step in flight ends", |app| {
        spectator(app).step.is_none() && matches!(app.editor.debug.status, Status::Stopped { .. })
    })
    .await;
    let count = e.fake.stops_sent().len();
    e.h.pump_for(Duration::from_millis(700)).await;
    assert_eq!(e.fake.stops_sent().len(), count, "it didn't go on");

    // Space cancels too
    e.h.keys("f").await?;
    e.h.pump_for(Duration::from_millis(150)).await;
    e.h.keys("<space>").await?;
    assert_eq!(spectator(&e.h.app).mode, Mode::Paused);
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Run to cursor

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_to_cursor_uses_a_temporary_breakpoint_that_goes_away() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(2, "entry"))
            .stop(Stop::new(5, "breakpoint"))
            .stop(Stop::new(8, "breakpoint"))
            .stop(Stop::new(12, "breakpoint"))
    })
    .await?;
    // the temporary breakpoint protocol of the stepping engine (the engine's hidden breakpoints
    // cover the lines already)
    e.stepping();
    e.launch()?;
    e.settled(1).await;
    // the cursor is on line 2 (the stop); debug mode, view mode, to line 8, run to the cursor
    e.h.keys("<space>Gv6jr<esc>").await?;
    e.h.until("the stop at line 8", |_| {
        e.fake.stops_sent().iter().any(|s| s.1 == 8)
    })
    .await;
    e.settled(2).await;
    e.h.until("the temporary breakpoint is gone", |_| {
        e.fake.requests_named("setBreakpoints").len() >= 2
    })
    .await;

    let sets = e.fake.requests_named("setBreakpoints");
    assert_eq!(
        sets[0].1["source"]["path"],
        e.path.to_string_lossy().as_ref()
    );
    assert_eq!(sets[0].1["breakpoints"], json!([{"line": 8}]));
    assert_eq!(sets[1].1["breakpoints"], json!([]), "removed afterwards");
    let names: Vec<String> = e.fake.requests().into_iter().map(|(_, c, _)| c).collect();
    let at = |name: &str| names.iter().position(|c| c == name).unwrap();
    assert!(at("setBreakpoints") < at("continue"));
    assert_eq!(e.motion_names(), ["continue"]);
    assert_eq!(
        e.fake.stops_sent().last().map(|s| s.1),
        Some(8),
        "line 5 was passed"
    );
    assert!(spectator(&e.h.app).run_to.is_none());
    assert!(e.h.app.editor.breakpoints.get(&e.path).is_none());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_to_cursor_while_playing_plays_until_that_line() -> anyhow::Result<()> {
    let mut e = env(|p| run(p, 10)).await?;
    e.launch()?;
    e.settled(1).await;
    e.h.app.editor.debug.spectator.speed = 7;
    // line 2 -> line 6; play, and run to the cursor in the same keys
    e.h.keys("<space>Gv4j<space>r<esc>").await?;
    e.h.until("the line", |app| {
        spectator(app).mode == Mode::Paused && spectator(app).run_to.is_none()
    })
    .await;
    e.settled(5).await;
    e.h.pump_for(Duration::from_millis(300)).await;
    let stops = e.fake.stops_sent();
    assert_eq!(stops.last().map(|s| s.1), Some(6));
    assert_eq!(stops.len(), 5, "no step after it");
    // the engine's one install, nothing else; a continue per line
    assert_eq!(e.fake.requests_named("setBreakpoints").len(), 1);
    assert_eq!(e.motion_names(), ["continue"; 4]);
    e.h.finish().await
}
