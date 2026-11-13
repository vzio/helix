mod handlers;
mod query;
#[cfg(test)]
mod tests;

use crate::{
    alt,
    compositor::{self, Component, Compositor, Context, Event, EventResult},
    ctrl, key, shift,
    ui::{
        self,
        document::{render_document, LinePos, TextRenderer},
        picker::query::PickerQuery,
        text_decorations::DecorationManager,
        EditorView,
    },
};
use futures_util::future::BoxFuture;
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo};
use thiserror::Error;
use tokio::sync::mpsc::Sender;
use tui::{
    buffer::Buffer as Surface,
    layout::Constraint,
    text::{Span, Spans},
    widgets::{Block, BorderType, Cell, Row, Table},
};
use vortex_event::AsyncHook;

use tui::widgets::Widget;

use vortex_core::unicode::width::UnicodeWidthStr;

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::Read,
    path::Path,
    sync::{
        atomic::{self, AtomicUsize},
        Arc,
    },
};

use crate::ui::{Prompt, PromptEvent};
use vortex_core::{
    char_idx_at_visual_offset, fuzzy::MATCHER, movement::Direction,
    text_annotations::TextAnnotations, unicode::segmentation::UnicodeSegmentation, Position,
};
use vortex_view::{
    editor::Action,
    graphics::{CursorKind, Margin, Modifier, Rect},
    input::KeyEvent,
    theme::Style,
    view::ViewPosition,
    Document, DocumentId, Editor,
};

pub use self::handlers::PickerLookup;
use self::handlers::{DynamicQueryChange, DynamicQueryHandler, PreviewHighlightHandler};

/// The mode of a picker that has modes, see [`Picker::with_modes`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerMode {
    /// Letters are keys: they move in the list or are left to the owner of the picker.
    Normal,
    /// Letters are typed in the prompt, as in every other picker.
    Filter,
}

pub const ID: &str = "picker";

pub const MIN_AREA_WIDTH_FOR_PREVIEW: u16 = 72;
/// Biggest file size to preview in bytes
pub const MAX_FILE_SIZE_FOR_PREVIEW: u64 = 10 * 1024 * 1024;

#[derive(PartialEq, Eq, Hash)]
pub enum PathOrId<'a> {
    Id(DocumentId),
    Path(&'a Path),
}

impl<'a> From<&'a Path> for PathOrId<'a> {
    fn from(path: &'a Path) -> Self {
        Self::Path(path)
    }
}

impl From<DocumentId> for PathOrId<'_> {
    fn from(v: DocumentId) -> Self {
        Self::Id(v)
    }
}

type FileCallback<T> = Box<dyn for<'a> Fn(&'a Editor, &'a T) -> Option<FileLocation<'a>>>;
/// Draws the preview of an item inside the preview's frame, see [`Picker::with_drawn_preview`].
type DrawPreview<T, D> = Box<dyn Fn(&T, &D, &Editor, Rect, &mut Surface)>;

/// File path and range of lines (used to align and highlight lines)
pub type FileLocation<'a> = (PathOrId<'a>, Option<(usize, usize)>);

/// How much of a binary file its preview shows: more rows than a preview has.
const BINARY_PREVIEW_BYTES: u64 = 16 * 1024;

pub enum CachedPreview {
    Document(Box<Document>),
    Directory(Vec<(String, bool)>),
    /// A binary file, previewed in the hex view (this fork), which reads only what it shows.
    Binary(Box<vortex_view::hex::HexBuffer>),
    LargeFile,
    NotFound,
}

// We don't store this enum in the cache so as to avoid lifetime constraints
// from borrowing a document already opened in the editor.
pub enum Preview<'picker, 'editor> {
    Cached(&'picker CachedPreview),
    EditorDocument(&'editor Document),
}

impl Preview<'_, '_> {
    fn document(&self) -> Option<&Document> {
        match self {
            Preview::EditorDocument(doc) => Some(doc),
            Preview::Cached(CachedPreview::Document(doc)) => Some(doc),
            _ => None,
        }
    }

    /// The bytes of a binary file, or of a document in the hex view.
    fn hex(&self) -> Option<&vortex_view::hex::HexBuffer> {
        match self {
            Preview::EditorDocument(doc) => doc.hex(),
            Preview::Cached(CachedPreview::Document(doc)) => doc.hex(),
            Preview::Cached(CachedPreview::Binary(hex)) => Some(hex),
            _ => None,
        }
    }

    fn dir_content(&self) -> Option<&Vec<(String, bool)>> {
        match self {
            Preview::Cached(CachedPreview::Directory(dir_content)) => Some(dir_content),
            _ => None,
        }
    }

    /// Alternate text to show for the preview.
    fn placeholder(&self) -> &str {
        match *self {
            Self::EditorDocument(_) => "<Invalid file location>",
            Self::Cached(preview) => match preview {
                CachedPreview::Document(_) => "<Invalid file location>",
                CachedPreview::Directory(_) => "<Invalid directory location>",
                CachedPreview::Binary(_) => "<Binary file>",
                CachedPreview::LargeFile => "<File too large to preview>",
                CachedPreview::NotFound => "<File not found>",
            },
        }
    }
}

/// What the matcher calls from its own threads when it has new results.
fn redraw_notifier() -> Arc<dyn Fn() + Sync + Send> {
    // With the `integration` feature the event system keeps one set of globals for each Tokio
    // runtime (one for each application of the tests), and panics on a thread that isn't in one.
    #[cfg(feature = "integration")]
    {
        let runtime = tokio::runtime::Handle::try_current().ok();
        Arc::new(move || {
            let _guard = runtime.as_ref().map(|runtime| runtime.enter());
            vortex_event::request_redraw()
        })
    }
    #[cfg(not(feature = "integration"))]
    Arc::new(vortex_event::request_redraw)
}

fn inject_nucleo_item<T, D>(
    injector: &nucleo::Injector<T>,
    columns: &[Column<T, D>],
    item: T,
    editor_data: &D,
) {
    injector.push(item, |item, dst| {
        for (column, text) in columns.iter().filter(|column| column.filter).zip(dst) {
            *text = column.format_text(item, editor_data).into()
        }
    });
}

pub struct Injector<T, D> {
    dst: nucleo::Injector<T>,
    columns: Arc<[Column<T, D>]>,
    editor_data: Arc<D>,
    version: usize,
    picker_version: Arc<AtomicUsize>,
    /// A marker that requests a redraw when the injector drops.
    /// This marker causes the "running" indicator to disappear when a background job
    /// providing items is finished and drops. This could be wrapped in an [Arc] to ensure
    /// that the redraw is only requested when all Injectors drop for a Picker (which removes
    /// the "running" indicator) but the redraw handle is debounced so this is unnecessary.
    _redraw: vortex_event::RequestRedrawOnDrop,
}

impl<I, D> Clone for Injector<I, D> {
    fn clone(&self) -> Self {
        Injector {
            dst: self.dst.clone(),
            columns: self.columns.clone(),
            editor_data: self.editor_data.clone(),
            version: self.version,
            picker_version: self.picker_version.clone(),
            _redraw: vortex_event::RequestRedrawOnDrop,
        }
    }
}

#[derive(Error, Debug)]
#[error("picker has been shut down")]
pub struct InjectorShutdown;

impl<T, D> Injector<T, D> {
    pub fn push(&self, item: T) -> Result<(), InjectorShutdown> {
        if self.version != self.picker_version.load(atomic::Ordering::Relaxed) {
            return Err(InjectorShutdown);
        }

        inject_nucleo_item(&self.dst, &self.columns, item, &self.editor_data);
        Ok(())
    }
    /// Whether the picker was closed (or its items replaced): what is pushed is dropped. A
    /// background job that finds items rarely checks it to stop early.
    pub fn is_shut_down(&self) -> bool {
        self.version != self.picker_version.load(atomic::Ordering::Relaxed)
    }
}

type ColumnFormatFn<T, D> = for<'a> fn(&'a T, &'a D) -> Cell<'a>;

pub struct Column<T, D> {
    name: Arc<str>,
    format: ColumnFormatFn<T, D>,
    /// Whether the column should be passed to nucleo for matching and filtering.
    /// `DynamicPicker` uses this so that the dynamic column (for example regex in
    /// global search) is not used for filtering twice.
    filter: bool,
    hidden: bool,
}

impl<T, D> Column<T, D> {
    pub fn new(name: impl Into<Arc<str>>, format: ColumnFormatFn<T, D>) -> Self {
        Self {
            name: name.into(),
            format,
            filter: true,
            hidden: false,
        }
    }

    /// A column which does not display any contents
    pub fn hidden(name: impl Into<Arc<str>>) -> Self {
        let format = |_: &T, _: &D| unreachable!();

        Self {
            name: name.into(),
            format,
            filter: false,
            hidden: true,
        }
    }

    pub fn without_filtering(mut self) -> Self {
        self.filter = false;
        self
    }

    fn format<'a>(&self, item: &'a T, data: &'a D) -> Cell<'a> {
        (self.format)(item, data)
    }

    fn format_text<'a>(&self, item: &'a T, data: &'a D) -> Cow<'a, str> {
        let text: String = self.format(item, data).content.into();
        text.into()
    }
}

/// Returns a new list of options to replace the contents of the picker
/// when called with the current picker query,
type DynQueryCallback<T, D> =
    fn(&str, &mut Editor, Arc<D>, &Injector<T, D>) -> BoxFuture<'static, anyhow::Result<()>>;

pub struct Picker<T: 'static + Send + Sync, D: 'static> {
    columns: Arc<[Column<T, D>]>,
    primary_column: usize,
    editor_data: Arc<D>,
    version: Arc<AtomicUsize>,
    matcher: Nucleo<T>,

    /// Current height of the completions box
    completion_height: u16,

    cursor: u32,
    prompt: Prompt,
    query: PickerQuery,

    /// Whether to show the preview panel (default true)
    show_preview: bool,
    /// Constraints for tabular formatting
    widths: Vec<Constraint>,

    callback_fn: PickerCallback<T>,
    /// Whether picking a result records a jump in history (this fork); not for the command palette.
    records_jumps: bool,
    default_action: Action,

    pub truncate_start: bool,
    /// Caches paths to documents
    preview_cache: HashMap<Arc<Path>, CachedPreview>,
    read_buffer: Vec<u8>,
    /// Given an item in the picker, return the file path and line number to display.
    file_fn: Option<FileCallback<T>>,
    /// Draws the preview instead of showing a file, see [`Picker::with_drawn_preview`].
    draw_preview: Option<DrawPreview<T, D>>,
    /// An event handler for syntax highlighting the currently previewed file.
    preview_highlight_handler: Sender<Arc<Path>>,
    dynamic_query_handler: Option<Sender<DynamicQueryChange>>,
    /// Whether the owner of the picker filters and orders the items itself, from the query it
    /// reads with [`Picker::query_line`]: the prompt then doesn't filter them.
    external_filter: bool,
    /// Extra keys that run a handler on the selected item and close the picker.
    /// Extra keys, the handler to run on the selected item, and whether to close the picker.
    key_handlers: Vec<(KeyEvent, PickerKeyHandler<T>, bool)>,
    /// Rows that are section dividers, see [`Picker::with_dividers`].
    dividers: Option<Dividers<T, D>>,
    /// Rows that are drawn as marked, see [`Picker::with_marked_rows`].
    marked: Option<PickerMarked<T>>,
    /// The mode of the picker, if it has modes, see [`Picker::with_modes`].
    mode: Option<PickerMode>,
    /// `g` was typed in normal mode, the next key completes the command (`gg`, `ge`).
    goto: bool,
    /// Items listed before the others for some queries, see [`Picker::with_preferred`].
    preferred: Option<Preferred<T>>,
    /// The rows when they aren't the matcher's: with preferred items, or with a query in a
    /// picker with groups (see [`Picker::with_groups`]).
    order: Option<Vec<ListedRow>>,
    /// `w`/`b` in normal mode move between the groups of dividers, see
    /// [`Picker::with_section_keys`].
    section_keys: bool,
    /// The table has a header row with the names of the columns; the fork's lists don't, see
    /// [`Picker::without_header`].
    header: bool,
    /// A short text on the prompt's row, before the count (this fork): the totals of a list, a
    /// range, what is being loaded. Not the status line's.
    info: Option<String>,
    /// The title and the picker's own keys in the key help overlay, `?` in normal mode (this
    /// fork, see [`Picker::with_help`]).
    help: Option<(
        String,
        Vec<(&'static str, Vec<(&'static str, &'static str)>)>,
    )>,
    /// Breaks ties between matches of the same score: the lower rank first (this fork: recent
    /// commands in the palette), see [`Picker::with_tie_rank`].
    tie_rank: Option<Box<dyn Fn(&T) -> u32>>,
}

/// Items listed first for some queries (this fork): the commits whose hash starts with a
/// hash-looking query, in the commit log.
struct Preferred<T> {
    /// Whether the primary query is one that prefers items.
    applies: fn(&str) -> bool,
    /// Whether an item is preferred for the primary query.
    prefer: fn(&T, &str) -> bool,
}

/// A row when the rows aren't the matcher's.
#[derive(Clone, Copy)]
enum ListedRow {
    /// The item at this index among all the items.
    Item(u32),
    /// The item at this position among the items the matcher matched.
    Matched(u32),
}

/// The rows of a picker with these fields (borrowed alone, so that the rest of the picker can be
/// borrowed mutably meanwhile).
fn rows_of<'a, T: Send + Sync + 'static>(
    matcher: &'a Nucleo<T>,
    order: &'a Option<Vec<ListedRow>>,
) -> Rows<'a, T> {
    Rows {
        snapshot: matcher.snapshot(),
        order: order.as_deref(),
    }
}

/// The rows of a picker: the items that match the query, in the order they are listed in.
struct Rows<'a, T: Send + Sync + 'static> {
    snapshot: &'a nucleo::Snapshot<T>,
    order: Option<&'a [ListedRow]>,
}

impl<'a, T: Send + Sync + 'static> Rows<'a, T> {
    fn count(&self) -> u32 {
        match self.order {
            Some(order) => order.len() as u32,
            None => self.snapshot.matched_item_count(),
        }
    }

    fn get(&self, row: u32) -> Option<nucleo::Item<'a, T>> {
        match self.order {
            Some(order) => match *order.get(row as usize)? {
                ListedRow::Item(index) => self.snapshot.get_item(index),
                ListedRow::Matched(position) => self.snapshot.get_matched_item(position),
            },
            None => self.snapshot.get_matched_item(row),
        }
    }

    fn range(&self, rows: std::ops::Range<u32>) -> impl Iterator<Item = nucleo::Item<'a, T>> + '_ {
        rows.filter_map(|row| self.get(row))
    }
}

impl<T: 'static + Send + Sync, D: 'static + Send + Sync> Picker<T, D> {
    pub fn stream(
        columns: impl IntoIterator<Item = Column<T, D>>,
        editor_data: D,
    ) -> (Nucleo<T>, Injector<T, D>) {
        let columns: Arc<[_]> = columns.into_iter().collect();
        let matcher_columns = columns.iter().filter(|col| col.filter).count() as u32;
        assert!(matcher_columns > 0);
        let matcher = Nucleo::new(Config::DEFAULT, redraw_notifier(), None, matcher_columns);
        let streamer = Injector {
            dst: matcher.injector(),
            columns,
            editor_data: Arc::new(editor_data),
            version: 0,
            picker_version: Arc::new(AtomicUsize::new(0)),
            _redraw: vortex_event::RequestRedrawOnDrop,
        };
        (matcher, streamer)
    }

    pub fn new<C, O, F>(
        columns: C,
        primary_column: usize,
        options: O,
        editor_data: D,
        callback_fn: F,
    ) -> Self
    where
        C: IntoIterator<Item = Column<T, D>>,
        O: IntoIterator<Item = T>,
        F: Fn(&mut Context, &T, Action) + 'static,
    {
        let columns: Arc<[_]> = columns.into_iter().collect();
        let matcher_columns = columns
            .iter()
            .filter(|col: &&Column<T, D>| col.filter)
            .count() as u32;
        assert!(matcher_columns > 0);
        let matcher = Nucleo::new(Config::DEFAULT, redraw_notifier(), None, matcher_columns);
        let injector = matcher.injector();
        for item in options {
            inject_nucleo_item(&injector, &columns, item, &editor_data);
        }
        Self::with(
            matcher,
            columns,
            primary_column,
            Arc::new(editor_data),
            Arc::new(AtomicUsize::new(0)),
            callback_fn,
        )
    }

    pub fn with_stream(
        matcher: Nucleo<T>,
        primary_column: usize,
        injector: Injector<T, D>,
        callback_fn: impl Fn(&mut Context, &T, Action) + 'static,
    ) -> Self {
        Self::with(
            matcher,
            injector.columns,
            primary_column,
            injector.editor_data,
            injector.picker_version,
            callback_fn,
        )
    }

    fn with(
        matcher: Nucleo<T>,
        columns: Arc<[Column<T, D>]>,
        default_column: usize,
        editor_data: Arc<D>,
        version: Arc<AtomicUsize>,
        callback_fn: impl Fn(&mut Context, &T, Action) + 'static,
    ) -> Self {
        assert!(!columns.is_empty());

        let prompt = Prompt::new(
            "".into(),
            None,
            ui::completers::none,
            |_editor: &mut Context, _pattern: &str, _event: PromptEvent| {},
        );

        let widths = columns
            .iter()
            .map(|column| Constraint::Length(column.name.chars().count() as u16))
            .collect();

        let query = PickerQuery::new(columns.iter().map(|col| &col.name).cloned(), default_column);

        Self {
            columns,
            primary_column: default_column,
            matcher,
            editor_data,
            version,
            cursor: 0,
            prompt,
            query,
            truncate_start: true,
            show_preview: true,
            callback_fn: Box::new(callback_fn),
            records_jumps: true,
            default_action: Action::Replace,
            completion_height: 0,
            widths,
            preview_cache: HashMap::new(),
            read_buffer: Vec::with_capacity(1024),
            file_fn: None,
            draw_preview: None,
            preview_highlight_handler: PreviewHighlightHandler::<T, D>::default().spawn(),
            dynamic_query_handler: None,
            external_filter: false,
            key_handlers: Vec::new(),
            dividers: None,
            marked: None,
            mode: None,
            goto: false,
            preferred: None,
            order: None,
            section_keys: false,
            header: true,
            info: None,
            help: None,
            tie_rank: None,
        }
    }

    /// With a query, matches of the same score are listed by `rank`, the lowest first, before
    /// the matcher's own order (shorter first, then the order they were added). Items without a
    /// rank return `u32::MAX`.
    pub fn with_tie_rank(mut self, rank: impl Fn(&T) -> u32 + 'static) -> Self {
        self.tie_rank = Some(Box::new(rank));
        self
    }

    /// No header row with the names of the columns: the columns of the fork's lists are clear
    /// from what they show.
    pub fn without_header(mut self) -> Self {
        self.header = false;
        self
    }

    /// A short text on the prompt's row, before the count.
    pub fn with_info(mut self, info: impl Into<String>) -> Self {
        self.info = Some(info.into());
        self
    }

    /// Changes the text on the prompt's row (`None` removes it).
    pub fn set_info(&mut self, info: Option<String>) {
        self.info = info;
    }

    /// The text on the prompt's row.
    pub fn info(&self) -> Option<&str> {
        self.info.as_deref()
    }

    /// The picker's own keys for the key help overlay (`?` in normal mode, see
    /// [`Picker::with_modes`]), in groups, before the keys every list has.
    pub fn with_help(
        mut self,
        title: impl Into<String>,
        groups: Vec<(&'static str, Vec<(&'static str, &'static str)>)>,
    ) -> Self {
        self.help = Some((title.into(), groups));
        self
    }

    /// The key help overlay of the picker: its own keys, then those of every list.
    pub fn key_help(&self) -> crate::ui::keyhelp::KeyHelp {
        let (title, mut groups) = self
            .help
            .clone()
            .unwrap_or_else(|| ("Keys".to_string(), Vec::new()));
        let mut list = vec![
            ("j k, ↓ ↑", "move"),
            ("C-d C-u", "half a page down, up"),
            ("gg ge, G", "first, last"),
        ];
        if self.section_keys {
            list.push(("w b", "next, previous group"));
        }
        list.extend([
            ("s", "filter (Esc goes back)"),
            ("Esc", "clear the filter, else close"),
            ("q", "close"),
            ("?", "these keys"),
        ]);
        groups.push(("list", list));
        crate::ui::keyhelp::KeyHelp::new(title, &groups)
    }

    pub fn injector(&self) -> Injector<T, D> {
        Injector {
            dst: self.matcher.injector(),
            columns: self.columns.clone(),
            editor_data: self.editor_data.clone(),
            version: self.version.load(atomic::Ordering::Relaxed),
            picker_version: self.version.clone(),
            _redraw: vortex_event::RequestRedrawOnDrop,
        }
    }

    pub fn truncate_start(mut self, truncate_start: bool) -> Self {
        self.truncate_start = truncate_start;
        self
    }

    pub fn with_preview(
        mut self,
        preview_fn: impl for<'a> Fn(&'a Editor, &'a T) -> Option<FileLocation<'a>> + 'static,
    ) -> Self {
        self.file_fn = Some(Box::new(preview_fn));
        // assumption: if we have a preview we are matching paths... If this is ever
        // not true this could be a separate builder function
        self.matcher.update_config(Config::DEFAULT.match_paths());
        self
    }

    /// The preview of the selected item is drawn by `draw` inside the preview's frame (the area
    /// it gets is within the border and margins), instead of showing a file.
    pub fn with_drawn_preview(
        mut self,
        draw: impl Fn(&T, &D, &Editor, Rect, &mut Surface) + 'static,
    ) -> Self {
        self.draw_preview = Some(Box::new(draw));
        self
    }

    pub fn with_history_register(mut self, history_register: Option<char>) -> Self {
        self.prompt.with_history_register(history_register);
        self
    }

    pub fn with_initial_cursor(mut self, cursor: u32) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn with_dynamic_query(
        mut self,
        callback: DynQueryCallback<T, D>,
        debounce_ms: Option<u64>,
    ) -> Self {
        let handler = DynamicQueryHandler::new(callback, debounce_ms).spawn();
        let event = DynamicQueryChange {
            query: self.primary_query(),
            // Treat the initial query as a paste.
            is_paste: true,
        };
        vortex_event::send_blocking(&handler, event);
        self.dynamic_query_handler = Some(handler);
        self
    }

    pub fn with_default_action(mut self, action: Action) -> Self {
        self.default_action = action;
        self
    }

    /// Lists the items for which `prefer` is true first, for the primary queries for which
    /// `applies` is true, even when they don't match the query otherwise; the other items that
    /// match follow in the usual order. The commit log uses it to list the commits whose hash
    /// starts with the query first.
    pub fn with_preferred(
        mut self,
        applies: fn(&str) -> bool,
        prefer: fn(&T, &str) -> bool,
    ) -> Self {
        self.preferred = Some(Preferred { applies, prefer });
        self
    }

    /// The rows, as they are listed.
    fn rows(&self) -> Rows<'_, T> {
        rows_of(&self.matcher, &self.order)
    }

    /// Lists the rows again, for the query and the items there are now: with the preferred
    /// items first, or by group, or as the matcher has them.
    fn update_order(&mut self) {
        self.order = None;
        self.update_preferred();
        if self.order.is_none() {
            self.update_groups();
        }
        if self.order.is_none() {
            self.update_ties();
        }
        if self.order.is_some() {
            self.cursor = self.cursor.min(self.rows().count().saturating_sub(1));
        }
    }

    /// Finds the preferred items again, for the query and the items there are now.
    fn update_preferred(&mut self) {
        let query = self.primary_query();
        let Some(preferred) = &self.preferred else {
            return;
        };
        if !(preferred.applies)(&query) {
            return;
        }
        let snapshot = self.matcher.snapshot();
        let first: Vec<u32> = (0..snapshot.item_count())
            .filter(|&index| {
                snapshot
                    .get_item(index)
                    .is_some_and(|item| (preferred.prefer)(item.data, &query))
            })
            .collect();
        if first.is_empty() {
            return;
        }
        // the preferred items that the matcher matched too are only listed once, first
        let first_items: HashSet<*const T> = first
            .iter()
            .filter_map(|&index| snapshot.get_item(index))
            .map(|item| item.data as *const T)
            .collect();
        let mut order: Vec<ListedRow> = first.into_iter().map(ListedRow::Item).collect();
        order.extend(
            snapshot
                .matched_items(..)
                .enumerate()
                .filter(|(_, item)| !first_items.contains(&(item.data as *const T)))
                .map(|(position, _)| ListedRow::Matched(position as u32)),
        );
        self.order = Some(order);
    }

    /// With a query and a tie rank, lists the matches of the same score by rank.
    fn update_ties(&mut self) {
        let Some(rank) = &self.tie_rank else {
            return;
        };
        let snapshot = self.matcher.snapshot();
        if snapshot.pattern().is_empty() {
            return;
        }
        // the matcher keeps its scores to itself: the matches are scored again, only when some
        // have a rank
        let ranks: Vec<u32> = snapshot
            .matched_items(..)
            .map(|item| rank(item.data))
            .collect();
        if ranks.iter().all(|&rank| rank == u32::MAX) {
            return;
        }
        let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let pattern = snapshot.pattern();
        let scores: Vec<u32> = snapshot
            .matched_items(..)
            .map(|item| {
                pattern
                    .score(item.matcher_columns, &mut matcher)
                    .unwrap_or(0)
            })
            .collect();
        let mut positions: Vec<usize> = (0..ranks.len()).collect();
        // stable: the matcher's order stays within the same score and rank
        positions.sort_by_key(|&position| (std::cmp::Reverse(scores[position]), ranks[position]));
        self.order = Some(
            positions
                .into_iter()
                .map(|position| ListedRow::Matched(position as u32))
                .collect(),
        );
    }

    /// With a query, lists the rows of a picker with groups in the order they were added, each
    /// divider before the entries of its group that match, and only if some do.
    fn update_groups(&mut self) {
        let Some(dividers) = &self.dividers else {
            return;
        };
        let Some(is_entry) = &dividers.groups else {
            return;
        };
        let snapshot = self.matcher.snapshot();
        if snapshot.pattern().is_empty() {
            return;
        }
        let matched: HashSet<*const T> = snapshot
            .matched_items(..)
            .map(|item| item.data as *const T)
            .collect();
        let mut order = Vec::new();
        // the divider of the group being read, until an entry of it matches
        let mut divider = None;
        for index in 0..snapshot.item_count() {
            let Some(item) = snapshot.get_item(index) else {
                continue;
            };
            let is_match = matched.contains(&(item.data as *const T));
            if dividers.title(item.data, &self.editor_data).is_some() {
                if is_entry(item.data) {
                    // a group of its own
                    divider = None;
                    if is_match {
                        order.push(ListedRow::Item(index));
                    }
                } else {
                    divider = Some(index);
                }
            } else if is_match {
                if let Some(divider) = divider.take() {
                    order.push(ListedRow::Item(divider));
                }
                order.push(ListedRow::Item(index));
            }
        }
        self.order = Some(order);
    }

    /// The owner of the picker filters and orders the items itself: editing the prompt doesn't
    /// filter or reorder them, whatever is typed shows every item in the order they were given.
    /// The owner reads the query with [`Picker::query_line`] and gives the items that match to
    /// [`Picker::replace_items`].
    pub fn with_external_filter(mut self) -> Self {
        self.external_filter = true;
        self
    }

    /// The picker is not overlaid on its own but inside another component: `lookup` finds it in
    /// the compositor, which the highlighting of the previews needs to give them their syntax.
    pub fn with_lookup(mut self, lookup: PickerLookup<T, D>) -> Self {
        self.preview_highlight_handler = PreviewHighlightHandler::new(lookup).spawn();
        self
    }

    /// What is typed in the prompt.
    pub fn query_line(&self) -> &str {
        self.prompt.line()
    }

    /// Replaces the contents of the prompt, as if the new text was typed.
    pub fn set_query_line(&mut self, line: String, editor: &Editor) {
        self.prompt.set_line(line, editor);
        self.handle_prompt_change(false);
    }

    /// Replaces every item with `items`, in this order if the picker has an external filter, and
    /// selects the one at index `selected` (or the last one if there are fewer). The new items
    /// are ready to show when this returns, unless there are so many that matching takes longer
    /// than a frame.
    pub fn replace_items(&mut self, items: impl IntoIterator<Item = T>, selected: u32) {
        // Not clearing the snapshot here would keep the old items on screen until the matcher ran.
        self.matcher.restart(true);
        if let Some(dividers) = &mut self.dividers {
            dividers.scanned = 0;
            dividers.count = 0;
        }
        let injector = self.matcher.injector();
        let mut count = 0u32;
        for item in items {
            inject_nucleo_item(&injector, &self.columns, item, &self.editor_data);
            count += 1;
        }
        self.cursor = selected.min(count.saturating_sub(1));
        self.matcher.tick(10);
        // the rows of the new items, for the owner looking for one of them right away
        self.update_order();
    }

    /// Picking a result doesn't record a jump in history (this fork): the command palette, whose
    /// commands record their own.
    pub fn without_jumps(mut self) -> Self {
        self.records_jumps = false;
        self
    }

    /// Runs the callback on a picked result: a jump in history (this fork) when it lands
    /// somewhere else.
    fn pick(&self, ctx: &mut Context, option: &T, action: Action) {
        if !self.records_jumps {
            return (self.callback_fn)(ctx, option, action);
        }
        let from = ctx.editor.place();
        (self.callback_fn)(ctx, option, action);
        ctx.editor
            .record_jump(vortex_view::history::Jump::Pick, from);
    }

    /// Pressing `key` runs `handler` on the selected item and closes the picker.
    pub fn with_key_handler(
        mut self,
        key: KeyEvent,
        handler: impl Fn(&mut Context, &T) + 'static,
    ) -> Self {
        self.key_handlers.push((key, Box::new(handler), true));
        self
    }

    /// Pressing `key` runs `handler` on the selected item and keeps the picker open, so that
    /// whatever `handler` pushes on top of it goes back to the picker when closed.
    pub fn with_stacked_key_handler(
        mut self,
        key: KeyEvent,
        handler: impl Fn(&mut Context, &T) + 'static,
    ) -> Self {
        self.key_handlers.push((key, Box::new(handler), false));
        self
    }

    /// Items for which `divider` returns a title are not entries but dividers between groups of
    /// entries: they are drawn as a full-width line with the title, the cursor never rests on
    /// one (movements skip them and `selection` is never one) and they don't count in the
    /// number of items. Give them empty column texts so that they match nothing while the
    /// picker is filtered.
    pub fn with_dividers(
        mut self,
        divider: impl for<'a> Fn(&'a T, &'a D) -> Option<Cow<'a, str>> + 'static,
    ) -> Self {
        self.dividers = Some(Dividers {
            title_fn: Box::new(divider),
            groups: None,
            scanned: 0,
            count: 0,
        });
        self
    }

    /// The dividers (see [`Picker::with_dividers`], which must be called first) head groups:
    /// the entries after each, up to the next divider. With a query, the rows stay in the order
    /// they were added instead of the matcher's, and a divider is listed while any entry of its
    /// group matches (its own text doesn't matter). A divider for which `is_entry` returns true
    /// is a group of its own, without entries: it is drawn as a divider, but the cursor stops on
    /// it, it is counted as an item and it matches the query by its own text.
    pub fn with_groups(mut self, is_entry: impl Fn(&T) -> bool + 'static) -> Self {
        if let Some(dividers) = &mut self.dividers {
            dividers.groups = Some(Box::new(is_entry));
        }
        self
    }

    /// In normal mode (see [`Picker::with_modes`]), `w` moves the cursor to the first entry of
    /// the next group of dividers and `b` to the first entry of the group it is in, or of the
    /// previous group when it is there already. They don't wrap around the ends of the list.
    pub fn with_section_keys(mut self) -> Self {
        self.section_keys = true;
        self
    }

    /// Items for which `marked` returns true are drawn with the `ui.selection` style, unless the
    /// cursor is on them. The picker doesn't act on them: this is for components that keep
    /// their own selection of several items, see [`Picker::cursor_index`] and [`Picker::item`].
    pub fn with_marked_rows(mut self, marked: impl Fn(&T) -> bool + 'static) -> Self {
        self.marked = Some(Box::new(marked));
        self
    }

    /// The picker has two modes instead of always sending the letters typed to its prompt, like
    /// the normal and insert modes of the editor. It opens in [`PickerMode::Normal`], where `j`
    /// and `k` move the cursor, `gg`/`ge`/`G` go to the start/end, `s` enters filter mode, `Esc`
    /// clears the query if there is one and closes the picker otherwise, `q` closes it, and the
    /// other letters do nothing (they are for the owner of the picker to use, see
    /// [`Picker::pending_key`]); pasting is ignored. The keys of pickers without modes work in
    /// both modes. In [`PickerMode::Filter`] everything works as in a picker without modes, and
    /// `Esc` goes back to normal mode, keeping the query. The cursor of the prompt is only shown
    /// in filter mode.
    pub fn with_modes(mut self) -> Self {
        self.mode = Some(PickerMode::Normal);
        self
    }

    /// The mode of the picker, always [`PickerMode::Filter`] if it has no modes.
    pub fn mode(&self) -> PickerMode {
        self.mode.unwrap_or(PickerMode::Filter)
    }

    /// Changes the mode of a picker that has modes, see [`Picker::with_modes`].
    pub fn set_mode(&mut self, mode: PickerMode) {
        if self.mode.is_some() {
            self.mode = Some(mode);
            self.goto = false;
        }
    }

    /// Whether the picker is in the middle of a command of several keys in normal mode (`g` was
    /// typed): the owner should give it the next key as it is.
    pub fn pending_key(&self) -> bool {
        self.goto
    }

    /// The position of the cursor among the items that match the query.
    pub fn cursor_index(&self) -> u32 {
        self.cursor
    }

    /// Puts the cursor on the `cursor`th item that matches the query, or on the last one if there
    /// are fewer. The cursor moves on to the next entry if that's a divider.
    pub fn set_cursor_index(&mut self, cursor: u32) {
        let len = self.rows().count();
        self.cursor = cursor.min(len.saturating_sub(1));
        self.skip_dividers(Direction::Forward);
    }

    /// The `index`th item in the order they were added to the picker, whether it matches the
    /// query or not.
    pub fn item(&self, index: u32) -> Option<&T> {
        self.matcher
            .snapshot()
            .get_item(index)
            .map(|item| item.data)
    }

    /// The `index`th item that matches the query, in the order they are listed in.
    pub fn matched_item(&self, index: u32) -> Option<&T> {
        self.rows().get(index).map(|item| item.data)
    }

    /// Whether `item` is a divider that the cursor skips, see [`Picker::with_dividers`] and
    /// [`Picker::with_groups`].
    fn is_divider_item(&self, item: &T) -> bool {
        self.dividers
            .as_ref()
            .is_some_and(|dividers| dividers.skips(item, &self.editor_data))
    }

    /// Whether `item` is drawn as a divider, the cursor stopping on it or not.
    fn is_drawn_as_divider(&self, item: &T) -> bool {
        self.dividers
            .as_ref()
            .is_some_and(|dividers| dividers.title(item, &self.editor_data).is_some())
    }

    /// Moves the cursor to the first entry of the next group (`Forward`), or of the group it is
    /// in or else the previous one (`Backward`), see [`Picker::with_section_keys`].
    pub fn move_to_section(&mut self, direction: Direction) {
        let rows = self.rows();
        let len = rows.count();
        let divider_at = |at: u32| {
            rows.get(at)
                .is_some_and(|item| self.is_drawn_as_divider(item.data))
        };
        let entry_at = |at: u32| {
            rows.get(at)
                .is_some_and(|item| !self.is_divider_item(item.data))
        };
        // the first entry of the group that starts at `divider`
        let first_entry = |divider: u32| (divider..len).find(|&at| entry_at(at));
        let target = match direction {
            Direction::Forward => ((self.cursor + 1)..len)
                .find(|&at| divider_at(at))
                .and_then(first_entry),
            Direction::Backward => {
                let group = (0..=self.cursor.min(len.saturating_sub(1)))
                    .rev()
                    .find(|&at| divider_at(at));
                match group.and_then(first_entry) {
                    Some(first) if first < self.cursor => Some(first),
                    _ => group
                        .and_then(|group| (0..group).rev().find(|&at| divider_at(at)))
                        .and_then(first_entry),
                }
            }
        };
        if let Some(target) = target {
            self.cursor = target;
        }
    }

    /// If the cursor is on a divider, moves it in `direction` to the first entry, wrapping around
    /// the ends of the list. It stays where it is if there are no entries.
    fn skip_dividers(&mut self, direction: Direction) {
        if self.dividers.is_none() {
            return;
        }
        let rows = self.rows();
        let len = rows.count();
        let is_entry = |at: u32| {
            rows.get(at)
                .is_some_and(|item| !self.is_divider_item(item.data))
        };
        let mut cursor = self.cursor;
        for _ in 0..len {
            if is_entry(cursor) {
                self.cursor = cursor;
                return;
            }
            cursor = match direction {
                Direction::Forward => (cursor + 1) % len,
                Direction::Backward => (cursor + len - 1) % len,
            };
        }
    }

    /// Counts the new dividers, and moves the cursor off a divider that arrived under it or
    /// that a new query put under it.
    fn update_dividers(&mut self) {
        let Some(dividers) = &mut self.dividers else {
            return;
        };
        let snapshot = self.matcher.snapshot();
        let total = snapshot.item_count();
        for index in dividers.scanned..total {
            if let Some(item) = snapshot.get_item(index) {
                dividers.count += dividers.skips(item.data, &self.editor_data) as u32;
            }
        }
        dividers.scanned = total;
        self.skip_dividers(Direction::Forward);
    }

    /// The number of items that match the query and the number of items, without the dividers.
    fn counts(&self) -> (u32, u32) {
        let snapshot = self.matcher.snapshot();
        let rows = self.rows();
        let Some(dividers) = &self.dividers else {
            return (rows.count(), snapshot.item_count());
        };
        // dividers have no text, so they only match while there's no query (or a negated one),
        // or while an entry of their group does
        let matching = if snapshot.pattern().is_empty() {
            dividers.count
        } else {
            rows.range(0..rows.count())
                .filter(|item| dividers.skips(item.data, &self.editor_data))
                .count() as u32
        };
        (
            rows.count().saturating_sub(matching),
            snapshot.item_count().saturating_sub(dividers.count),
        )
    }

    /// Move the cursor by a number of lines, either down (`Forward`) or up (`Backward`)
    pub fn move_by(&mut self, amount: u32, direction: Direction) {
        let len = self.rows().count();

        if len == 0 {
            // No results, can't move.
            return;
        }

        match direction {
            Direction::Forward => {
                self.cursor = self.cursor.saturating_add(amount) % len;
            }
            Direction::Backward => {
                self.cursor = self.cursor.saturating_add(len).saturating_sub(amount) % len;
            }
        }
        self.skip_dividers(direction);
    }

    /// Move the cursor down by exactly one page. After the last page comes the first page.
    pub fn page_up(&mut self) {
        self.move_by(self.completion_height as u32, Direction::Backward);
    }

    /// Move the cursor up by exactly one page. After the first page comes the last page.
    pub fn page_down(&mut self) {
        self.move_by(self.completion_height as u32, Direction::Forward);
    }

    /// Move the cursor to the first entry
    pub fn to_start(&mut self) {
        self.cursor = 0;
        self.skip_dividers(Direction::Forward);
    }

    /// Move the cursor to the last entry
    pub fn to_end(&mut self) {
        self.cursor = self.rows().count().saturating_sub(1);
        self.skip_dividers(Direction::Backward);
    }

    pub fn selection(&self) -> Option<&T> {
        self.rows()
            .get(self.cursor)
            .map(|item| item.data)
            .filter(|item| !self.is_divider_item(item))
    }

    fn primary_query(&self) -> Arc<str> {
        self.query
            .get(&self.columns[self.primary_column].name)
            .cloned()
            .unwrap_or_else(|| "".into())
    }

    fn header_height(&self) -> u16 {
        if self.header && self.columns.len() > 1 {
            1
        } else {
            0
        }
    }

    pub fn toggle_preview(&mut self) {
        self.show_preview = !self.show_preview;
    }

    fn prompt_handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        if let EventResult::Consumed(_) = self.prompt.handle_event(event, cx) {
            self.handle_prompt_change(matches!(event, Event::Paste(_)));
        }
        EventResult::Consumed(None)
    }

    fn handle_prompt_change(&mut self, is_paste: bool) {
        if self.external_filter {
            return;
        }
        // TODO: better track how the pattern has changed
        let line = self.prompt.line();
        let old_query = self.query.parse(line);
        if self.query == old_query {
            return;
        }
        // If the query has meaningfully changed, reset the cursor to the top of the results.
        self.cursor = 0;
        // Have nucleo reparse each changed column.
        for (i, column) in self
            .columns
            .iter()
            .filter(|column| column.filter)
            .enumerate()
        {
            let pattern = self
                .query
                .get(&column.name)
                .map(|f| &**f)
                .unwrap_or_default();
            let old_pattern = old_query
                .get(&column.name)
                .map(|f| &**f)
                .unwrap_or_default();
            // Fastlane: most columns will remain unchanged after each edit.
            if pattern == old_pattern {
                continue;
            }
            let is_append = pattern.starts_with(old_pattern);
            self.matcher.pattern.reparse(
                i,
                pattern,
                CaseMatching::Smart,
                Normalization::Smart,
                is_append,
            );
        }
        // If this is a dynamic picker, notify the query hook that the primary
        // query might have been updated.
        if let Some(handler) = &self.dynamic_query_handler {
            let event = DynamicQueryChange {
                query: self.primary_query(),
                is_paste,
            };
            vortex_event::send_blocking(handler, event);
        }
    }

    /// Get (cached) preview for the currently selected item. If a document corresponding
    /// to the path is already open in the editor, it is used instead.
    fn get_preview<'picker, 'editor>(
        &'picker mut self,
        editor: &'editor Editor,
    ) -> Option<(Preview<'picker, 'editor>, Option<(usize, usize)>)> {
        let current = self.selection()?;
        let (path_or_id, range) = (self.file_fn.as_ref()?)(editor, current)?;

        match path_or_id {
            PathOrId::Path(path) => {
                if let Some(doc) = editor.document_by_path(path) {
                    return Some((Preview::EditorDocument(doc), range));
                }

                if self.preview_cache.contains_key(path) {
                    // NOTE: we use `HashMap::get_key_value` here instead of indexing so we can
                    // retrieve the `Arc<Path>` key. The `path` in scope here is a `&Path` and
                    // we can cheaply clone the key for the preview highlight handler.
                    let (path, preview) = self.preview_cache.get_key_value(path).unwrap();
                    // (Only for a language: highlighting a plain text file does nothing, but
                    // asking for it redraws the picker, which asks again.)
                    if matches!(preview, CachedPreview::Document(doc) if doc.syntax().is_none() && doc.language_config().is_some())
                    {
                        vortex_event::send_blocking(&self.preview_highlight_handler, path.clone());
                    }
                    return Some((Preview::Cached(preview), range));
                }

                let path: Arc<Path> = path.into();
                let preview = std::fs::metadata(&path)
                    .and_then(|metadata| {
                        if metadata.is_dir() {
                            let files = super::directory_content(&path, editor)?;
                            let file_names: Vec<_> = files
                                .iter()
                                .filter_map(|(file_path, is_dir)| {
                                    let name = file_path
                                        .strip_prefix(&path)
                                        .map(|p| Some(p.as_os_str()))
                                        .unwrap_or_else(|_| file_path.file_name())?
                                        .to_string_lossy();
                                    if *is_dir {
                                        Some((format!("{}/", name), true))
                                    } else {
                                        Some((name.into_owned(), false))
                                    }
                                })
                                .collect();
                            Ok(CachedPreview::Directory(file_names))
                        } else if metadata.is_file() {
                            let is_binary = std::fs::File::open(&path).and_then(|file| {
                                // Read up to 1kb to detect the content type
                                let n = file.take(1024).read_to_end(&mut self.read_buffer)?;
                                let is_binary = crate::is_binary(&self.read_buffer[..n]);
                                self.read_buffer.clear();
                                Ok(is_binary)
                            })?;
                            // its start in the hex view, whatever the size (read now, so that
                            // the cache keeps no file open)
                            if is_binary {
                                let mut start = Vec::new();
                                std::fs::File::open(&path)?
                                    .take(BINARY_PREVIEW_BYTES)
                                    .read_to_end(&mut start)?;
                                let hex = vortex_view::hex::HexBuffer::from_bytes(start);
                                return Ok(CachedPreview::Binary(Box::new(hex)));
                            }
                            if metadata.len() > MAX_FILE_SIZE_FOR_PREVIEW {
                                return Ok(CachedPreview::LargeFile);
                            }
                            let mut doc = Document::open(
                                &path,
                                None,
                                false,
                                editor.config.clone(),
                                editor.syn_loader.clone(),
                            )
                            .or(Err(std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                "Cannot open document",
                            )))?;
                            let loader = editor.syn_loader.load();
                            if let Some(language_config) = doc.detect_language_config(&loader) {
                                doc.language = Some(language_config);
                                // Asynchronously highlight the new document
                                vortex_event::send_blocking(
                                    &self.preview_highlight_handler,
                                    path.clone(),
                                );
                            }
                            Ok(CachedPreview::Document(Box::new(doc)))
                        } else {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                "Neither a dir, nor a file",
                            ))
                        }
                    })
                    .unwrap_or(CachedPreview::NotFound);
                self.preview_cache.insert(path.clone(), preview);
                Some((Preview::Cached(&self.preview_cache[&path]), range))
            }
            PathOrId::Id(id) => {
                let doc = editor.documents.get(&id).unwrap();
                Some((Preview::EditorDocument(doc), range))
            }
        }
    }

    fn render_picker(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let status = self.matcher.tick(10);
        self.update_order();
        if status.changed {
            self.cursor = self.cursor.min(self.rows().count().saturating_sub(1));
        }
        self.update_dividers();
        let (matched_count, item_count) = self.counts();
        let snapshot = self.matcher.snapshot();
        let rows_of_items = rows_of(&self.matcher, &self.order);

        let text_style = cx.editor.theme.get("ui.text");
        let selected = cx.editor.theme.get("ui.text.focus");
        let highlight_style = cx.editor.theme.get("special").add_modifier(Modifier::BOLD);

        // -- Render the frame:
        // clear area
        let background = cx.editor.theme.get("ui.background");
        surface.clear_with(area, background);

        const BLOCK: Block<'_> = Block::bordered();

        // calculate the inner area inside the box
        let inner = BLOCK.inner(area);

        BLOCK.render(area, surface);

        // -- Render the input bar:

        let count = format!(
            "{}{}/{}",
            if status.running || self.matcher.active_injectors() > 0 {
                "(running) "
            } else {
                ""
            },
            matched_count,
            item_count,
        );

        let area = inner.clip_left(1).with_height(1);
        // the info (this fork) before the count, cut to leave the prompt half the row
        let info = self.info.as_deref().map(|info| {
            let room = (area.width / 2).saturating_sub(count.len() as u16 + 2);
            crate::ui::modebar::cut(info, room)
        });
        let info_width = info.as_deref().map_or(0, |info| {
            vortex_core::unicode::width::UnicodeWidthStr::width(info) as u16 + 2
        });
        let line_area = area.clip_right(count.len() as u16 + 1 + info_width);

        // render the prompt first since it will clear its background
        self.prompt.render(line_area, surface, cx);

        surface.set_stringn(
            (area.x + area.width).saturating_sub(count.len() as u16 + 1),
            area.y,
            &count,
            (count.len()).min(area.width as usize),
            text_style,
        );
        if let Some(info) = info.filter(|info| !info.is_empty()) {
            let style = cx
                .editor
                .theme
                .try_get("ui.picker.info")
                .or_else(|| cx.editor.theme.try_get("ui.text.inactive"))
                .unwrap_or(text_style);
            surface.set_stringn(
                (area.x + area.width).saturating_sub(count.len() as u16 + info_width),
                area.y,
                &info,
                info_width as usize,
                style,
            );
        }

        // -- Separator
        let sep_style = cx.editor.theme.get("ui.background.separator");
        let borders = BorderType::line_symbols(BorderType::Plain);
        for x in inner.left()..inner.right() {
            if let Some(cell) = surface.get_mut(x, inner.y + 1) {
                cell.set_symbol(borders.horizontal).set_style(sep_style);
            }
        }

        // -- Render the contents:
        // subtract area of prompt from top
        let inner = inner.clip_top(2);
        let rows = inner.height.saturating_sub(self.header_height()) as u32;
        let offset = self.cursor - (self.cursor % std::cmp::max(1, rows));
        let cursor = self.cursor.saturating_sub(offset);
        let end = offset.saturating_add(rows).min(rows_of_items.count());
        let mut indices = Vec::new();
        let mut matcher = MATCHER.lock();
        matcher.config = Config::DEFAULT;
        if self.file_fn.is_some() {
            matcher.config.set_match_paths()
        }

        let options = rows_of_items.range(offset..end).map(|item| {
            let is_divider = self
                .dividers
                .as_ref()
                .is_some_and(|dividers| dividers.title(item.data, &self.editor_data).is_some());
            if is_divider {
                return Row::new(self.columns.iter().map(|_| Cell::default()));
            }
            let mut widths = self.widths.iter_mut();
            let mut matcher_index = 0;

            Row::new(self.columns.iter().map(|column| {
                if column.hidden {
                    return Cell::default();
                }

                let Some(Constraint::Length(max_width)) = widths.next() else {
                    unreachable!();
                };
                let mut cell = column.format(item.data, &self.editor_data);
                let width = if column.filter {
                    let matched = snapshot.pattern().column_pattern(matcher_index).indices(
                        item.matcher_columns[matcher_index].slice(..),
                        &mut matcher,
                        &mut indices,
                    );
                    // a preferred item may not match the query
                    if matched.is_none() {
                        indices.clear();
                    }
                    indices.sort_unstable();
                    indices.dedup();
                    let mut indices = indices.drain(..);
                    let mut next_highlight_idx = indices.next().unwrap_or(u32::MAX);
                    let mut span_list = Vec::new();
                    let mut current_span = String::new();
                    let mut current_style = Style::default();
                    let mut grapheme_idx = 0u32;
                    let mut width = 0;

                    let spans: &[Span] =
                        cell.content.lines.first().map_or(&[], |it| it.0.as_slice());
                    for span in spans {
                        // this looks like a bug on first glance, we are iterating
                        // graphemes but treating them as char indices. The reason that
                        // this is correct is that nucleo will only ever consider the first char
                        // of a grapheme (and discard the rest of the grapheme) so the indices
                        // returned by nucleo are essentially grapheme indecies
                        for grapheme in span.content.graphemes(true) {
                            let style = if grapheme_idx == next_highlight_idx {
                                next_highlight_idx = indices.next().unwrap_or(u32::MAX);
                                span.style.patch(highlight_style)
                            } else {
                                span.style
                            };
                            if style != current_style {
                                if !current_span.is_empty() {
                                    span_list.push(Span::styled(current_span, current_style))
                                }
                                current_span = String::new();
                                current_style = style;
                            }
                            current_span.push_str(grapheme);
                            grapheme_idx += 1;
                        }
                        width += span.width();
                    }

                    span_list.push(Span::styled(current_span, current_style));
                    cell = Cell::from(Spans::from(span_list));
                    matcher_index += 1;
                    width
                } else {
                    cell.content
                        .lines
                        .first()
                        .map(|line| line.width())
                        .unwrap_or_default()
                };

                if width as u16 > *max_width {
                    *max_width = width as u16;
                }

                cell
            }))
        });

        let mut table = Table::new(options)
            .style(text_style)
            .highlight_style(selected)
            .highlight_symbol(" > ")
            .column_spacing(1)
            .widths(&self.widths);

        // -- Header
        if self.header && self.columns.len() > 1 {
            let active_column = self.query.active_column(self.prompt.position());
            let header_style = cx.editor.theme.get("ui.picker.header");
            let header_column_style = cx.editor.theme.get("ui.picker.header.column");

            table = table.header(
                Row::new(self.columns.iter().map(|column| {
                    if column.hidden {
                        Cell::default()
                    } else {
                        let style =
                            if active_column.is_some_and(|name| Arc::ptr_eq(name, &column.name)) {
                                cx.editor.theme.get("ui.picker.header.column.active")
                            } else {
                                header_column_style
                            };

                        Cell::from(Span::styled(Cow::from(&*column.name), style))
                    }
                }))
                .style(header_style),
            );
        }

        use tui::widgets::TableState;

        table.render_table(
            inner,
            surface,
            &mut TableState {
                offset: 0,
                selected: Some(cursor as usize),
            },
            self.truncate_start,
        );

        // -- Marked rows and dividers, over the table:
        if self.marked.is_some() || self.dividers.is_some() {
            let marked_style = cx.editor.theme.get("ui.selection");
            let divider_style = cx
                .editor
                .theme
                .try_get_exact("ui.picker.divider")
                .or_else(|| cx.editor.theme.try_get_exact("ui.text.inactive"))
                .unwrap_or_else(|| cx.editor.theme.get("comment"));
            let first_row = inner.y + self.header_height();
            for (i, item) in rows_of_items.range(offset..end).enumerate() {
                let row = Rect::new(inner.x, first_row + i as u16, inner.width, 1);
                if row.bottom() > inner.bottom() {
                    break;
                }
                if let Some(title) = self
                    .dividers
                    .as_ref()
                    .and_then(|dividers| dividers.title(item.data, &self.editor_data))
                {
                    let width = row.width as usize;
                    let used = 4 + title.width();
                    // a divider the cursor stops on, under it, has the cursor's marker
                    let under_cursor = i as u32 == cursor;
                    let start = if under_cursor { " > " } else { "── " };
                    let rule = "─".repeat(width.saturating_sub(used));
                    let line = format!("{start}{title} {rule}");
                    let style = if under_cursor {
                        text_style.patch(divider_style).patch(selected)
                    } else {
                        text_style.patch(divider_style)
                    };
                    surface.clear_with(row, text_style);
                    surface.set_stringn(row.x, row.y, &line, width, style);
                } else if i as u32 != cursor && self.marked.as_ref().is_some_and(|f| f(item.data)) {
                    surface.set_style(row, marked_style);
                }
            }
        }
    }

    fn render_preview(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        // -- Render the frame:
        // clear area
        let background = cx.editor.theme.get("ui.background");
        let text = cx.editor.theme.get("ui.text");
        let directory = cx.editor.theme.get("ui.text.directory");
        surface.clear_with(area, background);

        const BLOCK: Block<'_> = Block::bordered();

        // calculate the inner area inside the box
        let inner = BLOCK.inner(area);
        // 1 column gap on either side
        let margin = Margin::horizontal(1);
        let inner = inner.inner(margin);
        BLOCK.render(area, surface);

        if let Some(draw) = &self.draw_preview {
            if let Some(item) = self.selection() {
                draw(item, &self.editor_data, cx.editor, inner, surface);
            }
            return;
        }

        if let Some((preview, range)) = self.get_preview(cx.editor) {
            if let Some(hex) = preview.hex() {
                // its first rows, with no cursor
                let cursor = vortex_view::hex::HexCursor {
                    offset: u64::MAX,
                    ..Default::default()
                };
                super::hex_view::render_rows(&cx.editor.theme, hex, &cursor, inner, surface, false);
                return;
            }
            let doc = match preview.document() {
                Some(doc)
                    if range.is_none_or(|(start, end)| {
                        start <= end && end <= doc.text().len_lines()
                    }) =>
                {
                    doc
                }
                _ => {
                    if let Some(dir_content) = preview.dir_content() {
                        for (i, (path, is_dir)) in
                            dir_content.iter().take(inner.height as usize).enumerate()
                        {
                            let style = if *is_dir { directory } else { text };
                            surface.set_stringn(
                                inner.x,
                                inner.y + i as u16,
                                path,
                                inner.width as usize,
                                style,
                            );
                        }
                        return;
                    }

                    let alt_text = preview.placeholder();
                    let x = inner.x + inner.width.saturating_sub(alt_text.len() as u16) / 2;
                    let y = inner.y + inner.height / 2;
                    surface.set_stringn(x, y, alt_text, inner.width as usize, text);
                    return;
                }
            };

            let mut offset = ViewPosition::default();
            if let Some((start_line, end_line)) = range {
                let height = end_line - start_line;
                let text = doc.text().slice(..);
                let start = text.line_to_char(start_line);
                let middle = text.line_to_char(start_line + height / 2);
                if height < inner.height as usize {
                    let text_fmt = doc.text_format(inner.width, None);
                    let annotations = TextAnnotations::default();
                    (offset.anchor, offset.vertical_offset) = char_idx_at_visual_offset(
                        text,
                        middle,
                        // align to middle
                        -(inner.height as isize / 2),
                        0,
                        &text_fmt,
                        &annotations,
                    );
                    if start < offset.anchor {
                        offset.anchor = start;
                        offset.vertical_offset = 0;
                    }
                } else {
                    offset.anchor = start;
                }
            }

            let loader = cx.editor.syn_loader.load();
            let config = cx.editor.config();

            let syntax_highlighter =
                EditorView::doc_syntax_highlighter(doc, offset.anchor, area.height, &loader);
            let mut overlay_highlights = Vec::new();
            if doc
                .language_config()
                .and_then(|config| config.rainbow_brackets)
                .unwrap_or(config.rainbow_brackets)
            {
                if let Some(overlay) = EditorView::doc_rainbow_highlights(
                    doc,
                    offset.anchor,
                    area.height,
                    &cx.editor.theme,
                    &loader,
                ) {
                    overlay_highlights.push(overlay);
                }
            }

            EditorView::doc_diagnostics_highlights_into(
                doc,
                &cx.editor.theme,
                &mut overlay_highlights,
            );

            let mut decorations = DecorationManager::default();

            if let Some((start, end)) = range {
                let style = cx
                    .editor
                    .theme
                    .try_get("ui.highlight")
                    .unwrap_or_else(|| cx.editor.theme.get("ui.selection"));
                let draw_highlight = move |renderer: &mut TextRenderer, pos: LinePos| {
                    if (start..=end).contains(&pos.doc_line) {
                        let area = Rect::new(
                            renderer.viewport.x,
                            pos.visual_line,
                            renderer.viewport.width,
                            1,
                        );
                        renderer.set_style(area, style)
                    }
                };
                decorations.add_decoration(draw_highlight);
            }

            render_document(
                surface,
                inner,
                doc,
                offset,
                // TODO: compute text annotations asynchronously here (like inlay hints)
                &TextAnnotations::default(),
                syntax_highlighter,
                overlay_highlights,
                &cx.editor.theme,
                decorations,
            );
        }
    }
}

impl<I: 'static + Send + Sync, D: 'static + Send + Sync> Component for Picker<I, D> {
    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        // +---------+ +---------+
        // |prompt   | |preview  |
        // +---------+ |         |
        // |picker   | |         |
        // |         | |         |
        // +---------+ +---------+

        let render_preview = self.show_preview
            && (self.file_fn.is_some() || self.draw_preview.is_some())
            && area.width > MIN_AREA_WIDTH_FOR_PREVIEW;

        let picker_width = if render_preview {
            area.width / 2
        } else {
            area.width
        };

        let picker_area = area.with_width(picker_width);
        self.render_picker(picker_area, surface, cx);

        if render_preview {
            let preview_area = area.clip_left(picker_width);
            self.render_preview(preview_area, surface, cx);
        }
    }

    fn handle_event(&mut self, event: &Event, ctx: &mut Context) -> EventResult {
        // TODO: keybinds for scrolling preview

        let key_event = match event {
            Event::Key(event) if self.mode.is_some() => {
                // A terminal may send `G` with shift, which no key of the picker has.
                let mut event = *event;
                ui::editor::canonicalize_key(&mut event);
                event
            }
            Event::Key(event) => *event,
            // There is no prompt to paste into in normal mode.
            Event::Paste(..) if self.mode == Some(PickerMode::Normal) => {
                return EventResult::Consumed(None)
            }
            Event::Paste(..) => return self.prompt_handle_event(event, ctx),
            Event::Resize(..) => return EventResult::Consumed(None),
            // Picker is a modal and should consume mouse events so clicks don't fall
            // through to the editor underneath
            Event::Mouse(_) => return EventResult::Consumed(None),
            _ => return EventResult::Ignored(None),
        };

        let close_fn = |picker: &mut Self| {
            // if the picker is very large don't store it as last_picker to avoid
            // excessive memory consumption
            let callback: compositor::Callback =
                if picker.matcher.snapshot().item_count() > 1_000_000 {
                    Box::new(|compositor: &mut Compositor, _ctx| {
                        // remove the layer
                        compositor.pop();
                    })
                } else {
                    // stop streaming in new items in the background, really we should
                    // be restarting the stream somehow once the picker gets
                    // reopened instead (like for an FS crawl) that would also remove the
                    // need for the special case above but that is pretty tricky
                    picker.version.fetch_add(1, atomic::Ordering::Relaxed);
                    Box::new(|compositor: &mut Compositor, _ctx| {
                        // remove the layer
                        compositor.last_picker = compositor.pop();
                    })
                };
            EventResult::Consumed(Some(callback))
        };

        let normal = self.mode == Some(PickerMode::Normal);
        // The key after `g` completes `gg`/`ge`, whatever it is.
        if normal && std::mem::take(&mut self.goto) {
            match key_event {
                key!('g') => self.to_start(),
                key!('e') => self.to_end(),
                _ => {}
            }
            return EventResult::Consumed(None);
        }

        if let Some((_, handler, close)) =
            self.key_handlers.iter().find(|(key, ..)| *key == key_event)
        {
            if let Some(option) = self.selection() {
                handler(ctx, option);
            }
            if !close {
                return EventResult::Consumed(None);
            }
            return close_fn(self);
        }

        match key_event {
            key!(Esc) if self.mode == Some(PickerMode::Filter) => {
                self.mode = Some(PickerMode::Normal);
            }
            key!('j') if normal => self.move_by(1, Direction::Forward),
            key!('k') if normal => self.move_by(1, Direction::Backward),
            key!('g') if normal => self.goto = true,
            key!('G') if normal => self.to_end(),
            key!('s') if normal => self.mode = Some(PickerMode::Filter),
            // the key help overlay (this fork); in filter mode `?` is typed
            key!('?') if normal => return EventResult::Consumed(Some(self.key_help().push())),
            key!('w') if normal && self.section_keys => self.move_to_section(Direction::Forward),
            key!('b') if normal && self.section_keys => self.move_to_section(Direction::Backward),
            key!(Esc) if normal && !self.prompt.line().is_empty() => {
                self.set_query_line(String::new(), ctx.editor);
            }
            key!(Esc) | key!('q') if normal => return close_fn(self),
            shift!(Tab) | key!(Up) | ctrl!('p') => {
                self.move_by(1, Direction::Backward);
            }
            key!(Tab) | key!(Down) | ctrl!('n') => {
                self.move_by(1, Direction::Forward);
            }
            key!(PageDown) | ctrl!('d') => {
                self.page_down();
            }
            key!(PageUp) | ctrl!('u') => {
                self.page_up();
            }
            key!(Home) => {
                self.to_start();
            }
            key!(End) => {
                self.to_end();
            }
            key!(Esc) | ctrl!('c') => return close_fn(self),
            alt!(Enter) => {
                if let Some(option) = self.selection() {
                    self.pick(ctx, option, self.default_action);
                }
            }
            key!(Enter) => {
                // If the prompt has a history completion and is empty, use enter to accept
                // that completion
                if let Some(completion) = self
                    .prompt
                    .first_history_completion(ctx.editor)
                    .filter(|_| self.prompt.line().is_empty())
                {
                    // The percent character is used by the query language and needs to be
                    // escaped with a backslash.
                    let completion = if completion.contains('%') {
                        completion.replace('%', "\\%")
                    } else {
                        completion.into_owned()
                    };
                    self.prompt.set_line(completion, ctx.editor);

                    // Inserting from the history register is a paste.
                    self.handle_prompt_change(true);
                } else {
                    if let Some(option) = self.selection() {
                        self.pick(ctx, option, self.default_action);
                    }
                    if let Some(history_register) = self.prompt.history_register() {
                        if let Err(err) = ctx
                            .editor
                            .registers
                            .push(history_register, self.primary_query().to_string())
                        {
                            ctx.editor.set_error(err.to_string());
                        }
                    }
                    return close_fn(self);
                }
            }
            ctrl!('s') => {
                if let Some(option) = self.selection() {
                    self.pick(ctx, option, Action::HorizontalSplit);
                }
                return close_fn(self);
            }
            ctrl!('v') => {
                if let Some(option) = self.selection() {
                    self.pick(ctx, option, Action::VerticalSplit);
                }
                return close_fn(self);
            }
            ctrl!('t') => {
                self.toggle_preview();
            }
            _ if normal => {}
            _ => {
                self.prompt_handle_event(event, ctx);
            }
        }

        EventResult::Consumed(None)
    }

    fn cursor(&self, area: Rect, editor: &Editor) -> (Option<Position>, CursorKind) {
        if self.mode == Some(PickerMode::Normal) {
            return (None, CursorKind::Hidden);
        }
        let block = Block::bordered();
        // calculate the inner area inside the box
        let inner = block.inner(area);

        // prompt area
        let render_preview =
            self.show_preview && self.file_fn.is_some() && area.width > MIN_AREA_WIDTH_FOR_PREVIEW;

        let picker_width = if render_preview {
            area.width / 2
        } else {
            area.width
        };
        let area = inner.clip_left(1).with_height(1).with_width(picker_width);

        self.prompt.cursor(area, editor)
    }

    fn required_size(&mut self, (width, height): (u16, u16)) -> Option<(u16, u16)> {
        self.completion_height = height.saturating_sub(4 + self.header_height());
        Some((width, height))
    }

    fn id(&self) -> Option<&'static str> {
        Some(ID)
    }
}
impl<T: 'static + Send + Sync, D> Drop for Picker<T, D> {
    fn drop(&mut self) {
        // ensure we cancel any ongoing background threads streaming into the picker
        self.version.fetch_add(1, atomic::Ordering::Relaxed);
    }
}

/// The dividers of a picker, see [`Picker::with_dividers`].
struct Dividers<T, D> {
    title_fn: DividerTitle<T, D>,
    /// Whether a divider is an entry, when the dividers head groups (see
    /// [`Picker::with_groups`]).
    groups: Option<IsEntry<T>>,
    /// How many items (in the order they were added) are counted in `count` already.
    scanned: u32,
    /// How many of them are dividers.
    count: u32,
}

impl<T, D> Dividers<T, D> {
    /// The title of `item` if it is a divider.
    fn title<'a>(&self, item: &'a T, data: &'a D) -> Option<Cow<'a, str>> {
        (self.title_fn)(item, data)
    }

    /// Whether `item` is a divider that the cursor skips: not an entry.
    fn skips(&self, item: &T, data: &D) -> bool {
        self.title(item, data).is_some()
            && !self.groups.as_ref().is_some_and(|is_entry| is_entry(item))
    }
}

type PickerCallback<T> = Box<dyn Fn(&mut Context, &T, Action)>;
type PickerKeyHandler<T> = Box<dyn Fn(&mut Context, &T)>;
type PickerMarked<T> = Box<dyn Fn(&T) -> bool>;
type IsEntry<T> = Box<dyn Fn(&T) -> bool>;
type DividerTitle<T, D> = Box<dyn for<'a> Fn(&'a T, &'a D) -> Option<Cow<'a, str>>>;
