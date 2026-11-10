//! A side by side diff between the HEAD version of a file (left) and its current contents
//! (right). Changed lines are aligned, with filler rows where one side has no line.
//!
//! The text is read-only, but a diff of the working tree can stage, unstage and revert its hunks
//! (see `commands/hunks.rs`), and the diffs opened from a list of files (the uncommitted changes,
//! a commit, a range) go on to the next file with `n`/`N`.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use helix_core::syntax::{self, HighlightEvent};
use helix_core::unicode::width::UnicodeWidthChar;
use helix_core::{RopeSlice, Selection, Syntax};
use helix_vcs::staging::{self, HunkState, LineHunk};
use helix_vcs::FileChange;
use helix_view::editor::Action;
use helix_view::graphics::{Color, CursorKind, Modifier, Rect, Style};
use helix_view::input::{Event, KeyEvent, MouseEventKind};
use helix_view::keyboard::{KeyCode, KeyModifiers};
use helix_view::{align_view, current, Align, DocumentId, Editor, Theme};
use imara_diff::{Algorithm, Diff, InternedInput};
use tui::buffer::Buffer as Surface;

use crate::commands::hunks;
use crate::compositor::{Component, Compositor, Context, EventResult};
use crate::live::Live;

pub const ID: &str = "git-diff-view";

/// Unchanged lines kept visible above a hunk when jumping to it.
const HUNK_CONTEXT: usize = 3;

/// Word highlights are dropped when more than this fraction of a line pair changed: at that
/// point the whole line is different and highlighting every word is just noise.
const MAX_WORD_CHANGE_RATIO: f32 = 0.6;

/// One version of the file.
pub struct Side {
    title: String,
    text: String,
    /// Byte range of each line, without the line ending.
    lines: Vec<Range<usize>>,
    /// Syntax highlights of each line, as byte ranges relative to the start of the line.
    highlights: Vec<Vec<(Range<usize>, Style)>>,
}

impl Side {
    pub fn new(title: String, text: String, path: &Path, editor: &Editor) -> Side {
        let lines = line_ranges(&text);
        let highlights = highlight(
            &text,
            &lines,
            path,
            &editor.syn_loader.load(),
            &editor.theme,
        );
        Side {
            title,
            text,
            lines,
            highlights,
        }
    }

    fn line(&self, line: usize) -> &str {
        &self.text[self.lines[line].clone()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Equal,
    /// A removed line paired with the added line that replaced it.
    Changed,
    Removed,
    Added,
}

#[derive(Debug, PartialEq, Eq)]
struct Row {
    kind: RowKind,
    left: Option<usize>,
    right: Option<usize>,
    /// Changed byte ranges within the lines of a `Changed` row.
    left_words: Vec<Range<usize>>,
    right_words: Vec<Range<usize>>,
}

impl Row {
    fn new(kind: RowKind, left: Option<usize>, right: Option<usize>) -> Row {
        Row {
            kind,
            left,
            right,
            left_words: Vec::new(),
            right_words: Vec::new(),
        }
    }
}

/// The width of the longest line of both sides, for horizontal scrolling.
fn widest(left: &Side, right: &Side, tab_width: usize) -> usize {
    [left, right]
        .into_iter()
        .flat_map(|side| (0..side.lines.len()).map(move |line| side.line(line)))
        .map(|line| layout(line, tab_width, None, |_, _, _, _, _| true).1)
        .max()
        .unwrap_or(0)
}

/// `path` relative to the working directory, for messages.
fn relative(path: &Path) -> String {
    helix_stdx::path::get_relative_path(path)
        .display()
        .to_string()
}

/// Splits `text` like imara-diff's line tokenizer does, returning each line without its ending.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut start = 0;
    text.split_inclusive('\n')
        .map(|line| {
            let content = line.strip_suffix('\n').unwrap_or(line);
            let content = content.strip_suffix('\r').unwrap_or(content);
            let range = start..start + content.len();
            start += line.len();
            range
        })
        .collect()
}

/// Aligns the lines of `before` and `after`, returning the rows, the first row of each hunk and
/// the lines of each hunk (the hunks the staging functions of `helix_vcs` work with).
fn build_rows(before: &str, after: &str) -> (Vec<Row>, Vec<usize>, Vec<LineHunk>) {
    let before_lines = line_ranges(before);
    let after_lines = line_ranges(after);
    let line_hunks = staging::line_hunks(before, after);

    let mut rows = Vec::new();
    let mut hunks = Vec::new();
    let (mut b, mut a) = (0, 0);
    for hunk in &line_hunks {
        let (before_start, before_end) = (hunk.before.start as usize, hunk.before.end as usize);
        let (after_start, after_end) = (hunk.after.start as usize, hunk.after.end as usize);
        while b < before_start && a < after_start {
            rows.push(Row::new(RowKind::Equal, Some(b), Some(a)));
            b += 1;
            a += 1;
        }

        hunks.push(rows.len());
        let removed = before_end - before_start;
        let added = after_end - after_start;
        let paired = removed.min(added);
        for i in 0..paired {
            let (left, right) = (before_start + i, after_start + i);
            let (left_words, right_words) = word_diff(
                &before[before_lines[left].clone()],
                &after[after_lines[right].clone()],
            );
            rows.push(Row {
                kind: RowKind::Changed,
                left: Some(left),
                right: Some(right),
                left_words,
                right_words,
            });
        }
        for line in before_start + paired..before_end {
            rows.push(Row::new(RowKind::Removed, Some(line), None));
        }
        for line in after_start + paired..after_end {
            rows.push(Row::new(RowKind::Added, None, Some(line)));
        }
        b = before_end;
        a = after_end;
    }
    while b < before_lines.len() || a < after_lines.len() {
        let left = (b < before_lines.len()).then_some(b);
        let right = (a < after_lines.len()).then_some(a);
        rows.push(Row::new(RowKind::Equal, left, right));
        b += 1;
        a += 1;
    }
    (rows, hunks, line_hunks)
}

/// Splits a line into words, runs of whitespace and single punctuation characters.
fn word_tokens(line: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };

    let mut tokens: Vec<Range<usize>> = Vec::new();
    let mut previous = None;
    for (i, c) in line.char_indices() {
        let current = class(c);
        let extends = current != Class::Other && previous.as_ref() == Some(&current);
        match tokens.last_mut() {
            Some(last) if extends => last.end = i + c.len_utf8(),
            _ => tokens.push(i..i + c.len_utf8()),
        }
        previous = Some(current);
    }
    tokens
}

/// Returns the changed byte ranges of `left` and `right`, compared word by word.
fn word_diff(left: &str, right: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let left_tokens = word_tokens(left);
    let right_tokens = word_tokens(right);
    let mut input = InternedInput::default();
    input.update_before(left_tokens.iter().map(|range| &left[range.clone()]));
    input.update_after(right_tokens.iter().map(|range| &right[range.clone()]));
    // the histogram heuristic does not work well for short, repetitive tokens
    let mut diff = Diff::compute(Algorithm::Myers, &input);
    diff.postprocess_no_heuristic(&input);

    let mut left_words = Vec::new();
    let mut right_words = Vec::new();
    for hunk in diff.hunks() {
        if !hunk.before.is_empty() {
            left_words.push(
                left_tokens[hunk.before.start as usize].start
                    ..left_tokens[hunk.before.end as usize - 1].end,
            );
        }
        if !hunk.after.is_empty() {
            right_words.push(
                right_tokens[hunk.after.start as usize].start
                    ..right_tokens[hunk.after.end as usize - 1].end,
            );
        }
    }

    let changed: usize = left_words.iter().chain(&right_words).map(Range::len).sum();
    let total = left.len() + right.len();
    if total == 0 || changed as f32 > total as f32 * MAX_WORD_CHANGE_RATIO {
        return (Vec::new(), Vec::new());
    }
    (left_words, right_words)
}

/// Syntax highlights each line of `text`, using the language of `path`.
fn highlight(
    text: &str,
    lines: &[Range<usize>],
    path: &Path,
    loader: &syntax::Loader,
    theme: &Theme,
) -> Vec<Vec<(Range<usize>, Style)>> {
    let mut result = vec![Vec::new(); lines.len()];
    let slice = RopeSlice::from(text);
    let Some(syntax) = loader
        .language_for_filename(path)
        .or_else(|| loader.language_for_shebang(slice))
        .and_then(|language| Syntax::new(slice, language, loader).ok())
    else {
        return result;
    };

    let mut highlighter = syntax.highlighter(slice, loader, ..);
    let mut stack = Vec::new();
    let len = text.len() as u32;
    let mut pos = 0;
    let mut line = 0;
    while pos < len {
        if pos == highlighter.next_event_offset() {
            let (event, highlights) = highlighter.advance();
            if event == HighlightEvent::Refresh {
                stack.clear();
            }
            stack.extend(highlights);
        }
        let start = pos;
        pos = highlighter.next_event_offset().min(len);
        if pos < start {
            log::error!(
                "diff view: highlighter moved backwards in {}",
                path.display()
            );
            break;
        }
        if pos == start || stack.is_empty() {
            continue;
        }

        let style = stack.iter().fold(Style::default(), |acc, highlight| {
            acc.patch(theme.highlight(*highlight))
        });
        let (start, end) = (start as usize, pos as usize);
        while line + 1 < lines.len() && lines[line + 1].start <= start {
            line += 1;
        }
        let mut current = line;
        while current < lines.len() && lines[current].start < end {
            let range = &lines[current];
            let overlap = start.max(range.start)..end.min(range.end);
            if !overlap.is_empty() {
                result[current].push((
                    overlap.start - range.start..overlap.end - range.start,
                    style,
                ));
            }
            current += 1;
        }
    }
    result
}

/// Mixes `color` over `background`, if both are true colors.
fn blend(background: Option<Color>, color: Option<Color>, alpha: f32) -> Option<Color> {
    match (background?, color?) {
        (Color::Rgb(br, bg, bb), Color::Rgb(r, g, b)) => {
            let mix = |back: u8, front: u8| {
                (back as f32 + (front as f32 - back as f32) * alpha).round() as u8
            };
            Some(Color::Rgb(mix(br, r), mix(bg, g), mix(bb, b)))
        }
        _ => None,
    }
}

struct Palette {
    background: Style,
    text: Style,
    line_number: Style,
    filler: Style,
    separator: Style,
    title: Style,
    hint: Style,
    /// Line, word and sign styles for removed lines (left) and added lines (right).
    minus: ChangeStyle,
    plus: ChangeStyle,
    /// The same, quieter, for the hunks that are staged.
    minus_staged: ChangeStyle,
    plus_staged: ChangeStyle,
    /// The marks on the separator next to staged and partly staged hunks.
    staged: Style,
    partly_staged: Style,
    warning: Style,
}

struct ChangeStyle {
    line: Style,
    word: Style,
    sign: Style,
}

impl ChangeStyle {
    /// `alpha` is how much of the color of the change the background of a line takes.
    fn new(theme: &Theme, scope: &str, background: Option<Color>, alpha: f32) -> ChangeStyle {
        let color = theme.get(scope).fg;
        let sign = Style::default().fg(color.unwrap_or(Color::Reset));
        match (
            blend(background, color, alpha),
            blend(background, color, alpha * 8.0 / 3.0),
        ) {
            (Some(line), Some(word)) => ChangeStyle {
                line: Style::default().bg(line),
                word: Style::default().bg(word),
                sign,
            },
            // without true colors there is nothing to blend: color the text instead
            _ => ChangeStyle {
                line: Style::default(),
                word: sign.add_modifier(Modifier::REVERSED),
                sign,
            },
        }
    }
}

impl Palette {
    fn new(theme: &Theme) -> Palette {
        let background = theme.get("ui.background");
        Palette {
            background,
            text: theme.get("ui.text"),
            line_number: theme.get("ui.linenr"),
            filler: theme.get("ui.virtual.whitespace"),
            separator: theme.get("ui.window"),
            title: theme.get("ui.statusline").add_modifier(Modifier::BOLD),
            hint: theme.get("ui.statusline.inactive"),
            minus: ChangeStyle::new(theme, "diff.minus", background.bg, 0.15),
            plus: ChangeStyle::new(theme, "diff.plus", background.bg, 0.15),
            minus_staged: ChangeStyle::new(theme, "diff.minus", background.bg, 0.06),
            plus_staged: ChangeStyle::new(theme, "diff.plus", background.bg, 0.06),
            staged: Style::default()
                .fg(theme.get("diff.plus").fg.unwrap_or(Color::Reset))
                .add_modifier(Modifier::BOLD),
            partly_staged: Style::default()
                .fg(theme.get("diff.delta").fg.unwrap_or(Color::Reset))
                .add_modifier(Modifier::BOLD),
            warning: theme.get("warning"),
        }
    }
}

type OnOpen = Box<dyn FnOnce(&mut Compositor)>;

/// Both sides of a diff, built again after a change (see [`DiffView::with_rebuild`]).
pub struct Snapshot {
    pub left: Side,
    pub right: Side,
    /// The file Enter opens, if it exists.
    pub path: Option<PathBuf>,
    /// The index side of the file, for a diff that can stage.
    pub staging: Option<Staging>,
}

/// Builds the sides of the diff again, after the working tree changed or a hunk action.
pub type Rebuild = Box<dyn FnMut(&Editor) -> anyhow::Result<Snapshot>>;

/// What the hunk actions of a diff act on: the right side is the working tree (or a buffer).
pub struct Actions {
    pub target: Target,
    /// For a file of a git working tree, what staging needs. Without it there is no staging, and
    /// reverting goes back to the left side.
    pub staging: Option<Staging>,
}

/// What the right side of a diff that can be edited shows.
pub enum Target {
    /// A file of the working tree, or its buffer when it is open.
    File(PathBuf),
    /// A buffer without a file, diffed against its diff base (the `:practice` buffers).
    Buffer(DocumentId),
}

/// The git side of an uncommitted file.
#[derive(Clone)]
pub struct Staging {
    pub change: FileChange,
    /// Its version in the index, `None` when it isn't in the index (untracked).
    pub index: Option<String>,
    /// Whether HEAD has it (at its old path, for a rename).
    pub in_head: bool,
}

/// The files of an entry, see [`FileNav::files`].
pub type ListFiles = Rc<dyn Fn() -> Vec<(FileChange, bool)>>;
/// Builds the diff of another file of the entry, see [`FileNav::open`].
pub type OpenFile = Rc<dyn Fn(&mut Editor, &FileChange) -> anyhow::Result<DiffView>>;
/// Puts the cursor of the list on a file, see [`FileNav::select`].
pub type SelectFile = Rc<dyn Fn(&mut Compositor, &FileChange)>;

/// The other files of the entry a diff was opened from (the uncommitted changes, a commit or a
/// range), for `n`/`N` to go on to the next or previous file's hunks.
pub struct FileNav {
    /// The files of the entry in the order of its list, each with whether `n`/`N` skip it
    /// (lockfiles). Called when needed: the uncommitted changes follow the working tree.
    pub files: ListFiles,
    /// The file shown.
    pub current: FileChange,
    /// The diff of another file of the entry.
    pub open: OpenFile,
    /// Puts the cursor of the list on a file, when `n`/`N` go to it.
    pub select: SelectFile,
}

/// A question the next key answers.
enum Confirm {
    /// Deleting an untracked file: git can't bring it back.
    Delete(PathBuf),
}

pub struct DiffView {
    left: Side,
    right: Side,
    rows: Vec<Row>,
    hunks: Vec<usize>,
    /// The lines of each hunk, in the same order as `hunks`.
    line_hunks: Vec<LineHunk>,
    /// Whether each hunk is staged, for a diff that can stage; empty otherwise.
    states: Vec<HunkState>,
    /// File to open with Enter, if it still exists.
    path: Option<PathBuf>,
    /// Run when Enter opens the file, to close what the view was opened from.
    on_open: Option<OnOpen>,
    tab_width: usize,
    /// Long lines wrap instead of scrolling sideways, like `editor.soft-wrap`.
    wrap: bool,
    /// Columns of text on each side, from the last render.
    text_widths: (usize, usize),
    /// Width of the longest line of both sides.
    widest: usize,
    scroll: usize,
    /// Screen lines of the row at `scroll` scrolled out above the screen, when lines wrap. Lets a
    /// line taller than the screen be read whole.
    offset: usize,
    hscroll: usize,
    /// Screen lines of the diff, from the last render.
    height: usize,
    /// Rows of the diff that fit on screen, from the last render. Fewer than `height` when lines
    /// wrap.
    visible: usize,
    /// First key of a two key sequence (`]c` / `[c`, `gg`, `gh`...).
    pending: Option<char>,
    /// Index of the current hunk in `hunks`. Tracked explicitly because several hunks can map
    /// to the same scroll position (near the top or bottom, or when everything fits on screen).
    current: usize,
    /// Builds the sides again, after a hunk action or when `live` sees the working tree change.
    rebuild: Option<Rebuild>,
    live: Option<Live>,
    /// Stage, unstage and revert.
    actions: Option<Actions>,
    /// The other files of the entry, for `n`/`N`.
    nav: Option<FileNav>,
    confirm: Option<Confirm>,
}

impl DiffView {
    pub fn new(left: Side, right: Side, path: Option<PathBuf>, tab_width: usize) -> DiffView {
        let (rows, hunks, line_hunks) = build_rows(&left.text, &right.text);
        let scroll = hunks
            .first()
            .map_or(0, |hunk| hunk.saturating_sub(HUNK_CONTEXT));
        let tab_width = tab_width.max(1);
        let widest = widest(&left, &right, tab_width);
        DiffView {
            left,
            right,
            rows,
            hunks,
            line_hunks,
            states: Vec::new(),
            path,
            on_open: None,
            tab_width,
            wrap: false,
            text_widths: (0, 0),
            widest,
            scroll,
            offset: 0,
            hscroll: 0,
            height: 0,
            visible: 0,
            pending: None,
            current: 0,
            rebuild: None,
            live: None,
            actions: None,
            nav: None,
            confirm: None,
        }
    }

    /// Builds the sides again with `rebuild` after a hunk action.
    pub fn with_rebuild(
        mut self,
        rebuild: impl FnMut(&Editor) -> anyhow::Result<Snapshot> + 'static,
    ) -> DiffView {
        self.rebuild = Some(Box::new(rebuild));
        self
    }

    /// Builds the sides again whenever `live` sees the working tree change, keeping the lines on
    /// screen where they were. A diff that has no changes left stays open, empty. Needs
    /// [`DiffView::with_rebuild`].
    pub fn with_live(mut self, live: Live) -> DiffView {
        self.live = Some(live);
        self
    }

    /// Enables the hunk actions: revert, and stage and unstage with `actions.staging`.
    pub fn with_actions(mut self, actions: Actions) -> DiffView {
        self.actions = Some(actions);
        self.update_states();
        self
    }

    /// Lets `n`/`N` go on to the other files of the entry.
    pub fn with_nav(mut self, nav: FileNav) -> DiffView {
        self.nav = Some(nav);
        self
    }

    /// Whether each hunk is staged, from the index version of the file.
    fn update_states(&mut self) {
        self.states = match self.actions.as_ref().and_then(|a| a.staging.as_ref()) {
            Some(staging) => staging::hunk_states(
                &self.left.text,
                staging.index.as_deref().unwrap_or(""),
                &self.right.text,
                &self.line_hunks,
            ),
            None => Vec::new(),
        };
    }

    /// Rebuilds a live diff if the working tree changed.
    fn refresh(&mut self, editor: &Editor) {
        if self.live.as_mut().is_some_and(Live::changed) {
            self.reload(editor);
        }
    }

    /// Builds the sides again. An error (the file became binary...) keeps the diff as it was.
    fn reload(&mut self, editor: &Editor) {
        let Some(rebuild) = &mut self.rebuild else {
            return;
        };
        match rebuild(editor) {
            Ok(snapshot) => self.replace_sides(snapshot),
            Err(err) => log::debug!("diff view: {err}"),
        }
    }

    /// Shows new versions of both sides. The line at the top of the screen stays there (the same
    /// line number of the right side, or of the left side on a row without a right line), so
    /// does the horizontal scroll, and the current hunk is the one closest to it.
    fn replace_sides(&mut self, snapshot: Snapshot) {
        let Snapshot {
            left,
            right,
            path,
            staging,
        } = snapshot;
        let top = self.rows.get(self.scroll).map(|row| (row.left, row.right));
        let (rows, hunks, line_hunks) = build_rows(&left.text, &right.text);
        self.widest = widest(&left, &right, self.tab_width);
        (
            self.left,
            self.right,
            self.rows,
            self.hunks,
            self.line_hunks,
            self.path,
        ) = (left, right, rows, hunks, line_hunks, path);
        if let Some(actions) = &mut self.actions {
            if staging.is_some() {
                actions.staging = staging;
            }
        }
        self.update_states();
        let scroll = top
            .and_then(|(left, right)| match right {
                Some(right) => self.rows.iter().position(|row| row.right == Some(right)),
                None => self.rows.iter().position(|row| row.left == left),
            })
            .unwrap_or(self.scroll);
        self.set_position(scroll, self.offset);
        self.current = self.current.min(self.hunks.len().saturating_sub(1));
        self.follow_hunk();
    }

    /// Runs `on_open` on the compositor when Enter opens the file.
    pub fn with_on_open(mut self, on_open: impl FnOnce(&mut Compositor) + 'static) -> DiffView {
        self.on_open = Some(Box::new(on_open));
        self
    }

    /// Wraps long lines instead of scrolling them sideways.
    pub fn with_soft_wrap(mut self, wrap: bool) -> DiffView {
        self.wrap = wrap;
        self
    }

    /// Whether the two sides differ at all.
    pub fn has_changes(&self) -> bool {
        !self.hunks.is_empty()
    }

    /// The last scroll position, `scroll` and `offset`: the one that shows the end of the diff at
    /// the bottom of the screen.
    fn max_position(&self) -> (usize, usize) {
        let height = self.height.max(1);
        if !self.wrap {
            return (self.rows.len().saturating_sub(height), 0);
        }
        let mut lines = 0;
        for (index, row) in self.rows.iter().enumerate().rev() {
            lines += self.row_height(row);
            if lines > height {
                return (index, lines - height);
            }
        }
        (0, 0)
    }

    /// Sets the scroll position, no further than `max_position`.
    fn set_position(&mut self, scroll: usize, offset: usize) {
        (self.scroll, self.offset) = (scroll, offset).min(self.max_position());
    }

    /// Screen lines taken by `row`: more than one when one of its lines wraps.
    fn row_height(&self, row: &Row) -> usize {
        let (left_width, right_width) = self.text_widths;
        if !self.wrap || left_width == 0 || right_width == 0 {
            return 1;
        }
        let height = |side: &Side, line: Option<usize>, width: usize| {
            line.map_or(1, |line| {
                layout(
                    side.line(line),
                    self.tab_width,
                    Some(width),
                    |_, _, _, _, _| true,
                )
                .0
            })
        };
        height(&self.left, row.left, left_width).max(height(&self.right, row.right, right_width))
    }

    /// The sideways scroll that shows the end of the longest line.
    fn max_hscroll(&self) -> usize {
        let (left_width, right_width) = self.text_widths;
        self.widest.saturating_sub(left_width.min(right_width))
    }

    /// Scrolls by `delta` rows, or by screen lines when lines wrap.
    fn scroll_by(&mut self, delta: isize) {
        if !self.wrap {
            self.scroll_to(self.scroll.saturating_add_signed(delta));
            return;
        }
        let max = self.max_position();
        for _ in 0..delta.unsigned_abs() {
            if delta > 0 {
                if (self.scroll, self.offset) >= max {
                    break;
                }
                if self.offset + 1 < self.row_height(&self.rows[self.scroll]) {
                    self.offset += 1;
                } else {
                    self.scroll += 1;
                    self.offset = 0;
                }
            } else if self.offset > 0 {
                self.offset -= 1;
            } else if self.scroll > 0 {
                self.scroll -= 1;
                self.offset = self.row_height(&self.rows[self.scroll]) - 1;
            } else {
                break;
            }
        }
        self.follow_hunk();
    }

    fn scroll_to(&mut self, scroll: usize) {
        self.set_position(scroll, 0);
        self.follow_hunk();
    }

    /// Keeps the current hunk, unless it went out of view: then the first hunk in view (or the
    /// closest one) becomes the current one.
    fn follow_hunk(&mut self) {
        let in_view = |hunk: usize| hunk >= self.scroll && hunk < self.scroll + self.visible;
        if self
            .hunks
            .get(self.current)
            .is_some_and(|&hunk| in_view(hunk))
        {
            return;
        }
        self.current = match self.hunks.iter().position(|&hunk| hunk >= self.scroll) {
            Some(index) => index,
            None => self.hunks.len().saturating_sub(1),
        };
    }

    /// Makes hunk `index` the current one, and scrolls to it.
    fn go_to_hunk(&mut self, index: usize) {
        self.current = index.min(self.hunks.len().saturating_sub(1));
        if let Some(&hunk) = self.hunks.get(self.current) {
            self.set_position(hunk.saturating_sub(HUNK_CONTEXT), 0);
        }
    }

    /// Makes the hunk at `line` of the right side (from 0) the current one, or the first hunk
    /// after it, or the last hunk.
    pub fn go_to_line(&mut self, line: u32) {
        let index = self
            .line_hunks
            .iter()
            .position(|hunk| hunk.after.end > line || hunk.after.start >= line)
            .unwrap_or(usize::MAX);
        self.go_to_hunk(index);
    }

    /// `n`/`N`: the next or previous hunk. Past the last or first one, the first hunk of the next
    /// file of the entry or the last hunk of the previous one, around the ends of the entry.
    fn step_hunk(&mut self, forward: bool, editor: &mut Editor) -> EventResult {
        let last = self.hunks.len().saturating_sub(1);
        let inside = !self.hunks.is_empty()
            && if forward {
                self.current < last
            } else {
                self.current > 0
            };
        if inside {
            self.go_to_hunk(if forward {
                self.current + 1
            } else {
                self.current - 1
            });
            return EventResult::Consumed(None);
        }
        if let Some(result) = self.step_file(forward, editor) {
            return result;
        }
        // no other file with changes: around the ends of this one
        if !self.hunks.is_empty() {
            self.go_to_hunk(if forward { 0 } else { last });
            editor.set_status(if forward {
                "Wrapped around to the first hunk"
            } else {
                "Wrapped around to the last hunk"
            });
        }
        EventResult::Consumed(None)
    }

    /// Replaces the view with the diff of the next (or previous) file of the entry that has
    /// changes, at its first (or last) hunk. `None` when there is no other such file.
    fn step_file(&mut self, forward: bool, editor: &mut Editor) -> Option<EventResult> {
        let nav = self.nav.as_ref()?;
        let (list, open) = (nav.files.clone(), nav.open.clone());
        let files = list();
        let len = files.len() as isize;
        if len == 0 {
            return None;
        }
        // the files after (or before) the current one, or after the place it had when it is gone
        let current = files
            .iter()
            .position(|(change, _)| change.path() == nav.current.path());
        let (start, count) = match current {
            Some(pos) => (pos as isize + if forward { 1 } else { -1 }, len - 1),
            None => {
                let pos = files
                    .iter()
                    .position(|(change, _)| change.path() > nav.current.path())
                    .unwrap_or(files.len()) as isize;
                (if forward { pos } else { pos - 1 }, len)
            }
        };
        for step in 0..count {
            let raw = if forward { start + step } else { start - step };
            let (change, skip) = &files[raw.rem_euclid(len) as usize];
            if *skip {
                continue;
            }
            let Ok(mut view) = open(editor, change) else {
                continue;
            };
            if !view.has_changes() {
                continue;
            }
            let mut nav = self.nav.take()?;
            nav.current = change.clone();
            let select = nav.select.clone();
            view.nav = Some(nav);
            (view.height, view.visible, view.text_widths) =
                (self.height, self.visible, self.text_widths);
            view.go_to_hunk(if forward { 0 } else { usize::MAX });
            *self = view;
            if raw < 0 || raw >= len {
                editor.set_status(if forward {
                    "Wrapped around to the first file"
                } else {
                    "Wrapped around to the last file"
                });
            }
            let change = change.clone();
            return Some(EventResult::Consumed(Some(Box::new(
                move |compositor: &mut Compositor, _: &mut Context| select(compositor, &change),
            ))));
        }
        None
    }

    /// Runs a hunk action on the current hunk, or on the whole file, then shows the result.
    fn act(&mut self, op: hunks::Op, whole_file: bool, editor: &mut Editor) {
        let Some(actions) = &self.actions else {
            return;
        };
        if op != hunks::Op::Revert && actions.staging.is_none() {
            editor.set_status("Only the files of a git working tree can be staged");
            return;
        }
        let scope = if whole_file {
            hunks::Scope::File
        } else {
            match self.line_hunks.get(self.current) {
                Some(hunk) => hunks::Scope::Hunk(hunk.clone()),
                None => {
                    editor.set_status("No changes");
                    return;
                }
            }
        };
        match hunks::run(
            editor,
            actions,
            &self.left.text,
            &self.right.text,
            op,
            scope,
        ) {
            Ok(hunks::Outcome::Done(message)) => editor.set_status(message),
            Ok(hunks::Outcome::ConfirmDelete(path)) => {
                editor.set_warning(format!(
                    "Delete untracked {}? git can't bring it back. y deletes it, any other key keeps it",
                    relative(&path)
                ));
                self.confirm = Some(Confirm::Delete(path));
            }
            Err(err) => editor.set_error(err.to_string()),
        }
        self.reload(editor);
    }

    /// The answer to `confirm`.
    fn answer(&mut self, confirm: Confirm, key: &KeyEvent, editor: &mut Editor) -> EventResult {
        let Confirm::Delete(path) = confirm;
        if key.code != KeyCode::Char('y') || !key.modifiers.is_empty() {
            editor.set_status(format!("Kept {}", relative(&path)));
            return EventResult::Consumed(None);
        }
        match hunks::delete_untracked(editor, &path) {
            // nothing left to show
            Ok(message) => {
                editor.set_status(message);
                Self::close()
            }
            Err(err) => {
                editor.set_error(err.to_string());
                EventResult::Consumed(None)
            }
        }
    }

    /// The hunk `row` is part of.
    fn hunk_of_row(&self, row: usize) -> Option<usize> {
        let hunk = self
            .hunks
            .partition_point(|&start| start <= row)
            .checked_sub(1)?;
        (self.rows.get(row)?.kind != RowKind::Equal).then_some(hunk)
    }

    /// The number of the file among the files of the entry `n`/`N` go through, and how many there
    /// are.
    fn file_position(&self) -> Option<(usize, usize)> {
        let nav = self.nav.as_ref()?;
        let files: Vec<_> = (nav.files)()
            .into_iter()
            .filter(|(_, skip)| !skip)
            .collect();
        let index = files
            .iter()
            .position(|(change, _)| change.path() == nav.current.path())?;
        Some((index + 1, files.len()))
    }

    /// Rows of the current hunk.
    fn current_rows(&self) -> Range<usize> {
        let Some(&start) = self.hunks.get(self.current) else {
            return 0..0;
        };
        let len = self.rows[start..]
            .iter()
            .take_while(|row| row.kind != RowKind::Equal)
            .count();
        start..start + len
    }

    /// The current file line of the current hunk, or the closest one below it.
    fn current_right_line(&self) -> Option<usize> {
        self.rows[self.current_rows().start..]
            .iter()
            .find_map(|row| row.right)
            .or_else(|| self.rows.iter().rev().find_map(|row| row.right))
    }

    fn close() -> EventResult {
        EventResult::Consumed(Some(Box::new(|compositor: &mut Compositor, _| {
            compositor.remove(ID);
        })))
    }

    /// Closes the view and opens the file at the line shown at the top.
    fn open_file(&mut self) -> EventResult {
        let Some(path) = self.path.clone() else {
            return Self::close();
        };
        let line = self.current_right_line().unwrap_or(0);
        let on_open = self.on_open.take();
        EventResult::Consumed(Some(Box::new(move |compositor: &mut Compositor, cx| {
            compositor.remove(ID);
            if let Some(on_open) = on_open {
                on_open(compositor);
            }
            if let Err(err) = cx.editor.open(&path, Action::Replace) {
                cx.editor.set_error(format!("{err}"));
                return;
            }
            let (view, doc) = current!(cx.editor);
            let text = doc.text();
            let pos = text.line_to_char(line.min(text.len_lines().saturating_sub(1)));
            doc.set_selection(view.id, Selection::point(pos));
            align_view(doc, view, Align::Center);
        })))
    }

    fn handle_key(&mut self, key: &KeyEvent, editor: &mut Editor) -> EventResult {
        let half_page = (self.height / 2).max(1) as isize;
        let page = self.height.max(1) as isize;

        if let Some(confirm) = self.confirm.take() {
            return self.answer(confirm, key, editor);
        }

        if let Some(prefix) = self.pending.take() {
            match (prefix, key.code) {
                (']' | '[', KeyCode::Char('c')) => return self.step_hunk(prefix == ']', editor),
                ('g', KeyCode::Char('g')) => self.scroll_to(0),
                ('g', KeyCode::Char('e')) => self.scroll_to(usize::MAX),
                ('g', KeyCode::Char('h')) => self.hscroll = 0,
                ('g', KeyCode::Char('l')) => self.hscroll = self.max_hscroll(),
                _ => {}
            }
            return EventResult::Consumed(None);
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Self::close(),
            KeyCode::Char('c') if ctrl => return Self::close(),
            KeyCode::Enter => return self.open_file(),
            KeyCode::Char('d') if ctrl => self.scroll_by(half_page),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half_page),
            KeyCode::Char('f') if ctrl => self.scroll_by(page),
            KeyCode::Char('b') if ctrl => self.scroll_by(-page),
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Home => self.scroll_to(0),
            KeyCode::Char('G') | KeyCode::End => self.scroll_to(usize::MAX),
            KeyCode::Char('n') => return self.step_hunk(true, editor),
            KeyCode::Char('N') => return self.step_hunk(false, editor),
            KeyCode::Char('s') => self.act(hunks::Op::Stage, false, editor),
            KeyCode::Char('S') => self.act(hunks::Op::Stage, true, editor),
            KeyCode::Char('u') => self.act(hunks::Op::Unstage, false, editor),
            KeyCode::Char('U') => self.act(hunks::Op::Unstage, true, editor),
            KeyCode::Char('r') => self.act(hunks::Op::Revert, false, editor),
            KeyCode::Char('R') => self.act(hunks::Op::Revert, true, editor),
            KeyCode::Char(c @ (']' | '[' | 'g')) => self.pending = Some(c),
            KeyCode::Char('h') | KeyCode::Left => self.hscroll = self.hscroll.saturating_sub(4),
            KeyCode::Char('l') | KeyCode::Right => {
                self.hscroll = (self.hscroll + 4).min(self.max_hscroll())
            }
            KeyCode::Char('0') => self.hscroll = 0,
            KeyCode::Char('w') => {
                self.wrap = !self.wrap;
                self.hscroll = 0;
                self.scroll_to(self.scroll);
            }
            _ => {}
        }
        EventResult::Consumed(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_cell(
        &self,
        surface: &mut Surface,
        area: Rect,
        y: u16,
        skip: usize,
        height: u16,
        side: &Side,
        line: Option<usize>,
        kind: RowKind,
        words: &[Range<usize>],
        is_left: bool,
        is_current: bool,
        staged: bool,
        number_width: usize,
        palette: &Palette,
    ) {
        let Some(line) = line else {
            let filler = "╱".repeat(area.width as usize);
            for y in y..y + height {
                surface.set_stringn(area.x, y, &filler, area.width as usize, palette.filler);
            }
            return;
        };

        let change = match (kind, is_left, staged) {
            (RowKind::Equal, _, _) => None,
            (_, true, false) => Some(&palette.minus),
            (_, false, false) => Some(&palette.plus),
            (_, true, true) => Some(&palette.minus_staged),
            (_, false, true) => Some(&palette.plus_staged),
        };
        let line_style = change.map_or(Style::default(), |change| change.line);
        surface.set_style(Rect::new(area.x, y, area.width, height), line_style);

        let sign = match change {
            None => ' ',
            Some(_) if is_left => '-',
            Some(_) => '+',
        };
        let gutter = format!("{:>number_width$} ", line + 1);
        let sign_x = area.x + gutter.len() as u16;
        // the line number and the sign only on the first screen line of a wrapped line
        if skip == 0 {
            surface.set_stringn(
                area.x,
                y,
                &gutter,
                area.width as usize,
                match change {
                    Some(change) if is_current => change.sign.add_modifier(Modifier::BOLD),
                    _ => palette.line_number,
                }
                .patch(line_style),
            );
            if sign_x < area.right() {
                let sign_style = change.map_or(palette.line_number, |change| change.sign);
                surface.set_stringn(
                    sign_x,
                    y,
                    &sign.to_string(),
                    1,
                    sign_style.patch(line_style),
                );
            }
        }

        let text_x = sign_x + 2;
        if text_x >= area.right() {
            return;
        }
        let text_width = (area.right() - text_x) as usize;
        let base = palette.text.patch(line_style);
        let word_style = change.map_or(Style::default(), |change| change.word);
        let highlights = &side.highlights[line];
        let mut next_highlight = 0;
        let (wrap, hscroll) = match self.wrap {
            true => (Some(text_width), 0),
            false => (None, self.hscroll),
        };
        layout(
            side.line(line),
            self.tab_width,
            wrap,
            |byte, symbol, width, row, col| {
                if row >= skip + height as usize || col >= hscroll + text_width {
                    return false;
                }
                // only draw characters that are fully inside the scrolled window
                if row < skip || col < hscroll || col + width > hscroll + text_width {
                    return true;
                }
                while next_highlight < highlights.len() && highlights[next_highlight].0.end <= byte
                {
                    next_highlight += 1;
                }
                let mut style = base;
                if let Some((range, highlight)) = highlights.get(next_highlight) {
                    if range.contains(&byte) {
                        style = style.patch(*highlight);
                    }
                }
                if words.iter().any(|range| range.contains(&byte)) {
                    style = style.patch(word_style);
                }
                let x = text_x + (col - hscroll) as u16;
                surface.set_stringn(x, y + (row - skip) as u16, symbol, width, style);
                true
            },
        );
    }
}

/// Lays out the characters of `line` on screen, wrapping at `wrap` columns if set, and calls
/// `draw` with the byte offset of each one, what to draw (tabs become spaces), its width and its
/// screen line and column from the start of the line, until it returns `false`. Returns the screen
/// lines taken and the width of the last one.
fn layout(
    line: &str,
    tab_width: usize,
    wrap: Option<usize>,
    mut draw: impl FnMut(usize, &str, usize, usize, usize) -> bool,
) -> (usize, usize) {
    let mut row = 0;
    let mut col = 0;
    let mut buf = [0; 4];
    let mut tab = String::new();
    for (byte, c) in line.char_indices() {
        let width = |col: usize| match c {
            '\t' => tab_width - col % tab_width,
            c => c.width().unwrap_or(0),
        };
        let mut char_width = width(col);
        if char_width == 0 {
            continue;
        }
        if let Some(wrap) = wrap {
            if col > 0 && col + char_width > wrap {
                row += 1;
                col = 0;
                char_width = width(0);
            }
            if c == '\t' {
                char_width = char_width.min(wrap);
            }
        }
        let symbol = if c == '\t' {
            tab.clear();
            tab.extend(std::iter::repeat_n(' ', char_width));
            tab.as_str()
        } else {
            c.encode_utf8(&mut buf)
        };
        if !draw(byte, symbol, char_width, row, col) {
            break;
        }
        col += char_width;
    }
    (row + 1, col)
}

impl Component for DiffView {
    fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        match event {
            Event::Key(key) => self.handle_key(key, cx.editor),
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.scroll_by(3),
                    MouseEventKind::ScrollUp => self.scroll_by(-3),
                    _ => {}
                }
                EventResult::Consumed(None)
            }
            Event::Paste(_) => EventResult::Consumed(None),
            Event::Resize(..) | Event::IdleTimeout | Event::FocusGained | Event::FocusLost => {
                EventResult::Ignored(None)
            }
        }
    }

    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        self.refresh(cx.editor);
        // the last line stays the editor's, for its messages (what an action did...)
        let area = area.clip_bottom(1);
        let palette = Palette::new(&cx.editor.theme);
        surface.clear_with(area, palette.background);
        if area.height < 3 || area.width < 10 {
            return;
        }

        // the keys of the hunk actions get a line of their own
        let footer_height = if self.actions.is_some() { 2 } else { 1 };
        if area.height < 2 + footer_height {
            return;
        }
        let body = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height - 1 - footer_height,
        );

        let half = (area.width - 1) / 2;
        let left_area = Rect::new(area.x, body.y, half, body.height);
        let separator_x = area.x + half;
        let right_area = Rect::new(separator_x + 1, body.y, area.width - half - 1, body.height);
        let number_width = self
            .left
            .lines
            .len()
            .max(self.right.lines.len())
            .max(1)
            .to_string()
            .len();
        // the gutter: line number, a space, the sign and a space
        let text_width = |area: Rect| (area.width as usize).saturating_sub(number_width + 3);
        self.text_widths = (text_width(left_area), text_width(right_area));
        self.height = body.height as usize;
        // the widths may have changed, and the row heights with them
        self.set_position(self.scroll, self.offset);
        let first_height = self
            .rows
            .get(self.scroll)
            .map_or(1, |row| self.row_height(row));
        self.offset = self.offset.min(first_height - 1);
        self.hscroll = self.hscroll.min(self.max_hscroll());

        // header with the title of each side
        surface.set_style(Rect::new(area.x, area.y, area.width, 1), palette.title);
        surface.set_stringn(
            area.x + 1,
            area.y,
            &self.left.title,
            half.saturating_sub(1) as usize,
            palette.title,
        );
        surface.set_stringn(
            right_area.x + 1,
            area.y,
            &self.right.title,
            right_area.width.saturating_sub(1) as usize,
            palette.title,
        );

        let current_rows = self.current_rows();
        let mut y = body.y;
        let mut visible = 0;
        for (i, row) in self.rows[self.scroll..].iter().enumerate() {
            if y >= body.bottom() {
                break;
            }
            let skip = if i == 0 { self.offset } else { 0 };
            let height = (self.row_height(row) - skip).min((body.bottom() - y) as usize) as u16;
            let is_current = current_rows.contains(&(self.scroll + i));
            let state = self
                .hunk_of_row(self.scroll + i)
                .and_then(|hunk| self.states.get(hunk).copied());
            let staged = state == Some(HunkState::Staged);
            let (mark, mark_style) = match state {
                Some(HunkState::Staged) => ("┃", palette.staged),
                Some(HunkState::PartlyStaged) => ("╏", palette.partly_staged),
                _ => ("│", palette.separator),
            };
            for y in y..y + height {
                surface.set_stringn(separator_x, y, mark, 1, mark_style);
            }
            self.render_cell(
                surface,
                left_area,
                y,
                skip,
                height,
                &self.left,
                row.left,
                row.kind,
                &row.left_words,
                true,
                is_current,
                staged,
                number_width,
                &palette,
            );
            self.render_cell(
                surface,
                right_area,
                y,
                skip,
                height,
                &self.right,
                row.right,
                row.kind,
                &row.right_words,
                false,
                is_current,
                staged,
                number_width,
                &palette,
            );
            y += height;
            visible += 1;
        }
        self.visible = visible.max(1);
        for y in y..body.bottom() {
            surface.set_stringn(separator_x, y, "│", 1, palette.separator);
        }

        // footer with the position and the keys
        let footer_y = area.bottom() - footer_height;
        surface.set_style(
            Rect::new(area.x, footer_y, area.width, footer_height),
            palette.hint,
        );
        let mut position = match self.hunks.len() {
            0 => "no changes".to_string(),
            hunks => format!("hunk {}/{hunks}", self.current + 1),
        };
        match self.states.get(self.current) {
            Some(HunkState::Staged) => position.push_str(" staged"),
            Some(HunkState::PartlyStaged) => position.push_str(" partly staged"),
            _ => {}
        }
        if let Some((file, files)) = self.file_position() {
            position.push_str(&format!(" · file {file}/{files}"));
        }
        let navigation =
            "n/N next/prev hunk · j/k scroll · h/l pan · w wrap · enter open at hunk · q close";
        let lines = match &self.actions {
            None => vec![(format!(" {position} · {navigation}"), palette.hint)],
            Some(actions) => {
                let first = match &self.confirm {
                    Some(Confirm::Delete(path)) => (
                        format!(
                            " Delete untracked {}? y deletes it, any other key keeps it",
                            relative(path)
                        ),
                        palette.hint.patch(palette.warning),
                    ),
                    None if actions.staging.is_some() => (
                        format!(" {position} · s/u/r stage/unstage/revert hunk · S/U/R the file"),
                        palette.hint,
                    ),
                    None => (
                        format!(" {position} · r revert hunk · R the file"),
                        palette.hint,
                    ),
                };
                vec![first, (format!(" {navigation}"), palette.hint)]
            }
        };
        for (i, (line, style)) in lines.iter().enumerate() {
            surface.set_stringn(
                area.x,
                footer_y + i as u16,
                line,
                area.width as usize,
                *style,
            );
        }
    }

    fn cursor(&self, _area: Rect, _editor: &Editor) -> (Option<helix_core::Position>, CursorKind) {
        (None, CursorKind::Hidden)
    }

    fn id(&self) -> Option<&'static str> {
        Some(ID)
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod test {
    use super::*;

    fn kinds(rows: &[Row]) -> Vec<(RowKind, Option<usize>, Option<usize>)> {
        rows.iter()
            .map(|row| (row.kind, row.left, row.right))
            .collect()
    }

    fn positions(line: &str, wrap: Option<usize>) -> Vec<(String, usize, usize)> {
        let mut cells = Vec::new();
        layout(line, 4, wrap, |_, symbol, _, row, col| {
            cells.push((symbol.to_string(), row, col));
            true
        });
        cells
    }

    #[test]
    fn wraps_long_lines() {
        assert_eq!(layout("abcdefg", 4, Some(3), |_, _, _, _, _| true), (3, 1));
        assert_eq!(layout("abcdefg", 4, None, |_, _, _, _, _| true), (1, 7));
        assert_eq!(layout("", 4, Some(3), |_, _, _, _, _| true), (1, 0));
        // A wide character that doesn't fit goes to the next line whole.
        assert_eq!(
            positions("ab日", Some(3)),
            [("a".into(), 0, 0), ("b".into(), 0, 1), ("日".into(), 1, 0)]
        );
        // Tabs reach the next tab stop, and start over after a wrap.
        assert_eq!(
            positions("a\tb\t", Some(5)),
            [
                ("a".into(), 0, 0),
                ("   ".into(), 0, 1),
                ("b".into(), 0, 4),
                ("    ".into(), 1, 0)
            ]
        );
    }

    #[test]
    fn wrapped_rows_take_several_lines() {
        let editor_less_side = |text: &str| Side {
            title: String::new(),
            lines: line_ranges(text),
            highlights: vec![Vec::new(); line_ranges(text).len()],
            text: text.to_string(),
        };
        let before = "short\nshort\nshort\n";
        let after = "short\na much longer line\nshort\n";
        let mut view = DiffView::new(editor_less_side(before), editor_less_side(after), None, 4)
            .with_soft_wrap(true);
        view.text_widths = (6, 6);
        view.height = 3;
        let heights: Vec<usize> = view.rows.iter().map(|row| view.row_height(row)).collect();
        // The changed line is paired with the one it replaced.
        assert_eq!(heights, [1, 3, 1]);
        // The last 3 lines: the last 2 of row 1, and row 2.
        assert_eq!(view.max_position(), (1, 1));
        // Scrolling goes through the lines of a wrapped row, and stops at the end.
        view.scroll_to(0);
        view.scroll_by(1);
        assert_eq!((view.scroll, view.offset), (1, 0));
        view.scroll_by(5);
        assert_eq!((view.scroll, view.offset), (1, 1));
        view.scroll_by(-1);
        assert_eq!((view.scroll, view.offset), (1, 0));
        view.scroll_by(-1);
        assert_eq!((view.scroll, view.offset), (0, 0));
        view.scroll_to(usize::MAX);
        assert_eq!((view.scroll, view.offset), (1, 1));
        view.wrap = false;
        assert_eq!(view.max_position(), (0, 0));
        assert_eq!(view.max_hscroll(), "a much longer line".len() - 6);
    }

    #[test]
    fn splits_lines_like_imara() {
        assert_eq!(line_ranges("a\nbc\r\nd"), [0..1, 2..4, 6..7]);
        assert_eq!(line_ranges("a\n"), [0..1]);
        assert!(line_ranges("").is_empty());
    }

    #[test]
    fn aligns_rows() {
        use RowKind::*;
        let before = "fn main() {\n    let x = 1;\n    println!(x);\n}\n";
        let after = "fn main() {\n    let x = 2;\n    let y = 3;\n    println!(x);\n}\n";
        let (rows, hunks, _) = build_rows(before, after);
        assert_eq!(
            kinds(&rows),
            [
                (Equal, Some(0), Some(0)),
                (Changed, Some(1), Some(1)),
                (Added, None, Some(2)),
                (Equal, Some(2), Some(3)),
                (Equal, Some(3), Some(4)),
            ]
        );
        assert_eq!(hunks, [1]);
        // only the number changed
        assert_eq!(rows[1].left_words, [12..13]);
        assert_eq!(rows[1].right_words, [12..13]);
    }

    #[test]
    fn handles_new_and_deleted_files() {
        let (rows, hunks, _) = build_rows("", "a\nb\n");
        assert_eq!(
            kinds(&rows),
            [
                (RowKind::Added, None, Some(0)),
                (RowKind::Added, None, Some(1))
            ]
        );
        assert_eq!(hunks, [0]);

        let (rows, _, _) = build_rows("a\n", "");
        assert_eq!(kinds(&rows), [(RowKind::Removed, Some(0), None)]);

        let (rows, hunks, _) = build_rows("same\n", "same\n");
        assert_eq!(kinds(&rows), [(RowKind::Equal, Some(0), Some(0))]);
        assert!(hunks.is_empty());
    }

    #[test]
    fn word_diff_skips_completely_different_lines() {
        assert_eq!(word_diff("let a = 1;", "return foo;"), (vec![], vec![]));
        assert_eq!(word_diff("foo(bar)", "foo(baz)"), (vec![4..7], vec![4..7]));
    }
}
