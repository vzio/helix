use tokio::sync::mpsc::UnboundedSender;

use crate::{Cell, Cursor, InputModes, ShellEvent, ShellOptions, ShellStatus, Size};

/// The running shell: its process on a pseudo-terminal, and the emulator its output goes
/// through. Dropping it kills the process.
pub struct Shell {}

impl Shell {
    /// Starts the shell. Its threads send [`ShellEvent`]s on `events`; they never block on it.
    pub fn spawn(
        options: ShellOptions,
        events: UnboundedSender<ShellEvent>,
    ) -> anyhow::Result<Shell> {
        let _ = (options, events);
        todo!()
    }

    /// Writes bytes to the shell, as if typed. Snaps the view back to the bottom of the
    /// scrollback.
    pub fn write(&self, bytes: &[u8]) {
        let _ = bytes;
        todo!()
    }

    /// Resizes the screen and the pseudo-terminal (the program gets `SIGWINCH`). Does nothing
    /// when the size is the same.
    pub fn resize(&self, size: Size) {
        let _ = size;
        todo!()
    }

    pub fn size(&self) -> Size {
        todo!()
    }

    pub fn status(&self) -> ShellStatus {
        todo!()
    }

    /// The user has seen the result: [`ShellStatus::Finished`] goes back to
    /// [`ShellStatus::Idle`].
    pub fn acknowledge(&self) {
        todo!()
    }

    pub fn input_modes(&self) -> InputModes {
        todo!()
    }

    /// Scrolls the view of the scrollback by `lines`, up when positive. Only on the primary
    /// screen.
    pub fn scroll(&self, lines: i32) {
        let _ = lines;
        todo!()
    }

    /// How many lines the view is scrolled up from the bottom.
    pub fn scroll_offset(&self) -> usize {
        todo!()
    }

    /// Calls `f` with each cell of the screen as shown (with the scrollback offset applied),
    /// row by row, with its row and column.
    pub fn visit_cells(&self, f: impl FnMut(u16, u16, &Cell)) {
        let _ = f;
        todo!()
    }

    /// The cursor, when the program shows it and the view isn't scrolled up.
    pub fn cursor(&self) -> Option<Cursor> {
        todo!()
    }

    /// The title the program set, if any.
    pub fn title(&self) -> Option<String> {
        todo!()
    }

    /// Clears the flag that coalesces [`ShellEvent::Output`], so that the next output sends one.
    pub fn take_output_pending(&self) {
        todo!()
    }

    /// The process id of the shell.
    pub fn pid(&self) -> Option<u32> {
        todo!()
    }
}
