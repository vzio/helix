//! The dashboard (this fork, see FORK.md, "Dashboard"): the main menu `vx` and `vx <folder>`
//! open on, and `:dashboard` brings back. A logo, the project's git state and languages, and
//! the actions to start with, each a key.
//!
//! It isn't a buffer: it is drawn by the [`EditorView`](crate::ui::EditorView) over an empty
//! scratch buffer, the one `i` or `Esc` drop into, and it stays only while that buffer is the
//! focused one, unchanged, in normal mode. So opening a file from anywhere (a picker, the file
//! tree, `:open`), a split, or a key that edits or leaves normal mode makes it vanish, with no
//! hook of its own. Its own keys come first; every other key goes to the keymap as usual.
//!
//! The stats are read in the background ([`stats`]) and the drawing ([`render`]) animates them in
//! as they arrive, on the clock of [`timeline`]: the title opens from `VX` to `VORTEX` once
//! everything is in, over a [`vortex`] that spins, slows down or stands still as configured.
//! Once nothing moves anymore, no more frames are asked for.

pub mod languages;
pub mod render;
pub mod stats;
pub mod timeline;
pub mod title;
pub mod vortex;
pub mod wrap;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use vortex_loader::workspace_trust::TrustQuery;
use vortex_view::document::Mode;
use vortex_view::editor::Action;
use vortex_view::graphics::Rect;
use vortex_view::input::KeyEvent;
use vortex_view::keyboard::KeyCode;
use vortex_view::{DocumentId, Editor, ViewId};

use tui::buffer::Buffer as Surface;

use crate::commands::{self, MappableCommand};
use crate::keymap::Keymaps;

pub use render::Hints;
pub use stats::Stats;
pub use timeline::Timeline;

pub struct Dashboard {
    stats: Arc<Stats>,
    timeline: Timeline,
    dir: PathBuf,
    hints: Hints,
    /// The scratch buffer it is drawn over, in which view, at which version.
    view: ViewId,
    doc: DocumentId,
    version: i32,
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        self.stats.closed.store(true, Ordering::Relaxed);
    }
}

/// What a key does on the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyResult {
    /// Not one of its keys: the keymap takes it.
    Ignored,
    /// Done, the dashboard stays (a picker opened over it).
    Consumed,
    /// Done, and the dashboard goes.
    Close,
}

/// Whether the document is an empty scratch buffer the dashboard can be drawn over.
fn is_empty_scratch(doc: &vortex_view::Document) -> bool {
    doc.path().is_none() && !doc.is_modified() && !doc.is_hex() && doc.text().len_chars() <= 1
}

impl Dashboard {
    /// Opens the dashboard over the focused view: over its buffer when it is an empty scratch
    /// buffer, else over a new one.
    pub fn open(editor: &mut Editor, keymaps: &Keymaps) -> Self {
        if !is_empty_scratch(doc!(editor)) {
            editor.new_file(Action::Replace);
        }
        let (view, doc) = current_ref!(editor);
        let (view, doc_id, version) = (view.id, doc.id(), doc.version());

        let timeline = Timeline::new(Instant::now(), editor.config().dashboard_background);
        let dir = vortex_stdx::env::current_working_dir();
        let trust_full = editor
            .workspace_trust
            .query(&vortex_loader::find_workspace_in(&dir).0, TrustQuery::Git)
            .is_trusted();
        let stats = stats::load(
            dir.clone(),
            trust_full,
            editor.syn_loader.load_full(),
            editor.config().changes.clone(),
            timeline,
        );
        Self {
            stats,
            timeline,
            dir,
            hints: hints(keymaps),
            view,
            doc: doc_id,
            version,
        }
    }

    /// A dashboard over the focused view with the given stats, for tests: nothing is read.
    pub fn with_stats(editor: &Editor, stats: Arc<Stats>, born: Instant, hints: Hints) -> Self {
        let (view, doc) = current_ref!(editor);
        Self {
            stats,
            timeline: Timeline::new(born, editor.config().dashboard_background),
            dir: vortex_stdx::env::current_working_dir(),
            hints,
            view: view.id,
            doc: doc.id(),
            version: doc.version(),
        }
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    pub fn timeline(&self) -> Timeline {
        self.timeline
    }

    /// Whether the dashboard is still up: its buffer focused, unchanged, in normal mode.
    pub fn covers(&self, editor: &Editor) -> bool {
        editor.mode() == Mode::Normal
            && editor.tree.focus == self.view
            && editor.tree.contains(self.view)
            && editor.tree.get(self.view).doc == self.doc
            && editor
                .document(self.doc)
                .is_some_and(|doc| doc.version() == self.version && !doc.is_modified())
    }

    /// Handles `key`, one of its own (`i`, `Esc`, `f`, `e`, `l` in a git repository, `q`).
    pub fn handle_key(&self, key: KeyEvent, cx: &mut commands::Context) -> KeyResult {
        if !key.modifiers.is_empty() {
            return KeyResult::Ignored;
        }
        let command = match key.code {
            KeyCode::Esc => return KeyResult::Close,
            KeyCode::Char('i') => {
                MappableCommand::insert_mode.execute(cx);
                return KeyResult::Close;
            }
            KeyCode::Char('f') => MappableCommand::file_picker,
            KeyCode::Char('e') => MappableCommand::toggle_explorer_root,
            KeyCode::Char('l') if self.stats.shows_git() => MappableCommand::commit_picker,
            KeyCode::Char('q') => match ":quit".parse::<MappableCommand>() {
                Ok(quit) => quit,
                Err(_) => return KeyResult::Ignored,
            },
            _ => return KeyResult::Ignored,
        };
        command.execute(cx);
        KeyResult::Consumed
    }

    pub fn render(&self, editor: &Editor, area: Rect, surface: &mut Surface) {
        self.render_at(editor, area, surface, Instant::now());
    }

    /// Draws the dashboard as it is at `now`.
    pub fn render_at(&self, editor: &Editor, area: Rect, surface: &mut Surface, now: Instant) {
        let model = render::Model {
            stats: &self.stats,
            timeline: self.timeline,
            dir: &self.dir,
            hints: &self.hints,
            title: editor.config().dashboard_title,
        };
        render::render(&model, &editor.theme, area, surface, now);
    }
}

/// The shortest keys of the normal mode keymap for the actions.
pub fn hints(keymaps: &Keymaps) -> Hints {
    let map = keymaps.map();
    let Some(normal) = map.get(&Mode::Normal) else {
        return Hints::default();
    };
    let reverse = normal.reverse_map();
    let keys = |command: &str| -> Option<String> {
        let sequences = reverse.get(command)?;
        let shortest = sequences.iter().min_by_key(|keys| keys.len())?;
        Some(
            shortest
                .iter()
                .map(|key| key.to_string())
                .collect::<Vec<_>>()
                .join(" "),
        )
    };
    Hints {
        files: keys("file_picker"),
        tree: keys("toggle_explorer_root"),
        history: keys("commit_picker"),
        quit: Some(":q".to_string()),
    }
}
