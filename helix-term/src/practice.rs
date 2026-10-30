//! `:practice`, a built-in trainer: exercises teach a feature first, then count the keystrokes so
//! they can be solved again with fewer.
//!
//! The exercises are data files in `runtime/practice` (see [`exercise`] for the format). The state
//! of the running attempt lives in a per-thread session ([`session`]), which counts the key events
//! the application receives from the terminal and checks after each of them whether the buffer
//! holds the goal. [`panel`] draws the lesson next to the buffer, [`progress`] remembers the best
//! counts, and [`command`] has the typable command.

pub mod command;
pub mod exercise;
pub mod panel;
pub mod progress;
pub mod session;

pub use exercise::{load_exercises, load_exercises_from, validate, Exercise};
pub use panel::render;
pub use progress::{set_progress_file, Progress, Status};
pub use session::{
    after_event, attempt, before_event, exercises, reset_session, set_exercises, start_exercise,
    start_index, Attempt,
};
