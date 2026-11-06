//! Finds the OSC 133 shell integration markers in the output of the shell, before it goes to
//! the emulator (which ignores them):
//!
//! - `ESC ] 133 ; A ST`: the prompt starts
//! - `ESC ] 133 ; B ST`: the prompt ends, the user types the command
//! - `ESC ] 133 ; C ST`: the command runs
//! - `ESC ] 133 ; D [; exit code] ST`: the command finished
//!
//! where ST is `BEL` or `ESC \`. A marker may be split between two reads.
