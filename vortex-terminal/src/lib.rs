//! The shell of Vortex's terminal panel: one shell process on a pseudo-terminal
//! (`portable-pty`), the terminal emulator its output goes through (`alacritty_terminal`), and
//! the shell integration that reports when a command starts and how it ended (OSC 133).
//!
//! This crate knows nothing of the editor: it takes bytes to write, a size, and gives back the
//! screen to draw, the input modes the program asked for, the [`ShellStatus`], and
//! [`ShellEvent`]s on a channel. Drawing, keys and focus are `vortex-term`'s.

pub mod osc133;
mod shell;
#[cfg(test)]
mod tests;

pub use shell::Shell;

use std::path::PathBuf;

/// How to start the shell.
#[derive(Debug, Clone)]
pub struct ShellOptions {
    /// The program and its arguments. Empty: `$SHELL`, or `/bin/sh`.
    pub command: Vec<String>,
    /// The working directory of the shell.
    pub cwd: Option<PathBuf>,
    /// Variables to set, on top of Vortex's own environment.
    pub env: Vec<(String, String)>,
    /// A directory to put first on `PATH`.
    pub path_prefix: Option<PathBuf>,
    /// Inject the shell integration (OSC 133 markers) into bash and zsh, without touching the
    /// user's rc files. Other shells fall back to watching the foreground process group.
    pub integration: bool,
    /// The size of the screen in cells.
    pub size: Size,
    /// Lines of scrollback kept above the screen.
    pub scrollback: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

/// What the shell is doing, for the statusline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellStatus {
    /// At the prompt, or no command has been seen yet.
    Idle,
    /// A command is running.
    Running,
    /// The last command finished with this exit code; stays until [`Shell::acknowledge`] or the
    /// next command.
    Finished(i32),
}

/// Sent by the shell's threads on the channel given to [`Shell::spawn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// New output was processed: the screen changed. Coalesced: another one is sent only after
    /// [`Shell::take_output_pending`] is called.
    Output,
    /// [`Shell::status`] changed.
    Status(ShellStatus),
    /// The program set the window title.
    Title(String),
    /// The shell process exited (with its exit code, when known). The shell is dead; drop it.
    Exited(Option<i32>),
}

/// The modes the program in the terminal asked for, which decide how keys, the mouse and pastes
/// are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputModes {
    /// DECCKM: arrows send `ESC O A` instead of `ESC [ A`.
    pub app_cursor: bool,
    /// DECKPAM: the keypad is in application mode.
    pub app_keypad: bool,
    /// Pastes are wrapped in `ESC [ 200 ~` ... `ESC [ 201 ~`.
    pub bracketed_paste: bool,
    /// Which mouse events the program wants reported.
    pub mouse: MouseTracking,
    /// Mouse reports in SGR encoding (`ESC [ < b ; x ; y M`).
    pub sgr_mouse: bool,
    /// Mouse reports in UTF-8 encoding.
    pub utf8_mouse: bool,
    /// The alternate screen is shown (a full screen program runs).
    pub alt_screen: bool,
    /// In the alternate screen without mouse reporting, the wheel sends arrow keys.
    pub alternate_scroll: bool,
    /// Focus changes are reported with `ESC [ I` and `ESC [ O`.
    pub focus_reporting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    #[default]
    None,
    /// Presses and releases (1000).
    Click,
    /// Also motion while a button is held (1002).
    Drag,
    /// All motion (1003).
    Motion,
}

/// A color as the program set it, so that the outer terminal's palette applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// One of the 256 indexed colors; 0-15 are the named ANSI colors.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// One cell of the screen, as drawn.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// The character, with any zero width characters that follow it. Empty for the spacer
    /// cell after a wide character, which isn't drawn.
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    /// Foreground and background are swapped when drawn.
    pub inverse: bool,
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Beam,
}

/// Where the program shows the cursor, in screen cells from the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub shape: CursorShape,
}
