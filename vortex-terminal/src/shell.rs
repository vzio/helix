use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape as AnsiCursorShape, NamedColor, Processor, Rgb,
};
use anyhow::Context;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc::UnboundedSender;

use crate::osc133::{Marker, Scanner};
use crate::{
    Cell, Color, Cursor, CursorShape, InputModes, MouseTracking, ShellEvent, ShellOptions,
    ShellStatus, Size,
};

/// How long input is held at most, waiting for the shell to be ready.
const READY_TIMEOUT: Duration = Duration::from_secs(3);

/// How often the foreground process group is looked at, without shell integration.
const FALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// How long the exit of the shell waits for the reader to drain the last output.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// The smallest screen: the emulator doesn't work with less.
const MIN_SIZE: u16 = 2;

/// State shared between the [`Shell`], its threads and the emulator's listener. It doesn't
/// refer to any of them, so that nothing is kept alive in a cycle.
struct Common {
    events: UnboundedSender<ShellEvent>,
    writer: Mutex<Box<dyn Write + Send>>,
    size: Mutex<Size>,
    title: Mutex<Option<String>>,
    status: Mutex<ShellStatus>,
    /// An OSC 133 marker was seen: the shell has the integration.
    integrated: AtomicBool,
    /// An [`ShellEvent::Output`] was sent and [`Shell::take_output_pending`] wasn't called yet.
    output_pending: AtomicBool,
    /// The shell process is gone.
    exited: AtomicBool,
    /// What was typed before the shell was ready to read it, or None once it is: bash and zsh
    /// drop what is typed while they start (`:sh command` on a new shell).
    held_input: Mutex<Option<Vec<u8>>>,
    /// The shell is ready at its first prompt marker (with the integration), else at its first
    /// output.
    ready_at_marker: bool,
}

impl Common {
    fn send(&self, event: ShellEvent) {
        // The receiver may be gone: the editor is closing.
        let _ = self.events.send(event);
    }

    fn write(&self, bytes: &[u8]) {
        let mut writer = self.writer.lock();
        if let Err(err) = writer.write_all(bytes).and_then(|()| writer.flush()) {
            log::debug!("writing to the terminal failed: {err}");
        }
    }

    /// Writes what the user typed, or holds it until the shell is ready.
    fn write_input(&self, bytes: &[u8]) {
        let mut held = self.held_input.lock();
        match &mut *held {
            Some(held) => held.extend_from_slice(bytes),
            None => {
                drop(held);
                self.write(bytes);
            }
        }
    }

    /// The shell is ready: writes what was typed before.
    fn ready(&self) {
        let held = self.held_input.lock().take();
        if let Some(bytes) = held.filter(|bytes| !bytes.is_empty()) {
            self.write(&bytes);
        }
    }

    /// Changes the status with `f`, which gets the current one and returns the new one if it
    /// changes. Sends the event.
    fn update_status(&self, f: impl FnOnce(ShellStatus) -> Option<ShellStatus>) {
        let mut status = self.status.lock();
        if let Some(new) = f(*status) {
            if new != *status {
                *status = new;
                self.send(ShellEvent::Status(new));
            }
        }
    }

    fn handle_marker(&self, marker: Marker) {
        self.integrated.store(true, Ordering::SeqCst);
        self.update_status(|status| match (marker, status) {
            (Marker::CommandExecuted, _) => Some(ShellStatus::Running),
            // The shell may send `D` without a command having run (bash on an empty line).
            (Marker::CommandFinished(code), ShellStatus::Running) => {
                Some(ShellStatus::Finished(code.unwrap_or(0)))
            }
            // The prompt is back but no `D` came: the shell doesn't report exit codes.
            (Marker::PromptStart, ShellStatus::Running) => Some(ShellStatus::Idle),
            _ => None,
        });
    }
}

/// Receives the events of the emulator.
struct Listener(Arc<Common>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            // The program queried the terminal (device attributes, cursor position...).
            Event::PtyWrite(text) => self.0.write(text.as_bytes()),
            Event::TextAreaSizeRequest(format) => {
                let size = *self.0.size.lock();
                let text = format(WindowSize {
                    num_lines: size.rows,
                    num_cols: size.cols,
                    // The size of a cell in pixels isn't known here.
                    cell_width: 8,
                    cell_height: 16,
                });
                self.0.write(text.as_bytes());
            }
            Event::ColorRequest(index, format) => {
                self.0.write(format(default_color(index)).as_bytes());
            }
            Event::Title(title) => {
                *self.0.title.lock() = Some(title.clone());
                self.0.send(ShellEvent::Title(title));
            }
            Event::ResetTitle => {
                *self.0.title.lock() = None;
                self.0.send(ShellEvent::Title(String::new()));
            }
            _ => {}
        }
    }
}

/// The colors told to programs that ask for them (OSC 4, 10, 11): the real ones are the outer
/// terminal's, which are not known here.
fn default_color(index: usize) -> Rgb {
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    let (r, g, b) = match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let level = |n: usize| if n == 0 { 0 } else { (55 + 40 * n) as u8 };
            let i = index - 16;
            (level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        232..=255 => {
            let v = (8 + 10 * (index - 232)) as u8;
            (v, v, v)
        }
        // Background.
        257 => (0x00, 0x00, 0x00),
        // Foreground, cursor and the rest.
        _ => (0xe5, 0xe5, 0xe5),
    };
    Rgb { r, g, b }
}

struct TermSize {
    rows: usize,
    cols: usize,
}

impl TermSize {
    fn new(size: Size) -> Self {
        Self {
            rows: size.rows as usize,
            cols: size.cols as usize,
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

type SharedTerm = Arc<Mutex<Term<Listener>>>;

/// The running shell: its process on a pseudo-terminal, and the emulator its output goes
/// through. Dropping it kills the process.
pub struct Shell {
    common: Arc<Common>,
    term: SharedTerm,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    /// Holds the integration scripts; removed when the shell is dropped.
    _integration_dir: Option<tempfile::TempDir>,
}

/// What the integration changes in the way the shell is started.
#[derive(Default)]
struct Integration {
    args: Vec<OsString>,
    env: Vec<(&'static str, OsString)>,
    dir: Option<tempfile::TempDir>,
}

const BASHRC: &str = include_str!("integration/bashrc");
const ZSHENV: &str = include_str!("integration/zshenv");
const ZPROFILE: &str = include_str!("integration/zprofile");
const ZSHRC: &str = include_str!("integration/zshrc");
const ZLOGIN: &str = include_str!("integration/zlogin");

/// Writes the integration scripts for `program` when it is a shell that has one, and says how
/// to start it with them.
fn prepare_integration(
    program: &str,
    extra_args: &[String],
    options: &ShellOptions,
) -> anyhow::Result<Integration> {
    if !options.integration || !extra_args.is_empty() {
        return Ok(Integration::default());
    }
    let name = Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = || {
        tempfile::Builder::new()
            .prefix("vortex-shell-")
            .tempdir()
            .context("creating a directory for the shell integration")
    };
    match name.as_str() {
        "bash" => {
            let dir = dir()?;
            let rc = dir.path().join("bashrc");
            std::fs::write(&rc, BASHRC)?;
            Ok(Integration {
                args: vec!["--init-file".into(), rc.into_os_string(), "-i".into()],
                env: Vec::new(),
                dir: Some(dir),
            })
        }
        "zsh" => {
            let dir = dir()?;
            for (file, content) in [
                (".zshenv", ZSHENV),
                (".zprofile", ZPROFILE),
                (".zshrc", ZSHRC),
                (".zlogin", ZLOGIN),
            ] {
                std::fs::write(dir.path().join(file), content)?;
            }
            let ours = dir.path().as_os_str().to_owned();
            let mut env = vec![("ZDOTDIR", ours.clone()), ("VORTEX_ZDOTDIR", ours)];
            // The user's own ZDOTDIR, if they have one: the scripts read their files from it.
            let user_zdotdir = options
                .env
                .iter()
                .rev()
                .find(|(key, _)| key == "ZDOTDIR")
                .map(|(_, value)| OsString::from(value))
                .or_else(|| std::env::var_os("ZDOTDIR"));
            if let Some(zdotdir) = user_zdotdir {
                env.push(("VORTEX_USER_ZDOTDIR", zdotdir));
            }
            Ok(Integration {
                args: Vec::new(),
                env,
                dir: Some(dir),
            })
        }
        _ => Ok(Integration::default()),
    }
}

fn default_program() -> String {
    match std::env::var("SHELL") {
        Ok(shell) if !shell.is_empty() => shell,
        _ => "/bin/sh".into(),
    }
}

fn clamp_size(size: Size) -> Size {
    Size {
        rows: size.rows.max(MIN_SIZE),
        cols: size.cols.max(MIN_SIZE),
    }
}

fn pty_size(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl Shell {
    /// Starts the shell. Its threads send [`ShellEvent`]s on `events`; they never block on it.
    pub fn spawn(
        options: ShellOptions,
        events: UnboundedSender<ShellEvent>,
    ) -> anyhow::Result<Shell> {
        let size = clamp_size(options.size);
        let pair = native_pty_system()
            .openpty(pty_size(size))
            .context("opening a pseudo-terminal")?;

        let program = options
            .command
            .first()
            .cloned()
            .unwrap_or_else(default_program);
        let extra_args = options.command.get(1..).unwrap_or_default();
        let integration = prepare_integration(&program, extra_args, &options)?;

        let mut cmd = CommandBuilder::new(&program);
        cmd.args(extra_args);
        cmd.args(&integration.args);
        if let Some(cwd) = &options.cwd {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (key, value) in &options.env {
            cmd.env(key, value);
        }
        for (key, value) in &integration.env {
            cmd.env(key, value);
        }
        if let Some(prefix) = &options.path_prefix {
            let existing = options
                .env
                .iter()
                .rev()
                .find(|(key, _)| key == "PATH")
                .map(|(_, value)| OsString::from(value))
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default();
            let paths = std::iter::once(prefix.clone()).chain(std::env::split_paths(&existing));
            match std::env::join_paths(paths) {
                Ok(path) => cmd.env("PATH", path),
                Err(err) => log::warn!("not adding {} to PATH: {err}", prefix.display()),
            }
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("starting {program}"))?;
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();

        let master = pair.master;
        let reader = master
            .try_clone_reader()
            .context("reading from the pseudo-terminal")?;
        let writer = master
            .take_writer()
            .context("writing to the pseudo-terminal")?;

        let common = Arc::new(Common {
            events,
            writer: Mutex::new(writer),
            size: Mutex::new(size),
            title: Mutex::new(None),
            status: Mutex::new(ShellStatus::Idle),
            integrated: AtomicBool::new(false),
            output_pending: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            held_input: Mutex::new(Some(Vec::new())),
            ready_at_marker: integration.dir.is_some(),
        });
        let config = Config {
            scrolling_history: options.scrollback,
            ..Config::default()
        };
        let term = Arc::new(Mutex::new(Term::new(
            config,
            &TermSize::new(size),
            Listener(common.clone()),
        )));
        let master = Arc::new(Mutex::new(master));

        let (drained_tx, drained_rx) = mpsc::channel::<()>();
        {
            let common = common.clone();
            let term = term.clone();
            spawn_thread("vortex-terminal-reader", move || {
                read_loop(reader, &common, &term);
                let _ = drained_tx.send(());
            })?;
        }
        {
            let common = common.clone();
            spawn_thread("vortex-terminal-wait", move || {
                let code = match child.wait() {
                    Ok(status) => Some(status.exit_code() as i32),
                    Err(err) => {
                        log::warn!("waiting for the shell failed: {err}");
                        None
                    }
                };
                // Let the reader finish the last output first; a background process that
                // keeps the terminal open must not hold the exit back.
                let _ = drained_rx.recv_timeout(DRAIN_TIMEOUT);
                common.exited.store(true, Ordering::SeqCst);
                common.send(ShellEvent::Exited(code));
            })?;
        }
        {
            // in case the shell never sends a marker (an rc file that replaces the prompt hook)
            let common = common.clone();
            spawn_thread("vortex-terminal-ready", move || {
                std::thread::sleep(READY_TIMEOUT);
                common.ready();
            })?;
        }
        if let Some(pid) = pid {
            let common = common.clone();
            let master = master.clone();
            spawn_thread("vortex-terminal-poll", move || {
                poll_foreground(&common, &master, pid)
            })?;
        }

        Ok(Shell {
            common,
            term,
            master,
            killer: Mutex::new(killer),
            pid,
            _integration_dir: integration.dir,
        })
    }

    /// Writes bytes to the shell, as if typed. Snaps the view back to the bottom of the
    /// scrollback.
    pub fn write(&self, bytes: &[u8]) {
        self.term.lock().scroll_display(Scroll::Bottom);
        self.common.write_input(bytes);
    }

    /// Resizes the screen and the pseudo-terminal (the program gets `SIGWINCH`). Does nothing
    /// when the size is the same. Sizes under 2 rows or columns are raised to 2.
    pub fn resize(&self, size: Size) {
        let size = clamp_size(size);
        let mut current = self.common.size.lock();
        if *current == size {
            return;
        }
        *current = size;
        if let Err(err) = self.master.lock().resize(pty_size(size)) {
            log::warn!("resizing the pseudo-terminal failed: {err}");
        }
        self.term.lock().resize(TermSize::new(size));
    }

    pub fn size(&self) -> Size {
        *self.common.size.lock()
    }

    pub fn status(&self) -> ShellStatus {
        *self.common.status.lock()
    }

    /// The user has seen the result: [`ShellStatus::Finished`] goes back to
    /// [`ShellStatus::Idle`].
    pub fn acknowledge(&self) {
        self.common.update_status(|status| match status {
            ShellStatus::Finished(_) => Some(ShellStatus::Idle),
            _ => None,
        });
    }

    pub fn input_modes(&self) -> InputModes {
        let mode = *self.term.lock().mode();
        InputModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse: if mode.contains(TermMode::MOUSE_MOTION) {
                MouseTracking::Motion
            } else if mode.contains(TermMode::MOUSE_DRAG) {
                MouseTracking::Drag
            } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
                MouseTracking::Click
            } else {
                MouseTracking::None
            },
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            utf8_mouse: mode.contains(TermMode::UTF8_MOUSE),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
        }
    }

    /// Scrolls the view of the scrollback by `lines`, up when positive. Only on the primary
    /// screen.
    pub fn scroll(&self, lines: i32) {
        let mut term = self.term.lock();
        if !term.mode().contains(TermMode::ALT_SCREEN) {
            term.scroll_display(Scroll::Delta(lines));
        }
    }

    /// How many lines the view is scrolled up from the bottom.
    pub fn scroll_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }

    /// Calls `f` with each cell of the screen as shown (with the scrollback offset applied),
    /// row by row, with its row and column.
    pub fn visit_cells(&self, mut f: impl FnMut(u16, u16, &Cell)) {
        let term = self.term.lock();
        let grid = term.grid();
        let offset = grid.display_offset() as i32;
        let mut cell = Cell::default();
        for row in 0..grid.screen_lines() {
            let line = Line(row as i32 - offset);
            for col in 0..grid.columns() {
                convert_cell(&grid[Point::new(line, Column(col))], &mut cell);
                f(row as u16, col as u16, &cell);
            }
        }
    }

    /// The cursor, when the program shows it and the view isn't scrolled up.
    pub fn cursor(&self) -> Option<Cursor> {
        let term = self.term.lock();
        if !term.mode().contains(TermMode::SHOW_CURSOR) || term.grid().display_offset() != 0 {
            return None;
        }
        let shape = match term.cursor_style().shape {
            AnsiCursorShape::Block | AnsiCursorShape::HollowBlock => CursorShape::Block,
            AnsiCursorShape::Underline => CursorShape::Underline,
            AnsiCursorShape::Beam => CursorShape::Beam,
            AnsiCursorShape::Hidden => return None,
        };
        let point = term.grid().cursor.point;
        Some(Cursor {
            row: point.line.0.max(0) as u16,
            col: point.column.0 as u16,
            shape,
        })
    }

    /// The title the program set, if any.
    pub fn title(&self) -> Option<String> {
        self.common.title.lock().clone()
    }

    /// Clears the flag that coalesces [`ShellEvent::Output`], so that the next output sends one.
    pub fn take_output_pending(&self) {
        self.common.output_pending.store(false, Ordering::SeqCst);
    }

    /// The process id of the shell.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        // The threads end when the process is gone: the reader at the end of the output, the
        // others when it has exited.
        if let Err(err) = self.killer.lock().kill() {
            log::debug!("killing the shell failed: {err}");
        }
    }
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) -> anyhow::Result<()> {
    thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .with_context(|| format!("starting the {name} thread"))?;
    Ok(())
}

/// Reads the output of the shell until it ends: markers first, then the emulator.
fn read_loop(mut reader: Box<dyn Read + Send>, common: &Common, term: &SharedTerm) {
    let mut scanner = Scanner::new();
    let mut processor: Processor = Processor::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                // Linux reports the end of the output as EIO.
                log::debug!("reading from the terminal ended: {err}");
                break;
            }
        };
        let chunk = &buf[..n];
        for marker in scanner.feed(chunk) {
            common.handle_marker(marker);
            if marker == Marker::PromptStart {
                common.ready();
            }
        }
        processor.advance(&mut *term.lock(), chunk);
        if !common.ready_at_marker {
            common.ready();
        }
        if !common.output_pending.swap(true, Ordering::SeqCst) {
            common.send(ShellEvent::Output);
        }
    }
}

/// For shells without integration: looks at whether the shell or a command it started is in the
/// foreground of the terminal, until a marker shows up or the shell exits.
fn poll_foreground(common: &Common, master: &Mutex<Box<dyn MasterPty + Send>>, pid: u32) {
    while !common.integrated.load(Ordering::SeqCst) && !common.exited.load(Ordering::SeqCst) {
        let leader = master.lock().process_group_leader();
        if let Some(leader) = leader {
            let running = i64::from(leader) != i64::from(pid);
            common.update_status(|status| {
                if common.integrated.load(Ordering::SeqCst) {
                    return None;
                }
                match (running, status) {
                    (true, ShellStatus::Idle | ShellStatus::Finished(_)) => {
                        Some(ShellStatus::Running)
                    }
                    (false, ShellStatus::Running) => Some(ShellStatus::Idle),
                    _ => None,
                }
            });
        }
        thread::sleep(FALLBACK_POLL_INTERVAL);
    }
}

fn convert_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Named(named) => {
            let index = named as usize;
            match named {
                NamedColor::DimBlack
                | NamedColor::DimRed
                | NamedColor::DimGreen
                | NamedColor::DimYellow
                | NamedColor::DimBlue
                | NamedColor::DimMagenta
                | NamedColor::DimCyan
                | NamedColor::DimWhite => {
                    Color::Indexed((index - NamedColor::DimBlack as usize) as u8)
                }
                _ if index < 16 => Color::Indexed(index as u8),
                _ => Color::Default,
            }
        }
        AnsiColor::Spec(Rgb { r, g, b }) => Color::Rgb(r, g, b),
        AnsiColor::Indexed(i) => Color::Indexed(i),
    }
}

/// Fills `out` with the cell, reusing its text buffer.
fn convert_cell(cell: &alacritty_terminal::term::cell::Cell, out: &mut Cell) {
    let flags = cell.flags;
    out.text.clear();
    if !flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
        out.text.push(cell.c);
        if let Some(zerowidth) = cell.zerowidth() {
            out.text.extend(zerowidth);
        }
    }
    out.fg = convert_color(cell.fg);
    out.bg = convert_color(cell.bg);
    out.bold = flags.contains(Flags::BOLD);
    out.dim = flags.contains(Flags::DIM);
    out.italic = flags.contains(Flags::ITALIC);
    out.underline = flags.intersects(Flags::ALL_UNDERLINES);
    out.strikeout = flags.contains(Flags::STRIKEOUT);
    out.inverse = flags.contains(Flags::INVERSE);
    out.hidden = flags.contains(Flags::HIDDEN);
}
