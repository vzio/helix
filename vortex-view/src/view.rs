use crate::{
    align_view,
    annotations::diagnostics::InlineDiagnostics,
    document::{DocumentColorSwatches, DocumentInlayHints},
    editor::{GutterConfig, GutterType},
    graphics::Rect,
    handlers::diagnostics::DiagnosticsHandler,
    Align, Document, DocumentId, Theme, ViewId,
};

use vortex_core::{
    char_idx_at_visual_offset,
    doc_formatter::TextFormat,
    text_annotations::TextAnnotations,
    visual_offset_from_anchor, visual_offset_from_block, Position, RopeSlice, Selection,
    Transaction,
    VisualOffsetError::{PosAfterMaxRow, PosBeforeAnchorRow},
};

use std::{collections::HashMap, fmt};

#[derive(Clone, Debug, PartialEq, Eq, Copy, Default)]
pub struct ViewPosition {
    pub anchor: usize,
    pub horizontal_offset: usize,
    pub vertical_offset: usize,
}

#[derive(Clone)]
pub struct View {
    pub id: ViewId,
    pub area: Rect,
    pub doc: DocumentId,
    /// The places this view jumped away from (this fork: history, `ga`/`gA`).
    pub history: crate::history::History,
    // documents accessed from this view from the oldest one to last viewed one
    pub docs_access_history: Vec<DocumentId>,
    /// the last modified files before the current one
    /// ordered from most frequent to least frequent
    // uses two docs because we want to be able to swap between the
    // two last modified docs which we need to manually keep track of
    pub last_modified_docs: [Option<DocumentId>; 2],
    /// used to store previous selections of tree-sitter objects
    pub object_selections: Vec<Selection>,
    /// all gutter-related configuration settings, used primarily for gutter rendering
    pub gutters: GutterConfig,
    /// A mapping between documents and the last history revision the view was updated at.
    /// Changes between documents and views are synced lazily when switching windows. This
    /// mapping keeps track of the last applied history revision so that only new changes
    /// are applied.
    doc_revisions: HashMap<DocumentId, usize>,
    // HACKS: there should really only be a global diagnostics handler (the
    // non-focused views should just not have different handling for the cursor
    // line). For that we would need accces to editor everywhere (we want to use
    // the positioning code) so this can only happen by refactoring View and
    // Document into entity component like structure. That is a huge refactor
    // left to future work. For now we treat all views as focused and give them
    // each their own handler.
    pub diagnostics_handler: DiagnosticsHandler,
}

impl fmt::Debug for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("View")
            .field("id", &self.id)
            .field("area", &self.area)
            .field("doc", &self.doc)
            .finish()
    }
}

impl View {
    pub fn new(doc: DocumentId, gutters: GutterConfig) -> Self {
        Self {
            id: ViewId::default(),
            doc,
            area: Rect::default(), // will get calculated upon inserting into tree
            history: crate::history::History::default(),
            docs_access_history: Vec::new(),
            last_modified_docs: [None, None],
            object_selections: Vec::new(),
            gutters,
            doc_revisions: HashMap::new(),
            diagnostics_handler: DiagnosticsHandler::new(),
        }
    }

    pub fn add_to_history(&mut self, id: DocumentId) {
        if let Some(pos) = self.docs_access_history.iter().position(|&doc| doc == id) {
            self.docs_access_history.remove(pos);
        }
        self.docs_access_history.push(id);
    }

    pub fn inner_area(&self, doc: &Document) -> Rect {
        self.area.clip_left(self.gutter_offset(doc)).clip_bottom(1) // -1 for statusline
    }

    pub fn inner_height(&self) -> usize {
        self.area.clip_bottom(1).height.into() // -1 for statusline
    }

    pub fn inner_width(&self, doc: &Document) -> u16 {
        self.area.clip_left(self.gutter_offset(doc)).width
    }

    pub fn gutters(&self) -> &[GutterType] {
        &self.gutters.layout
    }

    pub fn gutter_offset(&self, doc: &Document) -> u16 {
        let total_width = self
            .gutters
            .layout
            .iter()
            .map(|gutter| gutter.width(self, doc) as u16)
            .sum();
        if total_width < self.area.width {
            total_width
        } else {
            0
        }
    }

    //
    pub fn offset_coords_to_in_view(
        &self,
        doc: &Document,
        scrolloff: usize,
    ) -> Option<ViewPosition> {
        self.offset_coords_to_in_view_center::<false>(doc, scrolloff)
    }

    pub fn offset_coords_to_in_view_center<const CENTERING: bool>(
        &self,
        doc: &Document,
        scrolloff: usize,
    ) -> Option<ViewPosition> {
        let view_offset = doc.get_view_offset(self.id)?;
        let doc_text = doc.text().slice(..);
        let viewport = self.inner_area(doc);
        let vertical_viewport_end = view_offset.vertical_offset + viewport.height as usize;
        let text_fmt = doc.text_format(viewport.width, None);
        let annotations = self.text_annotations(doc, None);

        let (scrolloff_top, scrolloff_bottom) = if CENTERING {
            (0, 0)
        } else {
            (
                // - 1 from the top so we have at least one gap in the middle.
                scrolloff.min(viewport.height.saturating_sub(1) as usize / 2),
                scrolloff.min(viewport.height as usize / 2),
            )
        };
        let (scrolloff_left, scrolloff_right) = if CENTERING {
            (0, 0)
        } else {
            (
                // - 1 from the left so we have at least one gap in the middle.
                scrolloff.min(viewport.width.saturating_sub(1) as usize / 2),
                scrolloff.min(viewport.width as usize / 2),
            )
        };

        let cursor = doc.selection(self.id).primary().cursor(doc_text);
        let mut offset = view_offset;
        let off = visual_offset_from_anchor(
            doc_text,
            offset.anchor,
            cursor,
            &text_fmt,
            &annotations,
            vertical_viewport_end,
        );

        let (new_anchor, at_top) = match off {
            Ok((visual_pos, _)) if visual_pos.row < scrolloff_top + offset.vertical_offset => {
                if CENTERING {
                    // cursor out of view
                    return None;
                }
                (true, true)
            }
            Ok((visual_pos, _)) if visual_pos.row + scrolloff_bottom >= vertical_viewport_end => {
                (true, false)
            }
            Ok((_, _)) => (false, false),
            Err(_) if CENTERING => return None,
            Err(PosBeforeAnchorRow) => (true, true),
            Err(PosAfterMaxRow) => (true, false),
        };

        if new_anchor {
            let v_off = if at_top {
                scrolloff_top as isize
            } else {
                viewport.height as isize - scrolloff_bottom as isize - 1
            };
            (offset.anchor, offset.vertical_offset) =
                char_idx_at_visual_offset(doc_text, cursor, -v_off, 0, &text_fmt, &annotations);
        }

        if text_fmt.soft_wrap {
            offset.horizontal_offset = 0;
        } else {
            // determine the current visual column of the text
            let col = off
                .unwrap_or_else(|_| {
                    visual_offset_from_block(
                        doc_text,
                        offset.anchor,
                        cursor,
                        &text_fmt,
                        &annotations,
                    )
                })
                .0
                .col;

            let last_col = offset.horizontal_offset + viewport.width.saturating_sub(1) as usize;
            if col > last_col.saturating_sub(scrolloff_right) {
                // scroll right
                offset.horizontal_offset += col - (last_col.saturating_sub(scrolloff_right))
            } else if col < offset.horizontal_offset + scrolloff_left {
                // scroll left
                offset.horizontal_offset = col.saturating_sub(scrolloff_left)
            };
        }

        // if we are not centering return None if view position is unchanged
        if !CENTERING && offset == view_offset {
            return None;
        }

        Some(offset)
    }

    pub fn ensure_cursor_in_view(&self, doc: &mut Document, scrolloff: usize) {
        if let Some(offset) = self.offset_coords_to_in_view_center::<false>(doc, scrolloff) {
            doc.set_view_offset(self.id, offset);
        }
    }

    pub fn ensure_cursor_in_view_center(&self, doc: &mut Document, scrolloff: usize) {
        if let Some(offset) = self.offset_coords_to_in_view_center::<true>(doc, scrolloff) {
            doc.set_view_offset(self.id, offset);
        } else {
            align_view(doc, self, Align::Center);
        }
    }

    pub fn is_cursor_in_view(&mut self, doc: &Document, scrolloff: usize) -> bool {
        self.offset_coords_to_in_view(doc, scrolloff).is_none()
    }

    /// Estimates the last visible document line on screen.
    /// This estimate is an upper bound obtained by calculating the first
    /// visible line and adding the viewport height.
    /// The actual last visible line may be smaller if softwrapping occurs
    /// or virtual text lines are visible
    #[inline]
    pub fn estimate_last_doc_line(&self, doc: &Document) -> usize {
        let doc_text = doc.text().slice(..);
        let line = doc_text.char_to_line(doc.view_offset(self.id).anchor.min(doc_text.len_chars()));
        // Saturating subs to make it inclusive zero indexing.
        (line + self.inner_height())
            .min(doc_text.len_lines())
            .saturating_sub(1)
    }

    /// Calculates the last non-empty visual line on screen
    #[inline]
    pub fn last_visual_line(&self, doc: &Document) -> usize {
        let doc_text = doc.text().slice(..);
        let viewport = self.inner_area(doc);
        let text_fmt = doc.text_format(viewport.width, None);
        let annotations = self.text_annotations(doc, None);
        let view_offset = doc.view_offset(self.id);

        // last visual line in view is trivial to compute
        let visual_height = doc.view_offset(self.id).vertical_offset + viewport.height as usize;

        // fast path when the EOF is not visible on the screen,
        if self.estimate_last_doc_line(doc) < doc_text.len_lines() - 1 {
            return visual_height.saturating_sub(1);
        }

        // translate to document line
        let pos = visual_offset_from_anchor(
            doc_text,
            view_offset.anchor,
            usize::MAX,
            &text_fmt,
            &annotations,
            visual_height,
        );

        match pos {
            Ok((Position { row, .. }, _)) => row.saturating_sub(view_offset.vertical_offset),
            Err(PosAfterMaxRow) => visual_height.saturating_sub(1),
            Err(PosBeforeAnchorRow) => 0,
        }
    }

    /// Translates a document position to an absolute position in the terminal.
    /// Returns a (line, col) position if the position is visible on screen.
    // TODO: Could return width as well for the character width at cursor.
    pub fn screen_coords_at_pos(
        &self,
        doc: &Document,
        text: RopeSlice,
        pos: usize,
    ) -> Option<Position> {
        let view_offset = doc.view_offset(self.id);

        let viewport = self.inner_area(doc);
        let text_fmt = doc.text_format(viewport.width, None);
        let annotations = self.text_annotations(doc, None);

        let mut pos = visual_offset_from_anchor(
            text,
            view_offset.anchor,
            pos,
            &text_fmt,
            &annotations,
            viewport.height as usize,
        )
        .ok()?
        .0;
        if pos.row < view_offset.vertical_offset {
            return None;
        }
        pos.row -= view_offset.vertical_offset;
        if pos.row >= viewport.height as usize {
            return None;
        }
        pos.col = pos.col.saturating_sub(view_offset.horizontal_offset);

        Some(pos)
    }

    /// Get the text annotations to display in the current view for the given document and theme.
    pub fn text_annotations<'a>(
        &self,
        doc: &'a Document,
        theme: Option<&Theme>,
    ) -> TextAnnotations<'a> {
        let mut text_annotations = TextAnnotations::default();

        if let Some(labels) = doc.jump_labels.get(&self.id) {
            let style = theme.and_then(|t| t.find_highlight("ui.virtual.jump-label"));
            text_annotations.add_overlay(labels, style);
        }

        if let Some(DocumentInlayHints {
            id: _,
            type_inlay_hints,
            parameter_inlay_hints,
            other_inlay_hints,
            padding_before_inlay_hints,
            padding_after_inlay_hints,
        }) = doc.inlay_hints.get(&self.id)
        {
            let type_style = theme.and_then(|t| t.find_highlight("ui.virtual.inlay-hint.type"));
            let parameter_style =
                theme.and_then(|t| t.find_highlight("ui.virtual.inlay-hint.parameter"));
            let other_style = theme.and_then(|t| t.find_highlight("ui.virtual.inlay-hint"));

            // Overlapping annotations are ignored apart from the first so the order here is not random:
            // types -> parameters -> others should hopefully be the "correct" order for most use cases,
            // with the padding coming before and after as expected.
            text_annotations
                .add_inline_annotations(padding_before_inlay_hints, None)
                .add_inline_annotations(type_inlay_hints, type_style)
                .add_inline_annotations(parameter_inlay_hints, parameter_style)
                .add_inline_annotations(other_inlay_hints, other_style)
                .add_inline_annotations(padding_after_inlay_hints, None);
        };
        let config = doc.config.load();

        if config.lsp.display_color_swatches {
            if let Some(DocumentColorSwatches {
                color_swatches,
                colors,
                color_swatches_padding,
            }) = &doc.color_swatches
            {
                for (color_swatch, color) in color_swatches.iter().zip(colors) {
                    text_annotations
                        .add_inline_annotations(std::slice::from_ref(color_swatch), Some(*color));
                }

                text_annotations.add_inline_annotations(color_swatches_padding, None);
            }
        }

        let width = self.inner_width(doc);
        let enable_cursor_line = self
            .diagnostics_handler
            .show_cursorline_diagnostics(doc, self.id);
        let config = config.inline_diagnostics.prepare(width, enable_cursor_line);
        if !config.disabled() {
            let cursor = doc
                .selection(self.id)
                .primary()
                .cursor(doc.text().slice(..));
            text_annotations.add_line_annotation(InlineDiagnostics::new(
                doc,
                cursor,
                width,
                doc.view_offset(self.id).horizontal_offset,
                config,
            ));
        }

        text_annotations
    }

    pub fn text_pos_at_screen_coords(
        &self,
        doc: &Document,
        row: u16,
        column: u16,
        fmt: TextFormat,
        annotations: &TextAnnotations,
        ignore_virtual_text: bool,
    ) -> Option<usize> {
        let inner = self.inner_area(doc);
        // 1 for status
        if row < inner.top() || row >= inner.bottom() {
            return None;
        }

        if column < inner.left() || column > inner.right() {
            return None;
        }

        self.text_pos_at_visual_coords(
            doc,
            row - inner.y,
            column - inner.x,
            fmt,
            annotations,
            ignore_virtual_text,
        )
    }

    pub fn text_pos_at_visual_coords(
        &self,
        doc: &Document,
        row: u16,
        column: u16,
        text_fmt: TextFormat,
        annotations: &TextAnnotations,
        ignore_virtual_text: bool,
    ) -> Option<usize> {
        let text = doc.text().slice(..);
        let view_offset = doc.view_offset(self.id);

        let text_row = row as usize + view_offset.vertical_offset;
        let text_col = column as usize + view_offset.horizontal_offset;

        let (char_idx, virt_lines) = char_idx_at_visual_offset(
            text,
            view_offset.anchor,
            text_row as isize,
            text_col,
            &text_fmt,
            annotations,
        );

        // if the cursor is on a line with only virtual text return None
        if virt_lines != 0 && ignore_virtual_text {
            return None;
        }
        Some(char_idx)
    }

    /// Translates a screen position to position in the text document.
    /// Returns a usize typed position in bounds of the text if found in this view, None if out of view.
    pub fn pos_at_screen_coords(
        &self,
        doc: &Document,
        row: u16,
        column: u16,
        ignore_virtual_text: bool,
    ) -> Option<usize> {
        self.text_pos_at_screen_coords(
            doc,
            row,
            column,
            doc.text_format(self.inner_width(doc), None),
            &self.text_annotations(doc, None),
            ignore_virtual_text,
        )
    }

    pub fn pos_at_visual_coords(
        &self,
        doc: &Document,
        row: u16,
        column: u16,
        ignore_virtual_text: bool,
    ) -> Option<usize> {
        self.text_pos_at_visual_coords(
            doc,
            row,
            column,
            doc.text_format(self.inner_width(doc), None),
            &self.text_annotations(doc, None),
            ignore_virtual_text,
        )
    }

    /// Translates screen coordinates into coordinates on the gutter of the view.
    /// Returns a tuple of usize typed line and column numbers starting with 0.
    /// Returns None if coordinates are not on the gutter.
    pub fn gutter_coords_at_screen_coords(&self, row: u16, column: u16) -> Option<Position> {
        // 1 for status
        if row < self.area.top() || row >= self.area.bottom() {
            return None;
        }

        if column < self.area.left() || column > self.area.right() {
            return None;
        }

        Some(Position::new(
            (row - self.area.top()) as usize,
            (column - self.area.left()) as usize,
        ))
    }

    pub fn remove_document(&mut self, doc_id: &DocumentId) {
        self.history.remove(doc_id);
        self.docs_access_history.retain(|doc| doc != doc_id);
    }

    // pub fn traverse<F>(&self, text: RopeSlice, start: usize, end: usize, fun: F)
    // where
    //     F: Fn(usize, usize),
    // {
    //     let start = self.screen_coords_at_pos(text, start);
    //     let end = self.screen_coords_at_pos(text, end);

    //     match (start, end) {
    //         // fully on screen
    //         (Some(start), Some(end)) => {
    //             // we want to calculate ends of lines for each char..
    //         }
    //         // from start to end of screen
    //         (Some(start), None) => {}
    //         // from start of screen to end
    //         (None, Some(end)) => {}
    //         // not on screen
    //         (None, None) => return,
    //     }
    // }

    /// Applies a [`Transaction`] to the view.
    pub fn apply(&mut self, transaction: &Transaction, doc: &mut Document) {
        self.history.apply(transaction, doc);
        self.doc_revisions
            .insert(doc.id(), doc.get_current_revision());
    }

    pub fn sync_changes(&mut self, doc: &mut Document) {
        if let Some(transaction) = self.changes_to_sync(doc) {
            self.apply(&transaction, doc);
        }
    }

    pub(crate) fn changes_to_sync(&mut self, doc: &mut Document) -> Option<Transaction> {
        let latest_revision = doc.get_current_revision();
        let current_revision = *self
            .doc_revisions
            .entry(doc.id())
            .or_insert(latest_revision);

        if current_revision == latest_revision {
            return None;
        }

        doc.history.get_mut().changes_since(current_revision)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use arc_swap::ArcSwap;
    use vortex_core::{syntax, Rope};

    // 1 diagnostic + 1 spacer + 3 linenr (< 1000 lines) + 1 spacer + 1 diff
    const DEFAULT_GUTTER_OFFSET: u16 = 7;

    // 1 diagnostics + 1 spacer + 1 gutter
    const DEFAULT_GUTTER_OFFSET_ONLY_DIAGNOSTICS: u16 = 3;

    use crate::document::Document;
    use crate::editor::{Config, GutterConfig, GutterLineNumbersConfig, GutterType};

    #[test]
    fn test_text_pos_at_screen_coords() {
        let mut view = View::new(DocumentId::default(), GutterConfig::default());
        view.area = Rect::new(40, 40, 40, 40);
        let rope = Rope::from_str("abc\n\tdef");
        let mut doc = Document::from(
            rope,
            None,
            Arc::new(ArcSwap::new(Arc::new(Config::default()))),
            Arc::new(ArcSwap::from_pointee(syntax::Loader::default())),
        );
        doc.ensure_view_init(view.id);

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                2,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            None
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                41,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            None
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                0,
                2,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            None
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                0,
                49,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            None
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                0,
                41,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            None
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                81,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            None
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                78,
                41,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            None
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 3,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(3)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                80,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(3)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                41,
                40 + DEFAULT_GUTTER_OFFSET + 1,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(4)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                41,
                40 + DEFAULT_GUTTER_OFFSET + 4,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(5)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                41,
                40 + DEFAULT_GUTTER_OFFSET + 7,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(8)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                41,
                80,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(8)
        );
    }

    #[test]
    fn test_text_pos_at_screen_coords_without_line_numbers_gutter() {
        let mut view = View::new(
            DocumentId::default(),
            GutterConfig {
                layout: vec![GutterType::Diagnostics],
                line_numbers: GutterLineNumbersConfig::default(),
            },
        );
        view.area = Rect::new(40, 40, 40, 40);
        let rope = Rope::from_str("abc\n\tdef");
        let mut doc = Document::from(
            rope,
            None,
            Arc::new(ArcSwap::new(Arc::new(Config::default()))),
            Arc::new(ArcSwap::from_pointee(syntax::Loader::default())),
        );
        doc.ensure_view_init(view.id);
        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                41,
                40 + DEFAULT_GUTTER_OFFSET_ONLY_DIAGNOSTICS + 1,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(4)
        );
    }

    #[test]
    fn test_text_pos_at_screen_coords_without_any_gutters() {
        let mut view = View::new(
            DocumentId::default(),
            GutterConfig {
                layout: vec![],
                line_numbers: GutterLineNumbersConfig::default(),
            },
        );
        view.area = Rect::new(40, 40, 40, 40);
        let rope = Rope::from_str("abc\n\tdef");
        let mut doc = Document::from(
            rope,
            None,
            Arc::new(ArcSwap::new(Arc::new(Config::default()))),
            Arc::new(ArcSwap::from_pointee(syntax::Loader::default())),
        );
        doc.ensure_view_init(view.id);
        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                41,
                40 + 1,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(4)
        );
    }

    #[test]
    fn test_text_pos_at_screen_coords_cjk() {
        let mut view = View::new(DocumentId::default(), GutterConfig::default());
        view.area = Rect::new(40, 40, 40, 40);
        let rope = Rope::from_str("Hi! こんにちは皆さん");
        let mut doc = Document::from(
            rope,
            None,
            Arc::new(ArcSwap::new(Arc::new(Config::default()))),
            Arc::new(ArcSwap::from_pointee(syntax::Loader::default())),
        );
        doc.ensure_view_init(view.id);

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(0)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 4,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(4)
        );
        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 5,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(4)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 6,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(5)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 7,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(5)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 8,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(6)
        );
    }

    #[test]
    fn test_text_pos_at_screen_coords_graphemes() {
        let mut view = View::new(DocumentId::default(), GutterConfig::default());
        view.area = Rect::new(40, 40, 40, 40);
        let rope = Rope::from_str("Hèl̀l̀ò world!");
        let mut doc = Document::from(
            rope,
            None,
            Arc::new(ArcSwap::new(Arc::new(Config::default()))),
            Arc::new(ArcSwap::from_pointee(syntax::Loader::default())),
        );
        doc.ensure_view_init(view.id);

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(0)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 1,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(1)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 2,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(3)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 3,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(5)
        );

        assert_eq!(
            view.text_pos_at_screen_coords(
                &doc,
                40,
                40 + DEFAULT_GUTTER_OFFSET + 4,
                TextFormat::default(),
                &TextAnnotations::default(),
                true
            ),
            Some(7)
        );
    }

    /// A history place is recorded with the document's *current* selection
    /// (i.e. positions valid at the document's current history revision), but it
    /// never advances the view's `doc_revisions` entry for that document. As a
    /// result, the next `View::sync_changes` computes `changes_since(old_revision)`
    /// and maps *every* jump - including the freshly pushed one - through a
    /// changeset whose pre-image is the *older*, shorter document. A jump that
    /// points past the end of that older document then runs off the end of the
    /// changeset and panics in `ChangeSet::update_positions`
    /// ("Positions ... are out of range for changeset len ...").
    ///
    /// To trigger the stale `doc_revisions` window without reaching into private
    /// fields we use two views over the same document: committing an edit through
    /// `view2` advances the shared document's history and updates *view2*'s
    /// `doc_revisions`, while `view1`'s `doc_revisions` is left pointing at the
    /// pre-edit revision. Pushing a jump into `view1` afterwards reproduces the
    /// exact situation `push` fails to guard against.
    #[test]
    fn history_record_keeps_doc_revisions_in_sync() {
        let config = Arc::new(ArcSwap::new(Arc::new(Config::default())));
        let loader = Arc::new(ArcSwap::from_pointee(syntax::Loader::default()));

        // Revision 0: a short document.
        let mut doc = Document::from(Rope::from_str("ab"), None, config, loader);

        let mut view1 = View::new(doc.id(), GutterConfig::default());
        let mut view2 = View::new(doc.id(), GutterConfig::default());
        doc.ensure_view_init(view1.id);
        doc.ensure_view_init(view2.id);

        // Sync view1 at revision 0 so its `doc_revisions` records the short doc.
        view1.sync_changes(&mut doc);
        assert_eq!(doc.get_current_revision(), 0);

        // Commit an insertion *through view2*, growing the document and advancing
        // its history to revision 1. Only view2's `doc_revisions` is updated here;
        // view1 still believes the document is at revision 0.
        let insert = Transaction::change(
            doc.text(),
            std::iter::once((2, 2, Some("XXXXXXXXXX".into()))),
        );
        assert!(doc.apply(&insert, view2.id));
        doc.append_changes_to_history(&mut view2);
        assert_eq!(doc.get_current_revision(), 1);
        let len = doc.text().len_chars();
        assert_eq!(len, 12);

        // Push a jump into view1 at the end of the *current* (revision 1) document.
        // This selection is perfectly valid for the document as it stands now.
        //
        // `Editor::record_jump` syncs the view to the document's current revision
        // before recording, so the new place and `doc_revisions` agree. Without
        // the sync the place would be ahead of `doc_revisions` (still revision 0),
        // and the `sync_changes` below would map position 12 through the revision
        // 0 -> 1 changeset (pre-image only 2 chars) and panic in
        // `ChangeSet::update_positions`.
        view1.sync_changes(&mut doc);
        let place = crate::history::Place::Text {
            doc: doc.id(),
            selection: Selection::point(len),
        };
        view1.history.push(place, false);

        // With the fix in place this is a no-op for the freshly pushed entry and
        // the jump remains a valid, in-bounds selection.
        view1.sync_changes(&mut doc);

        let Some(crate::history::Place::Text { selection, .. }) =
            view1.history.places().next_back()
        else {
            unreachable!()
        };
        assert!(
            selection.primary().head <= doc.text().len_chars(),
            "history selection must stay within document bounds after sync",
        );
    }
}
