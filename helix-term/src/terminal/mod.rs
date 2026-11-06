//! The terminal panel (fork feature): a shell below the editor's splits. `C-t` toggles focus,
//! `:sh` runs a command in it, `Esc` in normal mode hides it, and an `hx` in it opens files in
//! this editor. The shell itself is `helix-terminal`'s; this module draws it, routes keys and
//! the mouse to it, and starts and stops it.

pub mod input;
pub mod shim;
