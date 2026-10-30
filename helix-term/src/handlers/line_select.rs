//! State for `x` (`extend_line_below`) on empty lines.
//!
//! On an empty line the range `x` selects (the line ending) is the same as the 1-wide cursor on
//! that line, so the range alone can't tell "`x` was just pressed" from "the cursor is here".
//! `extend_line_below` remembers the selection it last set (the marker); the marker is dropped by
//! any other selection change in that view, and by any edit of that document, so only an
//! untouched selection that `x` itself produced counts as a selected line.
//!
//! The marker is per thread rather than a global: commands and hooks run synchronously on the
//! thread of the editor, and a global would be shared by every editor of the process (the
//! integration tests run many in parallel).

use std::cell::RefCell;

use helix_core::Selection;
use helix_event::register_hook;
use helix_view::events::{DocumentDidChange, SelectionDidChange};
use helix_view::handlers::Handlers;
use helix_view::{DocumentId, ViewId};

thread_local! {
    static MARKER: RefCell<Option<(DocumentId, ViewId, Selection)>> = const { RefCell::new(None) };
}

/// Remembers `selection` as the one `x` just set in this view. Call it after `set_selection`,
/// which clears the marker through the hook below.
pub fn mark(doc: DocumentId, view: ViewId, selection: &Selection) {
    MARKER.with_borrow_mut(|marker| *marker = Some((doc, view, selection.clone())));
}

/// Whether `selection` is exactly what `x` last set in this view, with nothing changed since.
pub fn is_marked(doc: DocumentId, view: ViewId, selection: &Selection) -> bool {
    MARKER.with_borrow(|marker| {
        marker
            .as_ref()
            .is_some_and(|(d, v, s)| *d == doc && *v == view && s == selection)
    })
}

fn clear_if(matches: impl FnOnce(DocumentId, ViewId) -> bool) {
    MARKER.with_borrow_mut(|marker| {
        if marker.as_ref().is_some_and(|(d, v, _)| matches(*d, *v)) {
            *marker = None;
        }
    });
}

pub(super) fn register_hooks(_handlers: &Handlers) {
    register_hook!(move |event: &mut SelectionDidChange<'_>| {
        // Not only the selection of the marked document: another buffer shown in the same view
        // must not inherit the marker.
        clear_if(|_, view| view == event.view);
        Ok(())
    });
    register_hook!(move |event: &mut DocumentDidChange<'_>| {
        // Edits move the selection without a `SelectionDidChange`.
        clear_if(|doc, _| doc == event.doc.id());
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_matches_only_the_exact_selection() {
        let doc = DocumentId::default();
        let view = ViewId::default();
        let selection = Selection::single(2, 3);
        assert!(!is_marked(doc, view, &selection));
        mark(doc, view, &selection);
        assert!(is_marked(doc, view, &selection));
        assert!(!is_marked(doc, view, &Selection::single(2, 4)));
        assert!(!is_marked(doc, view, &Selection::single(3, 2)));
        clear_if(|_, v| v == view);
        assert!(!is_marked(doc, view, &selection));
    }
}
