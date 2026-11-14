//! What rewind records, end to end against the scripted fake adapter: the recording diet (frames
//! that can't have changed since the last stop lend their variables instead of being fetched
//! again, the `carried from step N` row, bounded pipelining) and the staged install of the hidden
//! breakpoints (the files likely to run first go in before the program is let go).
//!
//! The stacks are scripted per stop with [`stack_stop`]: every frame has one scope `Locals` with
//! one variable `at` whose value is `<frame name>#<stop tag>`, so a test can tell which stop a
//! snapshot's variables of a frame came from.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;
use vortex_core::syntax::config::DebugAdapterConfig;
use vortex_term::{
    application::Application, debug, debug::panel, debug::record::IN_FLIGHT, debug::rewind,
};
use vortex_view::{
    current_ref,
    debug::{
        engine::{EngineKind, SessionState},
        lineline::MarkKind,
        spectator::Mode,
        Section, Status,
    },
};

use super::{
    fake_adapter::{FakeAdapter, Frame, Scope, Script, Stop, Var},
    harness::Harness,
};
use crate::test::helpers::AppBuilder;

// ---------------------------------------------------------------------------------------------
// The environment

struct Env {
    h: Harness,
    fake: FakeAdapter,
    /// The file open in the editor, as the editor knows it.
    path: PathBuf,
    /// The folder of the session's own code (the hidden breakpoints go under it only).
    own: PathBuf,
    /// The working directory of the launch.
    dir: PathBuf,
    _tmp: TempDir,
}

/// `files`: (path relative to the temporary folder, number of lines). `open` is the one the
/// editor has open; `own` is the folder (relative) the engine covers. `script` gets the absolute
/// path of every file, by the order of `files`.
async fn env_with(
    files: &[(&str, usize)],
    open: &str,
    own: &str,
    script: impl FnOnce(&[String]) -> Script,
) -> anyhow::Result<Env> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().canonicalize()?;
    let mut paths = Vec::new();
    for (name, lines) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let source: String = (1..=*lines).map(|i| format!("    step({i});\n")).collect();
        std::fs::write(&path, source)?;
        paths.push(path.to_string_lossy().to_string());
    }
    let h = Harness::new(AppBuilder::new().with_file(&dir.join(open), None))?;
    let path = {
        let (_, doc) = current_ref!(h.app.editor);
        doc.path().expect("the file has a path").to_owned()
    };
    let paths = files
        .iter()
        .zip(paths)
        .map(|((name, _), p)| {
            if *name == open {
                path.to_string_lossy().to_string()
            } else {
                p
            }
        })
        .collect::<Vec<_>>();
    let fake = FakeAdapter::start(script(&paths)).await?;
    Ok(Env {
        h,
        fake,
        path,
        own: dir.join(own),
        dir,
        _tmp: tmp,
    })
}

/// One file `main.rs` of 30 lines.
async fn env(script: impl FnOnce(&str) -> Script) -> anyhow::Result<Env> {
    env_with(&[("main.rs", 30)], "main.rs", ".", |p| script(&p[0])).await
}

fn adapter_config(own: &Path) -> DebugAdapterConfig {
    DebugAdapterConfig {
        name: "fake".into(),
        transport: "tcp".into(),
        command: String::new(),
        args: Vec::new(),
        port_arg: None,
        templates: Vec::new(),
        quirks: Default::default(),
        own_paths: Some(vec![own.to_owned()]),
        std_paths: None,
        lib_paths: None,
        capabilities: None,
    }
}

impl Env {
    fn launch_with(&mut self, program: Value) -> anyhow::Result<()> {
        debug::start(
            &mut self.h.app.editor,
            Some(self.fake.addr),
            &adapter_config(&self.own),
            "fake",
            "launch",
            json!({"program": program, "cwd": self.dir}),
        )
    }

    fn launch(&mut self) -> anyhow::Result<()> {
        self.launch_with(json!(self.dir.join("prog")))
    }

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

    /// Debug mode on, the editor focused: playback and rewind keys are its own.
    async fn focus(&mut self) -> anyhow::Result<()> {
        self.h.keys("<space>G").await?;
        assert!(self.h.app.editor.debug.mode_on());
        Ok(())
    }

    /// `l` (one playback step) in debug mode, `steps` times, waiting for each step to be recorded.
    async fn advance(&mut self, steps: usize) -> anyhow::Result<()> {
        for _ in 0..steps {
            let next = self.h.app.editor.debug.spectator.history.stops.len() + 1;
            self.h.keys("l").await?;
            self.settled(next).await;
        }
        Ok(())
    }

    /// Launches on the engine, waits for the install to be over and the first line.
    async fn start_engine(&mut self) -> anyhow::Result<()> {
        self.launch()?;
        self.settled(1).await;
        self.h.until("the engine ready", is_ready).await;
        self.focus().await
    }
}

fn is_ready(app: &Application) -> bool {
    let engine = &app.editor.debug.engine;
    engine
        .sessions
        .values()
        .all(|s| s.state == SessionState::Ready)
        && engine.task.is_none()
}

fn texts(app: &Application, section: Section) -> Vec<String> {
    panel::rows(&app.editor, section)
        .iter()
        .map(|row| row.text())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Scripted stacks and what they leave in the history

/// A stop whose stack is `frames` (name, line), the top first, all in the file `p`; the stop's
/// line is the top frame's. Every frame's `Locals` has `at = <name>#<tag>`.
fn stack_stop(p: &str, tag: u32, frames: &[(&str, u64)]) -> Stop {
    let mut stop = Stop::new(frames[0].1, "step");
    for (name, line) in frames {
        stop = stop.frame_with(
            Frame::new(*name, p, *line)
                .scope(Scope::new("Locals").var(Var::new("at", format!("{name}#{tag}")))),
        );
    }
    stop
}

fn line_script(p: &str, stops: Vec<Stop>) -> Script {
    stops
        .into_iter()
        .fold(Script::new(p).lines(), |script, stop| script.stop(stop))
}

/// `carried` of every recorded stop.
fn carried(app: &Application) -> Vec<Vec<Option<usize>>> {
    app.editor
        .debug
        .spectator
        .history
        .stops
        .iter()
        .map(|s| s.carried.clone())
        .collect()
}

/// The requests (`scopes` + `variables`) each recorded stop's capture made.
fn requests(app: &Application) -> Vec<usize> {
    app.editor
        .debug
        .spectator
        .history
        .stops
        .iter()
        .map(|s| s.requests)
        .collect()
}

/// The `at` value of every frame of recorded stop `index`.
fn values(app: &Application, index: usize) -> Vec<String> {
    let snapshot = &app.editor.debug.spectator.history.stops[index];
    snapshot
        .scopes
        .iter()
        .map(|scope| {
            let scope = scope.as_ref().expect("the frame was recorded");
            let children = scope[0].children.as_ref().expect("Locals has children");
            children
                .iter()
                .find(|c| c.name == "at")
                .expect("the variable at")
                .value
                .clone()
        })
        .collect()
}

const N: Option<usize> = None;
const fn s(step: usize) -> Option<usize> {
    Some(step)
}

// ---------------------------------------------------------------------------------------------
// Frame reuse

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playing_line_by_line_fetches_only_the_top_frame_after_the_first_stop() -> anyhow::Result<()>
{
    let mut e = env(|p| {
        line_script(
            p,
            (0..4)
                .map(|i| {
                    stack_stop(
                        p,
                        i + 1,
                        &[("leaf", 2 + i as u64), ("mid", 10), ("main", 20)],
                    )
                })
                .collect(),
        )
    })
    .await?;
    e.start_engine().await?;
    e.advance(3).await?;

    assert_eq!(
        carried(&e.h.app),
        [
            vec![N, N, N],
            vec![N, s(1), s(1)],
            vec![N, s(1), s(1)],
            vec![N, s(1), s(1)],
        ],
        "the callers keep the stop that fetched them"
    );
    // a scope and its variables per fetched frame: 3 frames, then the top one only
    assert_eq!(requests(&e.h.app), [6, 2, 2, 2]);
    // the callers hold what the first stop recorded, the top frame its own stop's
    assert_eq!(values(&e.h.app, 0), ["leaf#1", "mid#1", "main#1"]);
    assert_eq!(values(&e.h.app, 3), ["leaf#4", "mid#1", "main#1"]);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_that_moved_or_a_return_into_a_reused_frame_is_fetched_again() -> anyhow::Result<()>
{
    let mut e = env(|p| {
        line_script(
            p,
            vec![
                stack_stop(p, 1, &[("leaf", 2), ("mid", 10), ("main", 20)]),
                // mid moved from line 10 to 11
                stack_stop(p, 2, &[("leaf", 3), ("mid", 11), ("main", 20)]),
                // leaf returned: mid is the top, and main (reused at step 2) stays carried
                stack_stop(p, 3, &[("mid", 12), ("main", 20)]),
                // mid calls again from the line it was stopped at: the same frame, at the same
                // depth from the bottom, that was fetched as the top at step 3
                stack_stop(p, 4, &[("leaf", 13), ("mid", 12), ("main", 20)]),
            ],
        )
    })
    .await?;
    e.start_engine().await?;
    e.advance(3).await?;

    assert_eq!(
        carried(&e.h.app),
        [
            vec![N, N, N],
            vec![N, N, s(1)],
            vec![N, s(1)],
            vec![N, s(3), s(1)],
        ]
    );
    // fetched: 3 frames; top + moved mid; the new top only; the top only
    assert_eq!(requests(&e.h.app), [6, 4, 2, 2]);
    assert_eq!(values(&e.h.app, 1), ["leaf#2", "mid#2", "main#1"]);
    assert_eq!(values(&e.h.app, 2), ["mid#3", "main#1"]);
    assert_eq!(values(&e.h.app, 3), ["leaf#4", "mid#3", "main#1"]);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frames_are_matched_by_their_depth_from_the_bottom() -> anyhow::Result<()> {
    let mut e = env(|p| {
        line_script(
            p,
            vec![
                stack_stop(p, 1, &[("leaf", 2), ("mid", 10), ("main", 20)]),
                // a call: the stack is deeper by one, the old top is now a caller at the same
                // line, and everything below keeps its depth from the bottom
                stack_stop(
                    p,
                    2,
                    &[("helper", 3), ("leaf", 2), ("mid", 10), ("main", 20)],
                ),
                // the same functions at the same lines, but at other depths: nothing but the
                // bottom matches
                stack_stop(p, 3, &[("a", 4), ("mid", 10), ("leaf", 2), ("main", 20)]),
            ],
        )
    })
    .await?;
    e.start_engine().await?;
    e.advance(2).await?;

    assert_eq!(
        carried(&e.h.app),
        [
            vec![N, N, N],
            vec![N, s(1), s(1), s(1)],
            vec![N, N, N, s(1)],
        ]
    );
    assert_eq!(requests(&e.h.app), [6, 2, 6]);
    assert_eq!(values(&e.h.app, 2), ["a#3", "mid#3", "leaf#3", "main#1"]);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thread_switch_fetches_every_frame() -> anyhow::Result<()> {
    // thread 2's third line (12) is your breakpoint, and the program takes a while to get there
    let mut e = env(|p| {
        let one = |i: u64| stack_stop(p, i as u32, &[("leaf", i), ("mid", 15), ("main", 20)]);
        let two = |i: u64, delay: u64| {
            stack_stop(
                p,
                100 + i as u32,
                &[("work", i), ("wmid", 16), ("start", 21)],
            )
            .delay(delay)
        };
        Script::new(p)
            .lines()
            .threads()
            .non_stop()
            .capability("supportsSingleThreadExecutionRequests", json!(true))
            .thread_program(1, (2..=9).map(one).collect())
            .thread_program(2, vec![two(10, 0), two(11, 0), two(12, 1500)])
    })
    .await?;
    let line = e.path.clone();
    debug::breakpoints::toggle(&mut e.h.app.editor, line, 11);
    e.launch()?;
    e.settled(1).await;
    e.h.until("the engine ready", is_ready).await;
    e.focus().await?;
    e.h.keys("<space>").await?;
    assert_eq!(e.h.app.editor.debug.spectator.mode, Mode::Playing);
    e.h.until("thread 2's breakpoint", |app| {
        let stops = &app.editor.debug.spectator.history.stops;
        stops
            .back()
            .is_some_and(|s| s.key.thread.to_string() == "2")
    })
    .await;
    e.h.until("playback paused", |app| {
        app.editor.debug.spectator.mode == Mode::Paused
    })
    .await;

    let app = &e.h.app;
    let all = carried(app);
    let last = all.last().unwrap();
    assert_eq!(
        last,
        &vec![N, N, N],
        "the first stop on a new thread records everything"
    );
    assert_eq!(*requests(app).last().unwrap(), 6);
    assert_eq!(
        values(app, all.len() - 1),
        ["work#112", "wmid#112", "start#112"]
    );
    // the thread before it did reuse
    assert!(
        all.iter().any(|c| c.iter().any(Option::is_some)),
        "thread 1's stops lent frames: {all:?}"
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_stepping_engine_reuses_frames_across_step_stops_only() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(stack_stop(p, 1, &[("leaf", 2), ("mid", 10), ("main", 20)]))
            .stop(stack_stop(p, 2, &[("leaf", 3), ("mid", 10), ("main", 20)]))
            // a step that ends on a stop of another reason (a breakpoint, say): not a plain step
            .stop(stack_stop(p, 3, &[("leaf", 4), ("mid", 10), ("main", 20)]).keep_reason())
            .stop(stack_stop(p, 4, &[("leaf", 5), ("mid", 10), ("main", 20)]))
    })
    .await?;
    // set the stop reasons: the first is the entry, the third a breakpoint
    e.h.app.editor.debug.engine.kind = EngineKind::Stepping;
    e.launch()?;
    e.settled(1).await;
    e.focus().await?;
    e.advance(3).await?;

    let all = carried(&e.h.app);
    assert_eq!(all[0], [N, N, N]);
    assert_eq!(all[1], [N, s(1), s(1)], "reused across a step");
    assert_eq!(requests(&e.h.app)[..2], [6, 2]);
    let reasons: Vec<String> =
        e.h.app
            .editor
            .debug
            .spectator
            .history
            .stops
            .iter()
            .map(|s| s.reason.clone())
            .collect();
    assert_eq!(reasons[1], "step", "{reasons:?}");
    if reasons[2] != "step" {
        assert_eq!(all[2], [N, N, N], "{reasons:?}: not straight from a step");
        assert_eq!(all[3], [N, s(3), s(3)]);
    } else {
        // the fake reported the step as one
        assert_eq!(all[2], [N, s(1), s(1)]);
    }
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// The carried marker

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rewind_says_a_frame_was_carried_from_the_step_that_fetched_it() -> anyhow::Result<()> {
    let mut e = env(|p| {
        line_script(
            p,
            (0..4)
                .map(|i| {
                    stack_stop(
                        p,
                        i + 1,
                        &[("leaf", 2 + i as u64), ("mid", 10), ("main", 20)],
                    )
                })
                .collect(),
        )
    })
    .await?;
    e.start_engine().await?;
    e.advance(3).await?;
    assert_eq!(
        texts(&e.h.app, Section::Variables)
            .first()
            .map(String::as_str),
        Some("Locals")
    );

    e.h.keys("h").await?;
    assert!(e.h.app.editor.debug.is_rewinding());
    // the top frame was fetched at this stop
    let top = texts(&e.h.app, Section::Variables);
    assert_eq!(top, ["Locals", "at = leaf#3"]);
    assert_eq!(e.h.app.editor.debug.shown_carried(), None);

    // the callers were carried from step 1
    for frame in [1, 2] {
        rewind::select_frame(&mut e.h.app.editor, frame);
        let rows = texts(&e.h.app, Section::Variables);
        assert_eq!(rows[0], "carried from step 1", "{rows:?}");
        assert_eq!(
            rows[1..],
            [
                "Locals".to_string(),
                format!("at = {}#1", ["", "mid", "main"][frame])
            ]
        );
        assert_eq!(e.h.app.editor.debug.shown_carried(), Some(1));
    }
    // and back at a stop that fetched everything, there is none
    // the focused panel rewinds by ten (`Tab` focuses it in debug mode)
    e.h.keys("<tab>{").await?;
    rewind::select_frame(&mut e.h.app.editor, 1);
    let rows = texts(&e.h.app, Section::Variables);
    assert_eq!(rows, ["Locals", "at = mid#1"], "step 1 fetched it itself");
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Pipelining

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_arriving_out_of_order_still_land_in_their_own_frames_within_the_bound(
) -> anyhow::Result<()> {
    const DEPTH: usize = 24;
    let mut e = env(|p| {
        // a stack deeper than the bound, so the semaphore has to hold requests back; answers wait
        // for 5 requests and come back in reverse
        let frames: Vec<(String, u64)> = (0..DEPTH)
            .map(|i| (format!("f{i}"), if i == 0 { 2 } else { 20 + i as u64 }))
            .collect();
        let frames: Vec<(&str, u64)> = frames.iter().map(|(n, l)| (n.as_str(), *l)).collect();
        line_script(
            p,
            vec![stack_stop(p, 1, &frames), stack_stop(p, 2, &frames)],
        )
        .hold_answers(5)
    })
    .await?;
    e.start_engine().await?;
    e.advance(1).await?;

    for (index, tag) in [(0, 1), (1, 2)] {
        let expected: Vec<String> = (0..DEPTH).map(|i| format!("f{i}#{tag}")).collect();
        let got = values(&e.h.app, index);
        if index == 0 {
            assert_eq!(got, expected, "every frame holds its own variables");
        } else {
            // the second stop lends its callers: only the top frame is its own
            assert_eq!(got[0], "f0#2");
            assert!(got[1..]
                .iter()
                .enumerate()
                .all(|(i, v)| *v == format!("f{}#1", i + 1)));
        }
    }
    let max = e.fake.max_in_flight();
    assert!(max > 1, "the requests went out together: {max}");
    // the live panel's own requests for the top frame may add a couple on top of the capture's
    assert!(
        max <= IN_FLIGHT + 2,
        "at most {IN_FLIGHT} requests of the capture at once (+ the live panel's): {max}"
    );
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// The staged install

/// Eight files: `main.rs` is open, `b.rs` is the launch's program, `c.rs` has your breakpoint
/// and `d.rs`..`h.rs` are the rest, whose breakpoints are answered late.
const FILES: [(&str, usize); 8] = [
    ("main.rs", 30),
    ("b.rs", 5),
    ("c.rs", 5),
    ("d.rs", 5),
    ("e.rs", 5),
    ("f.rs", 5),
    ("g.rs", 5),
    ("h.rs", 5),
];

async fn staged(delay: u64, script: impl FnOnce(&str) -> Script) -> anyhow::Result<Env> {
    let mut e = env_with(&FILES, "main.rs", ".", |paths| {
        let mut s = script(&paths[0]);
        for (path, (_, lines)) in paths.iter().zip(FILES) {
            let all: Vec<u64> = (1..=lines as u64).collect();
            s = s.breakpoint_locations(path, &all);
        }
        s.slow_breakpoints(delay, &["main.rs", "b.rs", "c.rs"])
    })
    .await?;
    let c = e.dir.join("c.rs");
    debug::breakpoints::toggle(&mut e.h.app.editor, c, 2);
    Ok(e)
}

impl Env {
    /// The files `setBreakpoints` was asked for, in order of the requests.
    fn files_set(&self) -> Vec<String> {
        self.fake
            .requests_named("setBreakpoints")
            .into_iter()
            .map(|(_, args)| {
                Path::new(args["source"]["path"].as_str().unwrap())
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect()
    }

    fn launch_staged(&mut self) -> anyhow::Result<()> {
        // relative to `cwd`, as most launch configurations have it
        self.launch_with(json!("b.rs"))
    }
}

fn partial(app: &Application) -> bool {
    app.editor.debug.engine.sessions.values().any(|s| s.partial)
}

fn covered(app: &Application) -> usize {
    app.editor
        .debug
        .engine
        .record
        .marks()
        .filter(|m| matches!(m.kind, MarkKind::Covered))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_files_likely_to_run_first_go_in_first_and_the_program_starts_before_the_rest(
) -> anyhow::Result<()> {
    let mut e = staged(1500, |p| {
        line_script(
            p,
            (0..6)
                .map(|i| stack_stop(p, i + 1, &[("m", 2 + i as u64)]))
                .collect(),
        )
    })
    .await?;
    e.launch_staged()?;
    // the program goes on while the rest are going in
    e.h.until("configurationDone", |_| {
        !e.fake.requests_named("configurationDone").is_empty()
    })
    .await;
    assert!(
        partial(&e.h.app),
        "the rest of the files are still going in"
    );
    let label = e.h.app.editor.debug.engine.task.as_ref().unwrap().label();
    assert!(
        label.contains("lines in the rest aren't counted yet"),
        "{label}"
    );
    assert!(label.contains("/8 files"), "{label}");

    let fake = &e.fake;
    e.h.until("every file set", |_| {
        fake.requests_named("setBreakpoints").len() >= 8
    })
    .await;
    let order = e.files_set();
    // the first ones: the open buffer, the program, the file with your breakpoint. Up to 4 are
    // in flight (engine::IN_FLIGHT), and the 5th starts only when one is done
    let head: Vec<&str> = order[..4].iter().map(String::as_str).collect();
    for first in ["main.rs", "b.rs", "c.rs"] {
        assert!(
            head.contains(&first),
            "{first} among the first 4: {order:?}"
        );
    }
    // your breakpoint of c.rs went with it
    let c = e
        .fake
        .requests_named("setBreakpoints")
        .into_iter()
        .find(|(_, a)| a["source"]["path"].as_str().unwrap().ends_with("c.rs"))
        .unwrap();
    assert_eq!(c.1["breakpoints"][0]["line"], 3, "yours first");

    // configurationDone went out before the last file was even asked for
    let log = e.fake.requests();
    let at = |name: &str| log.iter().position(|(_, c, _)| c == name).unwrap();
    let config = at("configurationDone");
    let last_set = log
        .iter()
        .rposition(|(_, c, _)| c == "setBreakpoints")
        .unwrap();
    assert!(
        config < last_set,
        "configurationDone at {config}, last setBreakpoints at {last_set}"
    );
    // the first group's three answers were in before it
    let firsts_before: Vec<String> = log[..config]
        .iter()
        .filter(|(_, c, _)| c == "setBreakpoints")
        .map(|(_, _, a)| a["source"]["path"].as_str().unwrap().to_string())
        .collect();
    for first in ["main.rs", "b.rs", "c.rs"] {
        assert!(
            firsts_before.iter().any(|p| p.ends_with(first)),
            "{first}: {firsts_before:?}"
        );
    }
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hits_are_counted_while_the_rest_installs_and_the_end_marks_the_coverage(
) -> anyhow::Result<()> {
    let mut e = staged(3500, |p| {
        line_script(
            p,
            (0..8)
                .map(|i| {
                    stack_stop(
                        p,
                        i + 1,
                        &[("leaf", 2 + i as u64), ("mid", 15), ("main", 20)],
                    )
                })
                .collect(),
        )
    })
    .await?;
    e.launch_staged()?;
    e.settled(1).await;
    e.focus().await?;
    assert!(partial(&e.h.app), "still installing at the first line");
    assert_eq!(
        e.h.app
            .editor
            .debug
            .engine
            .sessions
            .values()
            .next()
            .unwrap()
            .state,
        SessionState::Ready,
        "the hidden breakpoints of the first files work"
    );
    assert!(e.h.app.editor.debug.engine.task.as_ref().unwrap().running);
    assert_eq!(covered(&e.h.app), 0);
    // the hit of the first line is counted, and so is the next
    e.advance(1).await?;
    let counted = e.h.app.editor.debug.engine.record.len();
    assert!(counted >= 2, "{counted} hits");
    // nothing is lent while lines of the rest may run unseen
    assert!(partial(&e.h.app), "the rest hasn't finished yet");
    assert_eq!(carried(&e.h.app), [vec![N, N, N], vec![N, N, N]]);
    assert_eq!(requests(&e.h.app), [6, 6]);

    e.h.until("the rest is in", |app| {
        partial(app).eq(&false) && is_ready(app)
    })
    .await;
    assert_eq!(covered(&e.h.app), 1, "coverage is whole from here");
    assert!(e.h.app.editor.debug.engine.task.is_none());
    // from here on the frames are lent again, from the step that fetched them
    e.advance(1).await?;
    assert_eq!(carried(&e.h.app)[2], [N, s(2), s(2)]);
    assert_eq!(requests(&e.h.app)[2], 2);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_file_installs_whole_before_the_program_starts_and_marks_nothing(
) -> anyhow::Result<()> {
    let mut e = env(|p| line_script(p, vec![stack_stop(p, 1, &[("m", 2)])])).await?;
    e.launch()?;
    e.settled(1).await;
    assert!(!partial(&e.h.app));
    e.h.until("the engine ready", is_ready).await;
    e.h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(
        covered(&e.h.app),
        0,
        "no Covered mark when nothing was late"
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn when_no_file_is_first_all_of_them_count_as_first() -> anyhow::Result<()> {
    // the open buffer, the program and the breakpoints are all outside the covered folder
    let files = [
        ("main.rs", 5),
        ("src/a.rs", 5),
        ("src/b.rs", 5),
        ("src/c.rs", 5),
    ];
    let mut e = env_with(&files, "main.rs", "src", |paths| {
        let mut s = line_script(&paths[1], vec![stack_stop(&paths[1], 1, &[("m", 2)])]);
        for p in &paths[1..] {
            s = s.breakpoint_locations(p, &[1, 2, 3, 4, 5]);
        }
        s.slow_breakpoints(300, &[])
    })
    .await?;
    e.launch()?;
    e.settled(1).await;
    // the program started only when every file was in
    assert!(!partial(&e.h.app));
    assert_eq!(e.files_set().len(), 3);
    let log = e.fake.requests();
    let config = log
        .iter()
        .position(|(_, c, _)| c == "configurationDone")
        .unwrap();
    assert_eq!(
        log[..config]
            .iter()
            .filter(|(_, c, _)| c == "setBreakpoints")
            .count(),
        3
    );
    e.h.until("the engine ready", is_ready).await;
    assert_eq!(covered(&e.h.app), 0);
    e.h.finish().await
}
