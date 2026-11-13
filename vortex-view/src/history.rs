//! History (this fork): the automatic "take me back" of `ga` and `gA`.
//!
//! Each view keeps the places it jumped away from, oldest first. Only teleports record one (see
//! [`Jump`] and [`records`]): going to a definition, a search, the start or end of the file, a
//! line by number, a picker's result, another buffer, another screen, a bookmark. Moving through
//! the text around the cursor never does. Every recording goes through [`Editor::record_jump`],
//! so that rule lives here only.
//!
//! Walking back (`gA`) first records where the walk starts, the present, so that walking forward
//! (`a`) or toggling (`ga`) can return to it; jumping somewhere new while walking drops the
//! places ahead, as an edit drops the redo history.
//!
//! A place is a position in a buffer or a screen (the dashboard, a diff, the debug panel), which
//! the terminal UI shows again: the editor only knows that it is one.

use std::{any::Any, cell::RefCell, collections::VecDeque, fmt, rc::Rc};

use vortex_core::{Selection, Transaction};

use crate::{Document, DocumentId, Editor, ViewId};

/// The most places a view remembers; the oldest go first.
pub const CAPACITY: usize = 30;

/// What moved the cursor, for [`records`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// `gd`, `gD`, `gy`, `gr`, `gi` and the other locations of a language server.
    Goto,
    /// `/`, `?`, `n`, `N`.
    Search,
    /// `gg`, `ge`, and `gg` with a count.
    FileEdge,
    /// `:<line>`.
    Line,
    /// `g.`, the last modification.
    Modification,
    /// `[d`, `]d`, `[D`, `]D`.
    Diagnostic,
    /// `[g`, `]g`, `[G`, `]G`.
    Change,
    /// The result picked in a picker.
    Pick,
    /// Another buffer, by any means (`gn`, `gf`, `:open`, `:buffer`...).
    Switch,
    /// A screen opens: the dashboard, a diff, the debug panel.
    Screen,
    /// `C-i`, `C-o`: the next or previous bookmark.
    Bookmark,
    /// `]f`, `[t`... the syntax tree's objects around the cursor: a motion.
    Object,
    /// `gw`, a label on the screen: a motion.
    Label,
    /// `g|`, a column of the line: a motion.
    Column,
}

/// Whether a jump of this kind records the place it leaves: teleports do, motions through the
/// text around the cursor don't.
pub fn records(jump: Jump) -> bool {
    !matches!(jump, Jump::Object | Jump::Label | Jump::Column)
}

/// A full-screen view that history can return to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenKind {
    Dashboard,
    /// `:vortex`, the dashboard's vortex alone.
    Vortex,
    /// The side by side diff (any of them: a file, a commit, an uncommitted change).
    Diff,
    /// The debug panel, focused.
    DebugPanel,
}

/// What the terminal UI needs to show a screen again (the diff view itself). It is taken when
/// the screen is shown, so a screen is shown from one place only.
pub type Stash = Rc<RefCell<Option<Box<dyn Any>>>>;

#[derive(Clone)]
pub struct Screen {
    pub kind: ScreenKind,
    pub stash: Option<Stash>,
}

impl Screen {
    pub fn new(kind: ScreenKind) -> Self {
        Self { kind, stash: None }
    }

    /// A screen that `state` shows again.
    pub fn stashed(kind: ScreenKind, state: Box<dyn Any>) -> Self {
        Self {
            kind,
            stash: Some(Rc::new(RefCell::new(Some(state)))),
        }
    }

    /// Takes what shows the screen again.
    pub fn take(&self) -> Option<Box<dyn Any>> {
        self.stash
            .as_ref()
            .and_then(|stash| stash.borrow_mut().take())
    }

    fn is_empty(&self) -> bool {
        self.stash
            .as_ref()
            .is_some_and(|stash| stash.borrow().is_none())
    }

    fn same(&self, other: &Screen) -> bool {
        self.kind == other.kind
            && match (&self.stash, &other.stash) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

impl fmt::Debug for Screen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Screen")
            .field("kind", &self.kind)
            .field("stashed", &self.stash.is_some())
            .finish()
    }
}

/// A place history returns to.
#[derive(Debug, Clone)]
pub enum Place {
    Text {
        doc: DocumentId,
        selection: Selection,
    },
    Screen(Screen),
}

impl Place {
    pub fn screen(kind: ScreenKind) -> Self {
        Place::Screen(Screen::new(kind))
    }
}

/// The places a view jumped away from, oldest first, and where a walk through them is.
#[derive(Debug, Clone, Default)]
pub struct History {
    places: VecDeque<Place>,
    /// The place a walk is at; `places.len()` when not walking (at the present). While walking,
    /// the last place is where the walk started.
    current: usize,
}

impl History {
    pub fn places(&self) -> impl DoubleEndedIterator<Item = &Place> {
        self.places.iter()
    }

    pub fn len(&self) -> usize {
        self.places.len()
    }

    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    /// The index of the place a walk is at, `len()` when not walking.
    pub fn current(&self) -> usize {
        self.current
    }

    pub fn is_walking(&self) -> bool {
        self.current < self.places.len()
    }

    /// Records `place`, dropping the places ahead of a walk; it replaces the last place when it is
    /// the same one (`same_as_last`).
    pub(crate) fn push(&mut self, place: Place, same_as_last: bool) {
        self.places.truncate(self.current);
        if same_as_last {
            self.places.pop_back();
        }
        self.places.push_back(place);
        while self.places.len() > CAPACITY {
            self.places.pop_front();
        }
        self.current = self.places.len();
    }

    /// The place a new jump would come after: the one before a walk's position.
    fn last_kept(&self) -> Option<&Place> {
        self.current
            .checked_sub(1)
            .and_then(|index| self.places.get(index))
    }

    /// Forgets the places in `doc` (closed).
    pub fn remove(&mut self, doc: &DocumentId) {
        let before = self
            .places
            .iter()
            .take(self.current)
            .filter(|place| matches!(place, Place::Text { doc: id, .. } if id == doc))
            .count();
        self.places
            .retain(|place| !matches!(place, Place::Text { doc: id, .. } if id == doc));
        self.current = self.current.saturating_sub(before).min(self.places.len());
    }

    /// Maps the places in `doc` through its changes, so they stay on their text.
    pub(crate) fn apply(&mut self, transaction: &Transaction, doc: &Document) {
        let text = doc.text().slice(..);
        for place in &mut self.places {
            if let Place::Text { doc: id, selection } = place {
                if *id == doc.id() {
                    *selection = selection
                        .clone()
                        .map(transaction.changes())
                        .ensure_invariants(text);
                }
            }
        }
    }
}

/// Which way `ga`, `gA` and `a` go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `ga`: to the last place, or back to where a walk started.
    Toggle,
    /// `gA`, then `A`: one place back.
    Back,
    /// `a` after `gA`: one place forward; the last one ends the walk.
    Forward,
}

impl Editor {
    /// Where the focused view is: a screen over it (the focused debug panel, the dashboard), or
    /// its cursor. The diff view isn't the editor's: it records itself.
    pub fn place(&self) -> Place {
        if self.debug.is_focused() {
            return Place::screen(ScreenKind::DebugPanel);
        }
        let view = self.tree.get(self.tree.focus);
        if let Some(cover) = &self.screen_cover {
            if cover.view == view.id
                && cover.doc == view.doc
                && self
                    .document(cover.doc)
                    .is_some_and(|doc| doc.version() == cover.version && !doc.is_modified())
            {
                return Place::screen(cover.kind);
            }
        }
        let doc = &self.documents[&view.doc];
        Place::Text {
            doc: view.doc,
            selection: doc.selection(view.id).clone(),
        }
    }

    /// Whether two places are the same for history: the same buffer and line, or the same screen.
    pub fn same_place(&self, a: &Place, b: &Place) -> bool {
        match (a, b) {
            (
                Place::Text {
                    doc: a,
                    selection: sa,
                },
                Place::Text {
                    doc: b,
                    selection: sb,
                },
            ) => {
                a == b
                    && self.document(*a).is_some_and(|doc| {
                        let text = doc.text().slice(..);
                        let len = text.len_chars();
                        let line = |selection: &Selection| {
                            text.char_to_line(selection.primary().cursor(text).min(len))
                        };
                        line(sa) == line(sb)
                    })
            }
            (Place::Screen(a), Place::Screen(b)) => a.same(b),
            _ => false,
        }
    }

    /// Records in the focused view's history the place `from` that a jump of kind `jump` left,
    /// now that it landed: the one function every jump goes through. Nothing is recorded for a
    /// motion ([`records`]), during a history step or another move that must not record (see
    /// [`Editor::without_history`]), for a jump that landed on the line it left, or for a place
    /// in a buffer that is closed; a place that is the same as the last one replaces it.
    pub fn record_jump(&mut self, jump: Jump, from: Place) {
        if self.history_paused || !records(jump) {
            return;
        }
        // a screen opens over the place: the editor's place is still `from`
        if jump != Jump::Screen && self.same_place(&from, &self.place()) {
            return;
        }
        if let Place::Text { doc, .. } = &from {
            if !self.documents.contains_key(doc) {
                return;
            }
        }
        if let Place::Screen(screen) = &from {
            if screen.is_empty() {
                return;
            }
        }
        let view_id = self.tree.focus;
        let same_as_last = self
            .tree
            .get(view_id)
            .history
            .last_kept()
            .is_some_and(|last| {
                let last = last.clone();
                self.same_place(&last, &from)
            });
        let view = self.tree.get_mut(view_id);
        // the place's selection is valid at its document's current revision: the view must be
        // too, or its next sync would map it through changes it has already seen
        if let Place::Text { doc, .. } = &from {
            if let Some(doc) = self.documents.get_mut(doc) {
                view.sync_changes(doc);
            }
        }
        view.history.push(from, same_as_last);
    }

    /// Runs `move_to`, recording the place it leaves as a jump of kind `jump`.
    pub fn teleport<R>(&mut self, jump: Jump, move_to: impl FnOnce(&mut Self) -> R) -> R {
        let from = self.place();
        let result = move_to(self);
        self.record_jump(jump, from);
        result
    }

    /// Runs `f` without recording jumps: history's own steps, and moves that aren't the user's
    /// (the debugger's stops).
    pub fn without_history<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let paused = std::mem::replace(&mut self.history_paused, true);
        let result = f(self);
        self.history_paused = paused;
        result
    }

    /// Whether history can go to `place` from `here`: its buffer is open, its screen can be shown
    /// again, and it isn't where the cursor already is.
    fn reachable(&self, place: &Place, here: &Place) -> bool {
        if self.same_place(place, here) {
            return false;
        }
        match place {
            Place::Text { doc, .. } => self.documents.contains_key(doc),
            Place::Screen(screen) => match screen.kind {
                ScreenKind::Diff => !screen.is_empty(),
                ScreenKind::DebugPanel => self.debug_adapters.get_active_client().is_some(),
                ScreenKind::Dashboard | ScreenKind::Vortex => true,
            },
        }
    }

    /// Takes a step through the focused view's history from `here` (the place the cursor or the
    /// screen on top is at) and returns the place to show, which the caller shows (with
    /// [`Editor::show_place`] for a place in a buffer). `count` repeats `Back` and `Forward`.
    pub fn history_step(
        &mut self,
        step: Step,
        count: usize,
        here: Place,
    ) -> Result<Place, &'static str> {
        let view_id = self.tree.focus;
        let count = count.max(1);
        // the present is recorded when a walk starts, so the walk can come back to it
        let start_walk = |editor: &mut Self| -> Result<(), &'static str> {
            let history = &editor.tree.get(view_id).history;
            let last = history.places.back().cloned();
            let usable = history
                .places
                .iter()
                .any(|place| editor.reachable(place, &here));
            if !usable {
                return Err("No jumps to go back to");
            }
            let same_as_last = last.is_some_and(|last| editor.same_place(&last, &here));
            if let Place::Text { doc, .. } = &here {
                let doc = editor.documents.get_mut(doc).expect("the focused buffer");
                editor.tree.get_mut(view_id).sync_changes(doc);
            }
            let history = &mut editor.tree.get_mut(view_id).history;
            history.push(here.clone(), same_as_last);
            history.current = history.places.len() - 1;
            Ok(())
        };

        match step {
            Step::Toggle if self.tree.get(view_id).history.is_walking() => {
                let history = &mut self.tree.get_mut(view_id).history;
                let start = history
                    .places
                    .pop_back()
                    .expect("a walk records where it starts");
                history.current = history.places.len();
                // its buffer may have been closed meanwhile
                if !self.reachable(&start, &here) && !self.same_place(&start, &here) {
                    return Err("Where the walk started is gone");
                }
                Ok(start)
            }
            Step::Toggle | Step::Back => {
                if !self.tree.get(view_id).history.is_walking() {
                    start_walk(self)?;
                }
                let history = &self.tree.get(view_id).history;
                let mut index = history.current;
                let mut found = None;
                let mut left = count;
                while index > 0 && left > 0 {
                    index -= 1;
                    if self.reachable(&history.places[index], &here) {
                        found = Some(index);
                        left -= 1;
                    }
                }
                let Some(index) = found else {
                    return Err("At the oldest jump");
                };
                let place = history.places[index].clone();
                self.tree.get_mut(view_id).history.current = index;
                Ok(place)
            }
            Step::Forward => {
                let history = &self.tree.get(view_id).history;
                if !history.is_walking() {
                    return Err("At the newest jump");
                }
                let last = history.places.len() - 1;
                let mut index = history.current;
                let mut found = None;
                let mut left = count;
                while index < last && left > 0 {
                    index += 1;
                    if index == last || self.reachable(&history.places[index], &here) {
                        found = Some(index);
                        left -= 1;
                    }
                }
                let Some(index) = found else {
                    return Err("At the newest jump");
                };
                let history = &mut self.tree.get_mut(view_id).history;
                if index == last {
                    // back where the walk started: the present again
                    let start = history
                        .places
                        .pop_back()
                        .expect("a walk records where it starts");
                    history.current = history.places.len();
                    return Ok(start);
                }
                history.current = index;
                Ok(history.places[index].clone())
            }
        }
    }

    /// Shows a place in a buffer in the focused view, centered, without recording a jump.
    pub fn show_place(&mut self, doc: DocumentId, selection: Selection) {
        self.debug.unfocus();
        let view_id = self.tree.focus;
        self.without_history(|editor| editor.jump_to(view_id, doc, selection));
    }
}

/// The screen drawn over a buffer (the dashboard): while the buffer is unchanged in its view, the
/// focused place is the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenCover {
    pub kind: ScreenKind,
    pub view: ViewId,
    pub doc: DocumentId,
    pub version: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teleports_record_and_motions_dont() {
        for jump in [
            Jump::Goto,
            Jump::Search,
            Jump::FileEdge,
            Jump::Line,
            Jump::Modification,
            Jump::Diagnostic,
            Jump::Change,
            Jump::Pick,
            Jump::Switch,
            Jump::Screen,
            Jump::Bookmark,
        ] {
            assert!(records(jump), "{jump:?} should record");
        }
        for jump in [Jump::Object, Jump::Label, Jump::Column] {
            assert!(!records(jump), "{jump:?} should not record");
        }
    }

    fn screen(kind: ScreenKind) -> Place {
        Place::screen(kind)
    }

    #[test]
    fn push_drops_the_places_ahead_and_replaces_the_same_one() {
        let mut history = History::default();
        history.push(screen(ScreenKind::Dashboard), false);
        history.push(screen(ScreenKind::Diff), false);
        history.push(screen(ScreenKind::DebugPanel), false);
        assert_eq!(history.current(), 3);
        history.current = 1;
        assert!(history.is_walking());
        history.push(screen(ScreenKind::Vortex), false);
        let kinds: Vec<_> = history
            .places()
            .map(|place| match place {
                Place::Screen(screen) => screen.kind,
                Place::Text { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(kinds, [ScreenKind::Dashboard, ScreenKind::Vortex]);
        assert!(!history.is_walking());
        history.push(screen(ScreenKind::Diff), true);
        assert_eq!(history.len(), 2);
    }

    /// `remove` drops the places of a closed buffer and keeps `current` on the same place (or the
    /// present), so a walk goes on from where it was.
    #[test]
    fn remove_keeps_the_walk_where_it_was() {
        let (a, b) = (DocumentId::new(1), DocumentId::new(2));
        let make = |current: usize| {
            let mut history = History::default();
            for (i, doc) in [a, b, a, b, a].into_iter().enumerate() {
                history.places.push_back(Place::Text {
                    doc,
                    selection: Selection::point(i),
                });
            }
            history.current = current;
            history
        };
        let docs = |history: &History| -> Vec<DocumentId> {
            history
                .places()
                .map(|place| match place {
                    Place::Text { doc, .. } => *doc,
                    Place::Screen(_) => unreachable!(),
                })
                .collect()
        };

        let mut history = make(3);
        history.remove(&a);
        assert_eq!(docs(&history), [b, b]);
        assert_eq!(history.current(), 1);
        assert!(matches!(
            history.places().nth(1),
            Some(Place::Text { selection, .. }) if *selection == Selection::point(3)
        ));

        let mut history = make(5);
        history.remove(&a);
        assert_eq!(history.current(), 2);
        assert!(!history.is_walking());

        let mut history = make(3);
        history.remove(&a);
        history.remove(&b);
        assert!(history.is_empty());
        assert_eq!(history.current(), 0);

        let mut history = make(0);
        history.remove(&a);
        assert_eq!(history.current(), 0);
    }

    #[test]
    fn history_is_bounded() {
        let mut history = History::default();
        for _ in 0..CAPACITY + 10 {
            history.push(screen(ScreenKind::Dashboard), false);
        }
        assert_eq!(history.len(), CAPACITY);
        assert_eq!(history.current(), CAPACITY);
    }

    #[test]
    fn a_stashed_screen_is_shown_from_one_place() {
        let screen = Screen::stashed(ScreenKind::Diff, Box::new(1u8));
        let copy = screen.clone();
        assert!(screen.same(&copy));
        assert!(!Screen::stashed(ScreenKind::Diff, Box::new(1u8)).same(&screen));
        assert!(screen.take().is_some());
        assert!(copy.is_empty());
        assert!(copy.take().is_none());
    }
}
