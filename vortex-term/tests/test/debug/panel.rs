//! The debug panel against the scripted fake adapter: sessions, stops, the panel's keys, the
//! console, breakpoints, child sessions.
//!
//! The editor is pumped until conditions hold, with a deadline. Menus: `Space G` is sticky, so
//! the tests close it with `<esc>` after a command (the panel must stay).

use std::path::PathBuf;

use indoc::indoc;
use serde_json::{json, Value};
use tempfile::NamedTempFile;
use vortex_core::syntax::config::DebugAdapterConfig;
use vortex_term::{application::Application, debug, debug::panel};
use vortex_view::{
    current_ref,
    debug::{LineKind, Role, Section, Status},
};

use super::{
    fake_adapter::{FakeAdapter, Scope, Script, Stop, Var},
    harness::Harness,
};
use crate::test::helpers::{test_config, test_syntax_loader, AppBuilder};

const SOURCE: &str = indoc! {"
    fn main() {
        let x = 1;
        let p = point(x);
        step(p);
        step(p);
        step(p);
        step(p);
        end();
    }
"};

struct Env {
    h: Harness,
    fake: FakeAdapter,
    /// The path the editor knows the file by.
    path: PathBuf,
    _file: NamedTempFile,
}

async fn env_with(
    builder: AppBuilder,
    file: NamedTempFile,
    script: impl FnOnce(&str) -> Script,
) -> anyhow::Result<Env> {
    let h = Harness::new(builder.with_file(file.path(), None))?;
    let path = {
        let (_, doc) = current_ref!(h.app.editor);
        doc.path().expect("the file has a path").to_owned()
    };
    let fake = FakeAdapter::start(script(&path.to_string_lossy())).await?;
    Ok(Env {
        h,
        fake,
        path,
        _file: file,
    })
}

fn source_file() -> anyhow::Result<NamedTempFile> {
    let mut file = tempfile::Builder::new().suffix(".rs").tempfile()?;
    std::io::Write::write_all(&mut file, SOURCE.as_bytes())?;
    Ok(file)
}

/// A file of 9 lines open in the editor, and a fake adapter running `script` (given the file's
/// path).
async fn env(script: impl FnOnce(&str) -> Script) -> anyhow::Result<Env> {
    env_with(AppBuilder::new(), source_file()?, script).await
}

fn adapter_config(path: &std::path::Path) -> DebugAdapterConfig {
    DebugAdapterConfig {
        name: "fake".into(),
        transport: "tcp".into(),
        command: String::new(),
        args: Vec::new(),
        port_arg: None,
        templates: Vec::new(),
        quirks: Default::default(),
        own_paths: Some(
            path.parent()
                .map(|dir| vec![dir.to_owned()])
                .unwrap_or_default(),
        ),
        std_paths: None,
        lib_paths: None,
        capabilities: None,
    }
}

impl Env {
    /// Starts the session with `debug::start`.
    fn launch(&mut self) -> anyhow::Result<()> {
        // these tests are about the panel, not playback: the engine has its own tests
        self.h.app.editor.debug.engine.kind = vortex_view::debug::engine::EngineKind::Stepping;
        debug::start(
            &mut self.h.app.editor,
            Some(self.fake.addr),
            &adapter_config(&self.path),
            "fake",
            "launch",
            json!({"program": "prog"}),
        )
    }

    /// Waits until the fake has received `n` requests named `command`; returns the n-th.
    async fn request(&mut self, command: &str, n: usize) -> (usize, Value) {
        let fake = &self.fake;
        self.h
            .until(&format!("request {command} #{n}"), |_| {
                fake.requests_named(command).len() >= n
            })
            .await;
        self.fake.requests_named(command)[n - 1].clone()
    }

    /// Waits for a stop, and for its variables.
    async fn stopped(&mut self, var: &str) {
        self.h
            .until("a stop", |app| {
                matches!(app.editor.debug.status, Status::Stopped { .. })
            })
            .await;
        self.has_row(Section::Variables, var).await;
    }

    async fn has_row(&mut self, section: Section, prefix: &str) {
        self.h
            .until(&format!("a row {prefix:?} in {section:?}"), |app| {
                texts(app, section).iter().any(|t| t.starts_with(prefix))
            })
            .await;
    }

    fn requests_in_order(&self) -> Vec<String> {
        self.fake
            .requests()
            .into_iter()
            .map(|(s, c, _)| format!("{s}:{c}"))
            .collect()
    }
}

fn texts(app: &Application, section: Section) -> Vec<String> {
    panel::rows(&app.editor, section)
        .iter()
        .map(|row| row.text())
        .collect()
}

fn row_has_role(app: &Application, section: Section, prefix: &str, role: Role) -> bool {
    let rows = panel::rows(&app.editor, section);
    let row = rows
        .iter()
        .find(|row| row.text().starts_with(prefix))
        .unwrap_or_else(|| panic!("no row {prefix:?}: {:?}", texts(app, section)));
    row.parts.iter().any(|(_, r)| *r == role)
}

fn console(app: &Application) -> Vec<(LineKind, String)> {
    app.editor
        .debug
        .console
        .lines
        .iter()
        .map(|line| (line.kind, line.text.clone()))
        .collect()
}

fn console_has(app: &Application, kind: LineKind, needle: &str) -> bool {
    console(app)
        .iter()
        .any(|(k, text)| *k == kind && text.contains(needle))
}

fn cursor_line(app: &Application) -> usize {
    let (view, doc) = current_ref!(app.editor);
    doc.selection(view.id)
        .primary()
        .cursor_line(doc.text().slice(..))
}

fn current_path(app: &Application) -> Option<PathBuf> {
    let (_, doc) = current_ref!(app.editor);
    doc.path().map(|p| p.to_owned())
}

/// The id of the first frame of the thread the panel follows.
fn top_frame_id(app: &Application) -> i64 {
    let client = app.editor.debug_adapters.get_active_client().unwrap();
    let thread = client.thread_id.unwrap();
    client.stack_frames[&thread][0].id as i64
}

fn no_sessions(app: &Application) -> bool {
    app.editor.debug_adapters.iter().next().is_none()
}

fn is_stopped(app: &Application, reason: &str) -> bool {
    matches!(&app.editor.debug.status, Status::Stopped { reason: r, .. } if r == reason)
}

fn locals(x: &str, a: &str) -> Scope {
    Scope::new("Locals").var(Var::new("x", x).ty("int")).var(
        Var::new("p", "Point")
            .ty("Point")
            .child(Var::new("a", a))
            .child(Var::new("inner", "Inner").child(Var::new("z", "9"))),
    )
}

fn simple(x: &str) -> Scope {
    Scope::new("Locals").var(Var::new("x", x).ty("int"))
}

// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_breakpoint_set_before_the_launch_is_configured_and_the_stop_fills_the_panel(
) -> anyhow::Result<()> {
    let mut e = env(|p| Script::new(p).stop(Stop::new(3, "breakpoint").scope(simple("1")))).await?;

    // debug mode's `b` on line 3, then back to the top: the stop has to move the cursor
    e.h.keys("2j<space>Gb<esc>gg").await?;
    assert_eq!(cursor_line(&e.h.app), 0);
    let list = &e.h.app.editor.breakpoints[&e.path];
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].line, 2);
    assert!(!list[0].verified);

    e.launch()?;
    assert!(e.h.app.editor.debug.visible, "the panel opens at the start");
    assert!(!e.h.app.editor.debug.focused);
    e.stopped("x = 1").await;

    let (_, args) = e.request("setBreakpoints", 1).await;
    assert_eq!(args["source"]["path"], e.path.to_string_lossy().as_ref());
    assert_eq!(args["breakpoints"].as_array().unwrap().len(), 1);
    assert_eq!(args["breakpoints"][0]["line"], 3, "lines are 1-based");
    assert_eq!(e.fake.requests_named("setBreakpoints").len(), 1);
    assert_eq!(e.fake.requests_named("configurationDone").len(), 1);
    let order = e.requests_in_order();
    let position = |name: &str| order.iter().position(|r| r.ends_with(name)).unwrap();
    assert!(position("setBreakpoints") < position("configurationDone"));

    let app = &e.h.app;
    assert!(app.editor.debug.visible);
    assert!(!app.editor.debug.focused);
    assert_eq!(
        app.editor.debug.status,
        Status::Stopped {
            reason: "breakpoint".into(),
            description: None
        }
    );
    assert_eq!(app.editor.debug.status.label(), "stopped: breakpoint");

    // threads and frames
    assert_eq!(app.editor.debug.threads.len(), 1);
    assert_eq!(app.editor.debug.threads[0].threads[0].name, "main");
    let client = app.editor.debug_adapters.get_active_client().unwrap();
    let frames = &client.stack_frames[&client.thread_id.unwrap()];
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].name, "main");
    assert_eq!(frames[0].line, 3);
    assert!(texts(app, Section::Stack)
        .iter()
        .any(|t| t.starts_with("main") && t.contains(":3")));

    // the editor jumped to the frame
    assert_eq!(current_path(app), Some(e.path.clone()));
    assert_eq!(cursor_line(app), 2);

    // the variables of the first scope
    assert_eq!(
        texts(app, Section::Variables),
        ["Locals", "x = 1  int"],
        "first scope is open"
    );

    // the breakpoint is verified (the answer comes with the configuration)
    e.h.until("the breakpoint to be verified", |app| {
        app.editor.breakpoints[&e.path][0].verified
    })
    .await;
    assert!(texts(&e.h.app, Section::Breakpoints)[0].starts_with("● "));

    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expanded_node_is_reloaded_after_a_step_and_changes_are_marked() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(3, "entry").scope(locals("1", "5")))
            .stop(Stop::new(5, "entry").scope(locals("2", "6")))
    })
    .await?;
    e.launch()?;
    e.stopped("x = 1").await;
    assert_eq!(
        texts(&e.h.app, Section::Variables),
        ["Locals", "x = 1  int", "p = Point  Point"]
    );
    let p_ref = e.h.app.editor.debug.scopes[0].children.as_ref().unwrap()[1].reference;
    assert_ne!(p_ref, 0);

    // C-g, down to p, l
    e.h.keys("<C-g>").await?;
    assert!(e.h.app.editor.debug.is_focused());
    e.h.keys("jjl").await?;
    e.has_row(Section::Variables, "a = 5").await;
    assert!(
        e.fake
            .requests_named("variables")
            .iter()
            .any(|(_, args)| args["variablesReference"] == p_ref),
        "variables for p's reference {p_ref}: {:?}",
        e.fake.requests_named("variables")
    );
    assert_eq!(
        texts(&e.h.app, Section::Variables),
        [
            "Locals",
            "x = 1  int",
            "p = Point  Point",
            "a = 5",
            "inner = Inner"
        ]
    );

    // Enter on `inner` (two rows down) opens it as well
    e.h.keys("jj<ret>").await?;
    e.has_row(Section::Variables, "z = 9").await;

    // step over in debug mode (the focused panel gives the editor back first); Esc leaves it
    // and the panel stays
    e.h.keys("<C-g>").await?;
    e.h.keys("<space>G.").await?;
    e.h.keys("<esc>").await?;
    e.h.until("the second stop", |app| {
        texts(app, Section::Variables)
            .iter()
            .any(|t| t.starts_with("x = 2"))
    })
    .await;
    assert!(e.h.app.editor.debug.visible);

    let new_p_ref = e.h.app.editor.debug.scopes[0].children.as_ref().unwrap()[1].reference;
    assert_ne!(new_p_ref, p_ref, "the fake changes references per stop");
    let variables = e.fake.requests_named("variables");
    assert!(
        variables
            .iter()
            .any(|(_, args)| args["variablesReference"] == new_p_ref),
        "the expanded node was loaded with the new reference {new_p_ref}: {variables:?}"
    );
    let app = &e.h.app;
    assert_eq!(
        texts(app, Section::Variables),
        [
            "Locals",
            "x = 2  int",
            "p = Point  Point",
            "a = 6",
            "inner = Inner",
            "z = 9"
        ]
    );
    assert!(row_has_role(
        app,
        Section::Variables,
        "x = 2",
        Role::Changed
    ));
    assert!(row_has_role(
        app,
        Section::Variables,
        "a = 6",
        Role::Changed
    ));
    assert!(!row_has_role(
        app,
        Section::Variables,
        "z = 9",
        Role::Changed
    ));
    assert!(!row_has_role(
        app,
        Section::Variables,
        "p = ",
        Role::Changed
    ));

    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_is_evaluated_now_and_at_the_next_stop() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(3, "entry").scope(simple("1")))
            .stop(Stop::new(5, "entry").scope(simple("2")))
    })
    .await?;
    e.launch()?;
    e.stopped("x = 1").await;
    let frame = top_frame_id(&e.h.app);

    e.h.keys("<space>Gw").await?;
    e.h.keys("x<ret>").await?;
    e.h.keys("<esc>").await?;

    let (_, args) = e.request("evaluate", 1).await;
    assert_eq!(args["expression"], "x");
    assert_eq!(args["context"], "watch");
    assert_eq!(args["frameId"], frame);
    e.h.until("the watch's value", |app| {
        app.editor
            .debug
            .watches
            .first()
            .is_some_and(|w| w.node.as_ref().is_some_and(|n| n.value == "1" && !n.error))
    })
    .await;
    e.h.keys("<C-g>2").await?;
    assert_eq!(texts(&e.h.app, Section::Watch), ["x = 1  int"]);
    e.h.keys("<C-g>").await?;

    // the next stop evaluates it again, in the new frame
    e.h.keys("<space>G.").await?;
    e.h.keys("<esc>").await?;
    e.h.until("the watch at the second stop", |app| {
        app.editor.debug.watches[0]
            .node
            .as_ref()
            .is_some_and(|n| n.value == "2")
    })
    .await;
    let new_frame = top_frame_id(&e.h.app);
    assert_ne!(new_frame, frame);
    let watch_evaluations: Vec<_> = e
        .fake
        .requests_named("evaluate")
        .into_iter()
        .filter(|(_, args)| args["context"] == "watch")
        .collect();
    // once per stop for the panel (the recording for rewind evaluates it again in the same
    // frame)
    let mut frames: Vec<_> = watch_evaluations
        .iter()
        .map(|(_, args)| args["frameId"].clone())
        .collect();
    frames.dedup();
    assert_eq!(frames.len(), 2, "{watch_evaluations:?}");
    assert_eq!(frames[1], new_frame);
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn debug_eval_runs_in_the_repl_context_and_the_console_shows_it() -> anyhow::Result<()> {
    let mut e = env(|p| Script::new(p).stop(Stop::new(3, "entry").scope(simple("1")))).await?;
    e.launch()?;
    e.stopped("x = 1").await;
    let frame = top_frame_id(&e.h.app);

    e.h.keys(":debug-eval<space>1<space>+<space>2<ret>").await?;
    e.h.until("the result", |app| console_has(app, LineKind::Result, "3"))
        .await;
    let (_, args) = e.request("evaluate", 1).await;
    assert_eq!(args["expression"], "1 + 2");
    assert_eq!(args["context"], "repl");
    assert_eq!(args["frameId"], frame);
    let lines = console(&e.h.app);
    assert!(
        lines.contains(&(LineKind::Input, "1 + 2".into())),
        "{lines:?}"
    );
    assert!(lines.contains(&(LineKind::Result, "3".into())), "{lines:?}");

    e.h.keys(":debug-eval<space>nope<ret>").await?;
    e.h.until("an error line", |app| {
        console(app).iter().any(|(k, _)| *k == LineKind::Error)
    })
    .await;
    assert!(console(&e.h.app).contains(&(LineKind::Input, "nope".into())));
    assert!(e.h.app.editor.debug.visible);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_value_can_be_changed_from_the_panel() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .set_variable()
            .stop(Stop::new(3, "entry").scope(simple("1")))
    })
    .await?;
    e.launch()?;
    e.stopped("x = 1").await;
    let scope_ref = e.h.app.editor.debug.scopes[0].reference;

    // c on x: the prompt starts with the value
    e.h.keys("<C-g>jc").await?;
    e.h.keys("<backspace>42<ret>").await?;
    let (_, args) = e.request("setVariable", 1).await;
    assert_eq!(args["variablesReference"], scope_ref);
    assert_eq!(args["name"], "x");
    assert_eq!(args["value"], "42");
    e.has_row(Section::Variables, "x = 42").await;
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_to_the_end_ends_the_session_and_keeps_the_console() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .output("stdout", "hello\n")
            .exit_code(7)
            .stop(Stop::new(3, "breakpoint").scope(simple("1")))
    })
    .await?;
    e.h.keys("2j<space>Gb<esc>").await?;
    e.launch()?;
    e.stopped("x = 1").await;
    e.h.until("the breakpoint to be verified", |app| {
        app.editor.breakpoints[&e.path][0].verified
    })
    .await;

    e.h.keys("<space>GL").await?;
    e.h.keys("<esc>").await?;
    e.h.until("the end of the session", no_sessions).await;
    // the panel hides and the status says how it ended
    let app = &e.h.app;
    assert!(!app.editor.debug.visible);
    assert!(!app.editor.debug.focused);
    assert_eq!(app.editor.debug.status, Status::Ended { code: Some(7) });
    assert_eq!(app.editor.debug.status.label(), "exited 7");
    assert!(no_sessions(app));
    assert!(!app.editor.breakpoints[&e.path][0].verified);
    // what the program printed stays in the console, with how it exited
    let lines = console(app);
    assert!(
        lines.contains(&(LineKind::Stdout, "hello".into())),
        "{lines:?}"
    );
    assert!(
        lines.contains(&(LineKind::Info, "Exited with code 7".into())),
        "{lines:?}"
    );
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

fn child_script(path: &str) -> Script {
    Script::new(path)
        .child()
        .stop(Stop::new(3, "breakpoint").scope(simple("1")))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_session_is_configured_once_and_the_panel_follows_it() -> anyhow::Result<()> {
    let mut e = env(child_script).await?;
    e.h.keys("2j<space>Gb<esc>gg").await?;
    e.launch()?;
    e.stopped("x = 1").await;

    // startDebugging got a successful reply; the child is a second connection
    let fake = &e.fake;
    assert!(
        fake.client_responses()
            .iter()
            .any(|(s, c, ok, _)| *s == 0 && c == "startDebugging" && *ok),
        "{:?}",
        fake.client_responses()
    );
    assert_eq!(fake.connections(), 2);
    // ... that launched with the configuration of the request
    let launches = fake.requests_named("launch");
    assert_eq!(launches.len(), 2, "{launches:?}");
    assert_eq!(launches[0].0, 0);
    assert_eq!(launches[1].0, 1);
    assert_eq!(launches[1].1["__pendingTargetId"], "t1");
    assert_eq!(launches[1].1["name"], "child [1]");

    // `initialized` came twice on the child: one configuration
    let on_child = |command: &str| {
        fake.requests_named(command)
            .iter()
            .filter(|(s, _)| *s == 1)
            .cloned()
            .collect::<Vec<_>>()
    };
    let set_breakpoints = on_child("setBreakpoints");
    assert_eq!(set_breakpoints.len(), 1, "{set_breakpoints:?}");
    assert_eq!(set_breakpoints[0].1["breakpoints"][0]["line"], 3);
    assert_eq!(on_child("configurationDone").len(), 1);
    // and the parent was configured once too
    let on_parent = |command: &str| {
        fake.requests_named(command)
            .iter()
            .filter(|(s, _)| *s == 0)
            .count()
    };
    assert_eq!(on_parent("setBreakpoints"), 1);
    assert_eq!(on_parent("configurationDone"), 1);

    // the stop happened in the child and the panel follows it
    let app = &e.h.app;
    assert_eq!(app.editor.debug_adapters.iter().count(), 2);
    let active = app.editor.debug_adapters.get_active_client().unwrap();
    let root = app.editor.debug_adapters.root(active.id());
    assert_ne!(active.id(), root);
    assert_eq!(active.parent, Some(root));
    assert!(app
        .editor
        .debug_adapters
        .get_client(root)
        .unwrap()
        .parent
        .is_none());
    assert_eq!(app.editor.debug.status.label(), "stopped: breakpoint");
    assert_eq!(cursor_line(app), 2);
    e.h.until("the breakpoint to be verified", |app| {
        app.editor.breakpoints[&e.path][0].verified
    })
    .await;

    // the child exits, then the parent: the session ends
    e.h.keys("<space>GL").await?;
    e.h.keys("<esc>").await?;
    e.h.until("the end of the session", no_sessions).await;
    let app = &e.h.app;
    assert!(!app.editor.debug.visible);
    assert!(matches!(app.editor.debug.status, Status::Ended { .. }));
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn toggling_a_breakpoint_reaches_the_parent_and_the_child_session() -> anyhow::Result<()> {
    let mut e = env(child_script).await?;
    e.h.keys("2j<space>Gb<esc>gg").await?;
    e.launch()?;
    e.stopped("x = 1").await;

    // the cursor is on line 3 (the stop); line 5 gets a breakpoint
    e.h.keys("<space>Gv2jb<esc>").await?;
    let fake = &e.fake;
    e.h.until("setBreakpoints on both sessions", |_| {
        let all = fake.requests_named("setBreakpoints");
        [0, 1]
            .iter()
            .all(|session| all.iter().filter(|(s, _)| s == session).count() >= 2)
    })
    .await;
    for session in [0, 1] {
        let last = e
            .fake
            .requests_named("setBreakpoints")
            .into_iter()
            .rfind(|(s, _)| *s == session)
            .unwrap()
            .1;
        let lines: Vec<_> = last["breakpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["line"].as_u64().unwrap())
            .collect();
        assert_eq!(lines, [3, 5], "session {session}");
    }
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breakpoints_can_be_disabled_and_deleted_from_the_panel() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(3, "entry").scope(simple("1")))
            .run_forever()
    })
    .await?;
    e.launch()?;
    e.stopped("x = 1").await;

    // lines 5 and 7 (the cursor is on line 3, where the program stopped)
    e.h.keys("<space>Gv2jb").await?;
    let lines_of = |args: &Value| -> Vec<u64> {
        args["breakpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["line"].as_u64().unwrap())
            .collect()
    };
    let (_, args) = e.request("setBreakpoints", 1).await;
    assert_eq!(lines_of(&args), [5]);
    e.h.keys("2jb<esc>").await?;
    let (_, args) = e.request("setBreakpoints", 2).await;
    assert_eq!(lines_of(&args), [5, 7]);
    e.h.until("both verified", |app| {
        texts(app, Section::Breakpoints).len() == 2
            && texts(app, Section::Breakpoints)
                .iter()
                .all(|t| t.starts_with("● "))
    })
    .await;

    // 4 selects the Breakpoints section; t disables the one under the cursor (line 5)
    e.h.keys("<C-g>4").await?;
    assert_eq!(e.h.app.editor.debug.section, Section::Breakpoints);
    e.h.keys("t").await?;
    let (_, args) = e.request("setBreakpoints", 3).await;
    assert_eq!(lines_of(&args), [7], "a disabled breakpoint is not sent");
    let rows = texts(&e.h.app, Section::Breakpoints);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].starts_with("○ ") && rows[0].ends_with(":5"),
        "{rows:?}"
    );
    assert!(
        rows[1].starts_with("● ") && rows[1].ends_with(":7"),
        "{rows:?}"
    );

    // t again enables it
    e.h.keys("t").await?;
    let (_, args) = e.request("setBreakpoints", 4).await;
    assert_eq!(lines_of(&args), [5, 7]);

    // d deletes it
    e.h.keys("d").await?;
    let (_, args) = e.request("setBreakpoints", 5).await;
    assert_eq!(lines_of(&args), [7]);
    let rows = texts(&e.h.app, Section::Breakpoints);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].ends_with(":7"));
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_exception_filter_answer_without_a_body_is_no_error() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .exception_filter("all", "All exceptions")
            .stop(Stop::new(3, "entry").scope(simple("1")))
    })
    .await?;
    e.launch()?;
    e.stopped("x = 1").await;
    let (_, args) = e.request("setExceptionBreakpoints", 1).await;
    assert!(args["filters"].is_array(), "{args}");
    e.request("configurationDone", 1).await;
    // let the answers be handled
    e.h.pump_for(std::time::Duration::from_millis(300)).await;
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    assert!(
        !matches!(
            e.h.app.editor.get_status(),
            Some((_, vortex_core::diagnostic::Severity::Error))
        ),
        "{:?}",
        e.h.app.editor.get_status()
    );
    assert!(!console(&e.h.app)
        .iter()
        .any(|(kind, _)| *kind == LineKind::Error));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_and_keys_of_the_panel() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .stop(Stop::new(3, "entry").scope(locals("1", "5")))
            .stop(Stop::new(5, "entry").scope(locals("2", "5")))
    })
    .await?;
    e.launch()?;
    e.stopped("x = 1").await;
    assert!(e.h.app.editor.debug.visible && !e.h.app.editor.debug.focused);

    // C-g focuses, C-g again gives the keys back
    e.h.keys("<C-g>").await?;
    assert!(e.h.app.editor.debug.is_focused());
    e.h.keys("<C-g>").await?;
    assert!(e.h.app.editor.debug.visible && !e.h.app.editor.debug.focused);

    // q gives them back too
    e.h.keys("<C-g>").await?;
    e.h.keys("q").await?;
    assert!(e.h.app.editor.debug.visible && !e.h.app.editor.debug.focused);
    e.h.keys("<C-g>").await?;
    assert!(e.h.app.editor.debug.is_focused());

    // j moves the panel's cursor, not the editor's
    let line = cursor_line(&e.h.app);
    assert_eq!(e.h.app.editor.debug.cursor(Section::Variables).row, 0);
    e.h.keys("j").await?;
    assert_eq!(e.h.app.editor.debug.cursor(Section::Variables).row, 1);
    assert_eq!(cursor_line(&e.h.app), line);
    e.h.keys("k").await?;
    assert_eq!(e.h.app.editor.debug.cursor(Section::Variables).row, 0);

    // debug mode, from the editor: `l` is one playback step (the focused panel takes `l`
    // for itself), and the view follows it
    e.h.keys("<C-g>").await?;
    assert!(!e.h.app.editor.debug.focused);
    e.h.keys("<space>Gl").await?;
    e.h.until("the step", |app| {
        texts(app, Section::Variables)
            .iter()
            .any(|t| t.starts_with("x = 2"))
    })
    .await;
    assert_eq!(cursor_line(&e.h.app), 4);
    // Tab focuses the panel in debug mode, and gives the keys back
    e.h.keys("<tab>").await?;
    assert!(e.h.app.editor.debug.is_focused());
    e.h.keys("<tab>").await?;
    assert!(!e.h.app.editor.debug.focused);

    // Esc leaves debug mode and the panel stays
    e.h.keys("<esc>").await?;
    assert!(!e.h.app.editor.debug.mode_on());
    assert!(e.h.app.editor.debug.visible, "Esc left debug mode only");
    e.h.keys("<space>G").await?;
    e.h.keys("<esc>").await?;
    assert!(e.h.app.editor.debug.visible, "Esc left debug mode only");
    // V focuses Variables
    e.h.keys("<space>GV").await?;
    assert!(e.h.app.editor.debug.is_focused(), "V focuses Variables");
    e.h.keys("<C-g>").await?;
    e.h.keys("<esc>").await?;
    e.h.keys("<space>GP").await?;
    assert!(!e.h.app.editor.debug.visible, "P toggled it off");
    e.h.keys("P").await?;
    e.h.keys("<esc>").await?;
    assert!(e.h.app.editor.debug.visible, "Esc left debug mode only");

    // Esc in normal mode with nothing pending hides the visible, unfocused panel
    e.h.keys("<esc>").await?;
    assert!(!e.h.app.editor.debug.visible);
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_steps_end_with_the_variables_of_the_last_stop() -> anyhow::Result<()> {
    let mut e = env(|p| {
        let mut script = Script::new(p).run_forever();
        for i in 1..=6 {
            script = script.stop(Stop::new(3, "entry").scope(simple(&i.to_string())));
        }
        script
    })
    .await?;
    e.launch()?;
    e.stopped("x = 1").await;

    // five steps in one go: debug mode stays on, `.` repeats
    e.h.keys("<space>G.....").await?;
    e.h.keys("<esc>").await?;
    e.h.until("the last stop's variables", |app| {
        texts(app, Section::Variables)
            .iter()
            .any(|t| t.starts_with("x = 6"))
    })
    .await;
    // nothing older arrives afterwards
    e.h.pump_for(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        texts(&e.h.app, Section::Variables),
        ["Locals", "x = 6  int"]
    );
    assert!(
        is_stopped(&e.h.app, "step"),
        "{:?}",
        e.h.app.editor.debug.status
    );
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn debug_remote_starts_a_session_from_a_template() -> anyhow::Result<()> {
    let lang_conf = indoc! {r#"
        [[language]]
        name = "rust"

        [language.debugger]
        name = "fake"
        transport = "tcp"

        [[language.debugger.templates]]
        name = "run"
        request = "launch"
        args = { program = "{0}" }
    "#};
    let builder = AppBuilder::new()
        .with_config(test_config())
        .with_lang_loader(test_syntax_loader(Some(lang_conf.into())));
    let mut e = env_with(builder, source_file()?, |p| {
        Script::new(p).stop(Stop::new(3, "entry").scope(simple("1")))
    })
    .await?;

    let command = format!(
        ":debug-remote<space>{}<space>run<space>/work/prog<ret>",
        e.fake.addr
    );
    e.h.keys(&command).await?;
    e.stopped("x = 1").await;
    let (session, args) = e.request("launch", 1).await;
    assert_eq!(session, 0);
    assert_eq!(args["program"], "/work/prog");
    assert!(args["cwd"].is_string(), "{args}");
    assert_eq!(e.h.app.editor.debug.session_name, "fake: run");
    assert!(e.h.app.editor.debug.visible);
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    // a second session is refused while this one runs
    e.h.keys(&command).await?;
    assert_eq!(e.fake.connections(), 1);
    assert!(e.h.app.editor.get_status().is_some());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_output_of_a_child_session_reaches_the_console() -> anyhow::Result<()> {
    let mut e = env(child_script).await?;
    e.h.keys("2j<space>Gb<esc>gg").await?;
    e.launch()?;
    e.stopped("x = 1").await;
    e.h.keys("<space>GL").await?;
    e.h.keys("<esc>").await?;
    e.h.until("the end of the session", no_sessions).await;
    assert!(
        console_has(&e.h.app, LineKind::Stderr, "Process exited with code 0"),
        "{:?}",
        console(&e.h.app)
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_evaluation_shows_the_adapters_message() -> anyhow::Result<()> {
    let mut e = env(|p| Script::new(p).stop(Stop::new(3, "entry").scope(simple("1")))).await?;
    e.launch()?;
    e.stopped("x = 1").await;
    e.h.keys(":debug-eval<space>nope<ret>").await?;
    e.h.until("the error line", |app| {
        console_has(app, LineKind::Error, "cannot evaluate")
    })
    .await;
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn esc_in_the_focused_panel_gives_the_keys_back_and_the_panel_stays() -> anyhow::Result<()> {
    let mut e = env(|p| Script::new(p).stop(Stop::new(3, "entry").scope(simple("1")))).await?;
    e.launch()?;
    e.stopped("x = 1").await;
    e.h.keys("<C-g>").await?;
    assert!(e.h.app.editor.debug.is_focused());
    e.h.keys("<esc>").await?;
    assert!(e.h.app.editor.debug.visible && !e.h.app.editor.debug.focused);
    e.h.finish().await
}

/// The rows of the screen as text.
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

fn help_is_open(app: &Application) -> bool {
    screen_rows(app)
        .iter()
        .any(|row| row.contains("any key closes"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn question_mark_in_the_focused_panel_opens_the_key_help() -> anyhow::Result<()> {
    let mut e = env(|p| Script::new(p).stop(Stop::new(3, "entry").scope(locals("1", "5")))).await?;
    e.launch()?;
    e.stopped("x = 1").await;

    assert!(!help_is_open(&e.h.app));
    e.h.keys("<C-g>").await?;
    assert!(e.h.app.editor.debug.is_focused());
    assert!(!help_is_open(&e.h.app));

    e.h.keys("?").await?;
    let rows = screen_rows(&e.h.app);
    assert!(help_is_open(&e.h.app), "{rows:#?}");
    assert!(
        rows.iter().any(|row| row.contains("Debug panel")),
        "{rows:#?}"
    );
    assert!(rows.iter().any(|row| row.contains("playback")), "{rows:#?}");
    // any key closes it, and the panel keeps the keys
    e.h.keys("x").await?;
    assert!(!help_is_open(&e.h.app));
    assert!(e.h.app.editor.debug.is_focused());
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.finish().await
}
