//! The debugger (fork feature): sessions with the debug adapters configured in
//! `languages.toml`, and the debug panel below the splits (variables, watch, call stack,
//! breakpoints, console). The state the panel shows is `vortex-view`'s `editor.debug`; this
//! module talks to the adapters, loads that state, draws it and routes keys to it.

pub mod breakpoints;
pub mod load;
pub mod panel;
mod render;
mod session;

pub use panel::{
    focus_section, handle_key, handle_mouse, show, toggle, toggle_focus, Handled, FOCUS_KEY,
};
pub use render::render;
pub use session::{end, end_with, handle_message, start};

use vortex_view::{graphics::Rect, Editor};

/// The panel's area below the splits, when it is shown: the splits get `area` without it. It
/// has the terminal panel's height (`[editor.terminal] height`), and its place.
pub fn split_area(editor: &Editor, area: Rect) -> (Rect, Option<Rect>) {
    if !editor.debug.visible || area.height < 6 {
        return (area, None);
    }
    let height = crate::terminal::screen_height(editor, area.height).max(4);
    let editor_area = area.clip_bottom(height);
    let panel = Rect::new(area.x, editor_area.bottom(), area.width, height);
    (editor_area, Some(panel))
}
