//! The terminal panel (`C-t`, `:sh`, the `hx` shim), against a real shell on a pty.
//!
//! The shell is `sh` so rc files don't matter. The shell's work happens on other threads and
//! reaches the editor through the job queue, so the tests pump the event loop until a condition
//! holds, with a deadline, rather than sleeping.
#![cfg(unix)]

use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use helix_term::{
    application::Application,
    config::Config,
    terminal::shim::{self, OpenFile, Request, Response},
};
use helix_view::{
    current_ref, doc,
    editor::Config as EditorConfig,
    input::parse_macro,
    terminal::{ShellStatus, TerminalConfig},
};
use termina::event::{Event, KeyEvent};
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::*;

const DEADLINE: Duration = Duration::from_secs(10);

fn config_with_shell(shell: &[&str]) -> Config {
    let mut config = test_config();
    config.editor = EditorConfig {
        terminal: TerminalConfig {
            shell: shell.iter().map(|s| s.to_string()).collect(),
            ..TerminalConfig::default()
        },
        ..test_editor_config()
    };
    config
}

/// An application whose input the test controls, and that keeps running between steps.
struct Harness {
    app: Application,
    tx: UnboundedSender<io::Result<Event>>,
    rx: UnboundedReceiverStream<io::Result<Event>>,
    running: bool,
}

impl Harness {
    fn new(builder: AppBuilder) -> anyhow::Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            app: builder.build()?,
            tx,
            rx: UnboundedReceiverStream::new(rx),
            running: true,
        })
    }

    fn with_shell(shell: &[&str]) -> anyhow::Result<Self> {
        Self::new(AppBuilder::new().with_config(config_with_shell(shell)))
    }

    fn sh() -> anyhow::Result<Self> {
        Self::with_shell(&["sh"])
    }

    /// Runs the event loop for a short while: until it is idle, at most `limit`.
    async fn step(&mut self, limit: Duration) {
        if !self.running {
            return;
        }
        let idle = tokio::time::timeout(limit, self.app.event_loop_until_idle(&mut self.rx));
        if let Ok(still_running) = idle.await {
            self.running = still_running;
        }
    }

    /// Sends the keys (`parse_macro` syntax) and lets the editor process them.
    async fn keys(&mut self, keys: &str) -> anyhow::Result<()> {
        for key in parse_macro(keys)? {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key))))?;
        }
        self.step(Duration::from_secs(3)).await;
        Ok(())
    }

    /// Runs the event loop until `cond` holds. Panics with `what` after the deadline.
    async fn until(&mut self, what: &str, cond: impl Fn(&Application) -> bool) {
        let start = Instant::now();
        loop {
            if cond(&self.app) {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "timed out waiting for: {what}\nstatus: {:?}\nscreen:\n{}",
                self.app.editor.get_status(),
                screen(&self.app)
            );
            self.step(Duration::from_millis(100)).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Runs the event loop until `future` (a blocking call on another thread) is done.
    async fn await_blocking<T: Send + 'static>(
        &mut self,
        what: &str,
        handle: tokio::task::JoinHandle<T>,
    ) -> T {
        let start = Instant::now();
        while !handle.is_finished() {
            assert!(
                start.elapsed() < DEADLINE,
                "timed out waiting for: {what}\nstatus: {:?}\nscreen:\n{}",
                self.app.editor.get_status(),
                screen(&self.app)
            );
            self.step(Duration::from_millis(100)).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        handle.await.unwrap()
    }

    /// Runs the event loop for `duration` (to check that something does *not* happen).
    async fn pump_for(&mut self, duration: Duration) {
        let start = Instant::now();
        while start.elapsed() < duration {
            self.step(Duration::from_millis(50)).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn finish(mut self) -> anyhow::Result<()> {
        if self.running {
            // closes the shell too: it is dropped with the editor
            for key in parse_macro("<esc>:q!<ret>")? {
                self.tx.send(Ok(Event::Key(KeyEvent::from(key))))?;
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), self.app.event_loop(&mut self.rx))
                .await;
        }
        let errs = self.app.close().await;
        anyhow::ensure!(errs.is_empty(), "errors closing the app: {errs:?}");
        Ok(())
    }
}

/// The text of the shell's screen, rows separated by newlines.
fn screen(app: &Application) -> String {
    let Some(shell) = &app.editor.terminal.shell else {
        return String::from("<no shell>");
    };
    let mut rows: Vec<String> = Vec::new();
    shell.visit_cells(|row, _col, cell| {
        while rows.len() <= row as usize {
            rows.push(String::new());
        }
        let text = if cell.text.is_empty() {
            " "
        } else {
            cell.text.as_str()
        };
        rows[row as usize].push_str(text);
    });
    rows.iter()
        .map(|row| row.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn screen_contains(app: &Application, needle: &str) -> bool {
    screen(app).contains(needle)
}

/// The prompt of `sh` is `$ `, or `# ` as root.
fn has_prompt(app: &Application) -> bool {
    screen(app)
        .lines()
        .any(|line| matches!(line.trim(), "$" | "#"))
}

fn shell_running(app: &Application) -> bool {
    app.editor.terminal.shell.is_some()
}

/// Opens the panel and waits until the shell has drawn its prompt.
async fn open_panel(h: &mut Harness) -> anyhow::Result<()> {
    h.keys("<C-t>").await?;
    h.until("the shell's prompt", has_prompt).await;
    Ok(())
}

/// Reads the shim's socket path out of the shell's environment.
async fn socket_path(h: &mut Harness, dir: &Path) -> anyhow::Result<PathBuf> {
    let out = dir.join("socket");
    h.keys(&format!(
        "printenv HELIX_TERMINAL_SOCKET <gt> {}<ret>",
        out.display()
    ))
    .await?;
    let read = || std::fs::read_to_string(&out).unwrap_or_default();
    let start = Instant::now();
    while !read().ends_with('\n') {
        assert!(start.elapsed() < DEADLINE, "no socket path written");
        h.step(Duration::from_millis(50)).await;
    }
    Ok(PathBuf::from(read().trim()))
}

async fn send(socket: PathBuf, request: Request) -> tokio::task::JoinHandle<io::Result<Response>> {
    tokio::task::spawn_blocking(move || shim::send(&socket, &request))
}

fn in_editor(app: &Application) -> bool {
    !app.editor.terminal.is_focused()
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_t_opens_and_toggles_focus() -> anyhow::Result<()> {
    let mut h = Harness::sh()?;
    assert!(!h.app.editor.terminal.is_visible());

    h.keys("<C-t>").await?;
    let panel = &h.app.editor.terminal;
    assert!(panel.shell.is_some());
    assert!(panel.is_visible() && panel.is_focused());
    h.until("the shell's prompt", has_prompt).await;

    h.keys("<C-t>").await?;
    let panel = &h.app.editor.terminal;
    assert!(panel.is_visible(), "stays visible when unfocused");
    assert!(!panel.is_focused());
    assert!(panel.shell.is_some());

    // and focuses it again, keeping the same shell
    let pid = panel.shell.as_ref().unwrap().pid();
    h.keys("<C-t>").await?;
    assert!(h.app.editor.terminal.is_focused());
    assert_eq!(h.app.editor.terminal.shell.as_ref().unwrap().pid(), pid);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_t_in_insert_mode() -> anyhow::Result<()> {
    let mut h = Harness::sh()?;
    h.keys("ifoo").await?;
    h.keys("<C-t>").await?;
    assert!(h.app.editor.terminal.is_focused());
    assert_eq!(h.app.editor.mode(), helix_view::document::Mode::Insert);

    // keys go to the shell; the editor is still in insert mode when the focus comes back
    h.keys("<C-t>").await?;
    assert!(in_editor(&h.app));
    assert!(h.app.editor.terminal.is_visible());
    assert_eq!(h.app.editor.mode(), helix_view::document::Mode::Insert);
    h.keys("bar").await?;
    assert_eq!(doc!(h.app.editor).text().to_string(), "foobar\n");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn focused_keys_go_to_the_shell() -> anyhow::Result<()> {
    let mut h = Harness::sh()?;
    open_panel(&mut h).await?;
    let before = doc!(h.app.editor).text().to_string();

    // `i` would enter insert mode, `dd` and `x` would change the document
    h.keys("ixdd typed-text").await?;
    h.until("the typed text on the screen", |app| {
        screen_contains(app, "ixdd typed-text")
    })
    .await;
    assert_eq!(doc!(h.app.editor).text().to_string(), before);
    assert_eq!(h.app.editor.mode(), helix_view::document::Mode::Normal);

    // Esc goes to the shell too (it doesn't hide the panel) while focused
    h.keys("<esc>").await?;
    assert!(h.app.editor.terminal.is_focused());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_hides_an_unfocused_panel() -> anyhow::Result<()> {
    let mut h = Harness::sh()?;

    // hidden: nothing special, no shell started
    h.keys("<esc>").await?;
    assert!(!h.app.editor.terminal.is_visible());
    assert!(h.app.editor.terminal.shell.is_none());

    open_panel(&mut h).await?;
    h.keys("<C-t>").await?;
    assert!(h.app.editor.terminal.is_visible());

    // a pending key: Esc cancels it, the panel stays
    h.keys("g<esc>").await?;
    assert!(h.app.editor.terminal.is_visible());
    // and a count
    h.keys("2<esc>").await?;
    assert!(h.app.editor.terminal.is_visible());

    // nothing pending: hides, the shell keeps running
    h.keys("<esc>").await?;
    let panel = &h.app.editor.terminal;
    assert!(!panel.is_visible() && !panel.is_focused());
    assert!(panel.shell.is_some());

    // hidden: Esc does nothing more, and C-t shows the same shell again
    h.keys("<esc>").await?;
    assert!(h.app.editor.terminal.shell.is_some());
    h.keys("<C-t>").await?;
    assert!(h.app.editor.terminal.is_focused());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_in_insert_mode_keeps_the_panel() -> anyhow::Result<()> {
    let mut h = Harness::sh()?;
    open_panel(&mut h).await?;
    h.keys("<C-t>i<esc>").await?;
    // the Esc left insert mode; it didn't hide the panel
    assert!(h.app.editor.terminal.is_visible());
    assert_eq!(h.app.editor.mode(), helix_view::document::Mode::Normal);
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn sh_command_runs_in_the_panel() -> anyhow::Result<()> {
    let mut h = Harness::sh()?;
    h.keys(":sh echo hi-$((1+1))<ret>").await?;
    assert!(h.app.editor.terminal.is_visible());
    assert!(h.app.editor.terminal.is_focused());
    h.until("the output", |app| {
        screen(app).lines().any(|line| line.trim() == "hi-2")
    })
    .await;

    // the long name, and no command: just focuses
    h.keys("<C-t>").await?;
    assert!(!h.app.editor.terminal.is_focused());
    h.keys(":run-shell-command<ret>").await?;
    assert!(h.app.editor.terminal.is_focused());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_closes_the_panel() -> anyhow::Result<()> {
    let mut h = Harness::sh()?;
    open_panel(&mut h).await?;
    h.keys("exit<ret>").await?;
    h.until("the panel to close", |app| !shell_running(app))
        .await;
    let panel = &h.app.editor.terminal;
    assert!(!panel.visible && !panel.focused);
    assert!(!panel.is_visible() && !panel.is_focused());

    // a fresh shell
    h.keys("<C-t>").await?;
    let panel = &h.app.editor.terminal;
    assert!(panel.shell.is_some() && panel.is_focused());
    h.until("the new prompt", has_prompt).await;
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn sh_exit_closes_the_panel() -> anyhow::Result<()> {
    let mut h = Harness::sh()?;
    h.keys(":sh exit<ret>").await?;
    h.until("the panel to close", |app| !shell_running(app))
        .await;
    assert!(!h.app.editor.terminal.visible);
    assert!(!h.app.editor.terminal.focused);

    h.keys(":sh echo again<ret>").await?;
    assert!(h.app.editor.terminal.is_focused());
    h.until("the output of the new shell", |app| {
        screen(app).lines().any(|line| line.trim() == "again")
    })
    .await;
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn shim_focus_unfocuses_the_panel() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut h = Harness::sh()?;
    open_panel(&mut h).await?;
    let socket = socket_path(&mut h, dir.path()).await?;
    assert!(h.app.editor.terminal.is_focused());

    let call = send(socket, Request::Focus).await;
    let response = h.await_blocking("the answer to Focus", call).await?;
    assert_eq!(response, Response::Done);
    assert!(!h.app.editor.terminal.is_focused());
    assert!(h.app.editor.terminal.is_visible());
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn shim_open_goes_to_line_and_column() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "one\ntwo three\nfour\n")?;
    let mut h = Harness::sh()?;
    open_panel(&mut h).await?;
    let socket = socket_path(&mut h, dir.path()).await?;

    let request = Request::Open {
        files: vec![OpenFile {
            path: file.clone(),
            line: Some(2),
            column: Some(3),
        }],
        wait: false,
    };
    let call = send(socket, request).await;
    let response = h.await_blocking("the answer to Open", call).await?;
    assert_eq!(response, Response::Done);

    let editor = &h.app.editor;
    assert!(!editor.terminal.is_focused());
    assert!(editor.terminal.is_visible());
    let (view, document) = current_ref!(editor);
    assert_eq!(
        document.path().map(|p| p.canonicalize().unwrap()),
        Some(file.canonicalize()?)
    );
    let text = document.text().slice(..);
    let cursor = document.selection(view.id).primary().cursor(text);
    let line = text.char_to_line(cursor);
    assert_eq!((line, cursor - text.line_to_char(line)), (1, 2));
    assert!(editor.terminal.waiters.is_empty());
    h.finish().await
}

/// `hx --wait`: the client is answered when the buffer is closed.
async fn open_and_wait(
    h: &mut Harness,
    dir: &Path,
) -> anyhow::Result<(PathBuf, tokio::task::JoinHandle<io::Result<Response>>)> {
    let file = dir.join("msg.txt");
    std::fs::write(&file, "message\n")?;
    let socket = socket_path(h, dir).await?;
    let request = Request::Open {
        files: vec![OpenFile {
            path: file.clone(),
            line: None,
            column: None,
        }],
        wait: true,
    };
    let call = send(socket, request).await;
    h.until("the file to open", |app| {
        app.editor.terminal.waiters.len() == 1
    })
    .await;
    // nothing answers while the buffer is open
    h.pump_for(Duration::from_millis(300)).await;
    assert!(!call.is_finished(), "answered before the buffer closed");
    assert!(h.app.editor.terminal.is_visible());
    assert!(!h.app.editor.terminal.is_focused());
    assert!(doc!(h.app.editor).path().is_some());
    Ok((file, call))
}

#[tokio::test(flavor = "multi_thread")]
async fn shim_wait_answers_when_the_buffer_closes() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut h = Harness::sh()?;
    open_panel(&mut h).await?;
    let (file, call) = open_and_wait(&mut h, dir.path()).await?;

    h.keys(":q<ret>").await?;
    let response = h.await_blocking("the answer after :q", call).await?;
    assert_eq!(response, Response::Done);
    assert!(h.running, "`:q` on the awaited buffer quit the editor");
    assert!(h.app.editor.terminal.is_focused(), "back in the panel");
    assert!(h.app.editor.terminal.waiters.is_empty());
    assert!(h.app.editor.documents().all(|d| d.path() != Some(&file)));
    assert_eq!(std::fs::read_to_string(&file)?, "message\n");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn shim_wait_answers_after_wq() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut h = Harness::sh()?;
    open_panel(&mut h).await?;
    let (file, call) = open_and_wait(&mut h, dir.path()).await?;

    h.keys("A!<esc>:wq<ret>").await?;
    let response = h.await_blocking("the answer after :wq", call).await?;
    assert_eq!(response, Response::Done);
    assert!(h.running);
    assert!(h.app.editor.terminal.is_focused());
    assert_eq!(std::fs::read_to_string(&file)?, "message!\n");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn shim_wait_answers_after_force_quit() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut h = Harness::sh()?;
    open_panel(&mut h).await?;
    let (file, call) = open_and_wait(&mut h, dir.path()).await?;

    h.keys("A!<esc>:q!<ret>").await?;
    let response = h.await_blocking("the answer after :q!", call).await?;
    assert_eq!(response, Response::Done);
    assert!(h.running);
    assert!(h.app.editor.terminal.is_focused());
    assert_eq!(std::fs::read_to_string(&file)?, "message\n");
    h.finish().await
}

#[tokio::test(flavor = "multi_thread")]
async fn status_of_the_last_command() -> anyhow::Result<()> {
    if std::process::Command::new("bash")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("bash is missing, skipping");
        return Ok(());
    }
    let mut h = Harness::with_shell(&["bash"])?;
    h.keys("<C-t>").await?;
    h.until("the prompt", |app| {
        app.editor.terminal.shell.as_ref().is_some_and(|shell| {
            let mut any = false;
            shell.visit_cells(|_, _, cell| any |= !cell.text.trim().is_empty());
            any
        })
    })
    .await;

    h.keys("false<ret>").await?;
    h.keys("<C-t>").await?;
    assert!(!h.app.editor.terminal.is_focused());
    h.until("the status to be Finished(1)", |app| {
        app.editor.terminal.shell.as_ref().map(|s| s.status()) == Some(ShellStatus::Finished(1))
    })
    .await;

    // seeing the panel again acknowledges the result
    h.keys("<C-t>").await?;
    assert_eq!(
        h.app.editor.terminal.shell.as_ref().unwrap().status(),
        ShellStatus::Idle
    );
    h.finish().await
}

async fn popup_command(command: &str) -> anyhow::Result<()> {
    let mut h = Harness::sh()?;
    h.keys(command).await?;
    h.until(
        "the command to run",
        |app| matches!(app.editor.get_status(), Some((msg, _)) if msg.as_ref() == "Command run"),
    )
    .await;
    let panel = &h.app.editor.terminal;
    assert!(panel.shell.is_none(), "{command} started the shell");
    assert!(!panel.is_visible() && !panel.is_focused());
    h.finish().await
}

/// `:!` and `:shell-popup` keep their old behaviour: a popup, not the panel.
#[tokio::test(flavor = "multi_thread")]
async fn bang_without_space_is_a_popup() -> anyhow::Result<()> {
    popup_command(":!echo popup<ret>").await
}

#[tokio::test(flavor = "multi_thread")]
async fn bang_with_space_is_a_popup() -> anyhow::Result<()> {
    popup_command(":! echo popup<ret>").await
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_popup_is_a_popup() -> anyhow::Result<()> {
    popup_command(":shell-popup echo popup<ret>").await
}
