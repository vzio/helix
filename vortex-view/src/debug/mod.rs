//! The state of the debug panel (fork feature): the session's status, the variables tree, the
//! watch expressions, the console, and where the cursor is in each section. `vortex-term` loads
//! it from the debug adapter, draws it and sends it keys; this module turns it into rows.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use vortex_dap::{registry::DebugAdapterId, StackFrame, Thread, ThreadId};

pub mod config;
pub mod engine;
pub mod history;
pub mod lineline;
pub mod spectator;
pub mod timing;

pub use spectator::{Spectator, ThreadKey};

use crate::editor::Breakpoint;
use crate::graphics::Rect;

/// The sections of the panel, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Section {
    #[default]
    Variables,
    Watch,
    Stack,
    Breakpoints,
    Console,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Variables,
        Section::Watch,
        Section::Stack,
        Section::Breakpoints,
        Section::Console,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Variables => "Variables",
            Section::Watch => "Watch",
            Section::Stack => "Call stack",
            Section::Breakpoints => "Breakpoints",
            Section::Console => "Console",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// The section `by` tabs away, wrapping around.
    pub fn cycle(self, by: isize) -> Section {
        let len = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + by).rem_euclid(len) as usize]
    }
}

/// What the debugged program does, for the statusline and the panel's title.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    /// No session, and none ended yet.
    #[default]
    Idle,
    /// Launched or attached; not configured yet.
    Starting,
    Running,
    Stopped {
        reason: String,
        description: Option<String>,
    },
    /// The session ended; `code` is the exit code when the adapter reported one.
    Ended {
        code: Option<i64>,
    },
}

impl Status {
    pub fn is_active(&self) -> bool {
        !matches!(self, Status::Idle | Status::Ended { .. })
    }

    /// Short words for the statusline: `running`, `stopped: breakpoint`, `exited 3`.
    pub fn label(&self) -> String {
        match self {
            Status::Idle => String::new(),
            Status::Starting => "starting".into(),
            Status::Running => "running".into(),
            Status::Stopped { reason, .. } => format!("stopped: {reason}"),
            Status::Ended { code: Some(code) } => format!("exited {code}"),
            Status::Ended { code: None } => "ended".into(),
        }
    }
}

/// A scope, a variable, or a watch expression, with its children once they were loaded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarNode {
    pub name: String,
    pub value: String,
    pub ty: Option<String>,
    /// The adapter's `variablesReference`: non-zero when the node has children. It changes at
    /// every stop, so the tree is reloaded by path, never by reference.
    pub reference: usize,
    /// The expression that gives this variable, for watching it or `setExpression`.
    pub evaluate_name: Option<String>,
    /// `None` until loaded.
    pub children: Option<Vec<VarNode>>,
    /// The value differs from the one at the previous stop.
    pub changed: bool,
    /// A scope the adapter says is expensive to load: only loaded when expanded.
    pub expensive: bool,
    /// An evaluation that failed: `value` holds the message.
    pub error: bool,
    /// The adapter's `memoryReference`: the value points to memory that can be shown.
    pub memory_reference: Option<String>,
}

impl VarNode {
    pub fn has_children(&self) -> bool {
        self.reference > 0
    }

    /// About how much memory the node and its loaded children take.
    pub fn approx_bytes(&self) -> usize {
        std::mem::size_of::<VarNode>()
            + self.name.len()
            + self.value.len()
            + self.ty.as_ref().map_or(0, String::len)
            + self.evaluate_name.as_ref().map_or(0, String::len)
            + self
                .children
                .iter()
                .flatten()
                .map(VarNode::approx_bytes)
                .sum::<usize>()
    }

    fn find(&self, path: &[String]) -> Option<&VarNode> {
        match path.split_first() {
            None => Some(self),
            Some((first, rest)) => self
                .children
                .as_ref()?
                .iter()
                .find(|child| &child.name == first)?
                .find(rest),
        }
    }

    fn find_mut(&mut self, path: &[String]) -> Option<&mut VarNode> {
        match path.split_first() {
            None => Some(self),
            Some((first, rest)) => self
                .children
                .as_mut()?
                .iter_mut()
                .find(|child| &child.name == first)?
                .find_mut(rest),
        }
    }
}

/// Which tree a path points into.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Tree {
    /// The scopes of the selected frame.
    Scopes,
    /// The watch expression at this index.
    Watch(usize),
}

/// A node of the variables or the watch tree: the names from the scope (or the expression)
/// down to the node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VarPath {
    pub tree: Tree,
    pub names: Vec<String>,
}

/// What a row of the panel stands for, so keys on it know what to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Var(VarPath),
    Thread(ThreadKey),
    Frame {
        key: ThreadKey,
        index: usize,
    },
    Breakpoint {
        path: PathBuf,
        line: usize,
    },
    ConsoleLine(usize),
    /// A hint that stands for nothing (an empty section).
    None,
}

/// The badge of a value whose memory `M` shows.
pub const MEMORY_BADGE: &str = "▦";

/// How a piece of a row is drawn; `vortex-term` maps these to theme scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Name,
    Value,
    Changed,
    Type,
    Dim,
    Error,
    Heading,
    Current,
    Breakpoint,
    Unverified,
    Disabled,
    Stdout,
    Stderr,
    Info,
    Input,
    /// A frozen thread's badge.
    Frozen,
    /// The rewind marker.
    Rewind,
    /// The badge of a value whose memory can be shown.
    Memory,
    /// In rewind, a frame whose variables were carried from an earlier stop.
    Carried,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    None,
    Collapsed,
    Expanded,
    /// The current frame, or the current thread.
    Current,
}

/// One line of a section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub depth: u16,
    pub marker: Marker,
    pub parts: Vec<(String, Role)>,
    pub target: Target,
}

impl Row {
    fn hint(text: impl Into<String>) -> Row {
        Row {
            depth: 0,
            marker: Marker::None,
            parts: vec![(text.into(), Role::Dim)],
            target: Target::None,
        }
    }

    /// The row's text, for searching.
    pub fn text(&self) -> String {
        self.parts.iter().map(|(text, _)| text.as_str()).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Stdout,
    Stderr,
    /// The adapter's own messages (`console`, `important`).
    Info,
    /// An expression typed in the console.
    Input,
    /// Its value.
    Result,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub kind: LineKind,
    pub text: String,
}

/// The program's and the adapter's output, and the expressions evaluated in the console.
#[derive(Debug, Default)]
pub struct Console {
    pub lines: VecDeque<ConsoleLine>,
    /// The last line hasn't ended yet: the next output of the same kind continues it.
    open: bool,
}

impl Console {
    /// Lines kept; older ones are dropped.
    pub const LIMIT: usize = 5000;

    /// Adds output, which may hold several lines or end in the middle of one.
    pub fn push(&mut self, kind: LineKind, text: &str) {
        let text = text.replace("\r\n", "\n");
        let mut pieces = text.split('\n').peekable();
        while let Some(piece) = pieces.next() {
            let last = pieces.peek().is_none();
            if last && piece.is_empty() {
                // the text ended with a newline
                self.open = false;
                break;
            }
            match self.lines.back_mut() {
                Some(line) if self.open && line.kind == kind => line.text.push_str(piece),
                _ => self.lines.push_back(ConsoleLine {
                    kind,
                    text: piece.to_owned(),
                }),
            }
            self.open = last;
        }
        while self.lines.len() > Self::LIMIT {
            self.lines.pop_front();
        }
    }

    /// Adds a whole line (an input, a result, a message of the editor's).
    pub fn line(&mut self, kind: LineKind, text: impl Into<String>) {
        self.open = false;
        for piece in text.into().split('\n') {
            self.lines.push_back(ConsoleLine {
                kind,
                text: piece.to_owned(),
            });
        }
        self.open = false;
        while self.lines.len() > Self::LIMIT {
            self.lines.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.open = false;
    }
}

/// A watch expression and its last value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    pub expression: String,
    /// `None` before the first evaluation, or while nothing is stopped.
    pub node: Option<VarNode>,
}

/// The cursor and scroll position of one section.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub scroll: usize,
}

/// The debug panel below the editor's splits, and what it shows.
#[derive(Debug, Default)]
pub struct DebugState {
    /// Shown below the splits.
    pub visible: bool,
    /// Keys go to the panel.
    pub focused: bool,
    /// Where the panel was last drawn.
    pub area: Rect,
    /// The section the keys act on (and the one shown, in a narrow panel).
    pub section: Section,
    /// The data section shown next to the console in a wide panel.
    pub data_section: Section,
    pub cursors: HashMap<Section, Cursor>,
    /// The search of each section (`s`): rows that don't contain it are hidden.
    pub filters: HashMap<Section, String>,
    pub status: Status,
    /// The name of the session the panel follows.
    pub session_name: String,
    /// The exit code the adapter reported, kept until the session ends.
    pub exit_code: Option<i64>,
    /// The threads of every session, in the order the sessions started.
    pub threads: Vec<SessionThreads>,
    /// Threads the call stack shows expanded besides the current one.
    pub expanded_threads: HashSet<ThreadKey>,
    /// The scopes of the selected frame.
    pub scopes: Vec<VarNode>,
    pub watches: Vec<Watch>,
    /// Paths the user expanded; kept across stops so the same nodes open again.
    pub expanded: HashSet<VarPath>,
    /// Paths the user collapsed although they open by default (the first scope).
    pub collapsed: HashSet<VarPath>,
    /// The values at the previous stop, to mark the ones that changed: the function of the
    /// top frame and the values by path.
    pub previous: Option<(String, HashMap<VarPath, String>)>,
    pub console: Console,
    /// The exception filters chosen with debug mode's `E`; `None`: the adapter's
    /// defaults. Every new session gets them.
    pub exception_filters: Option<Vec<String>>,
    /// The variables shown are the top frame's (changes are marked against them).
    pub viewing_top: bool,
    /// `g` was pressed: a second `g` goes to the first row.
    pub pending_g: bool,
    /// The console's rows at the last draw: a cursor on the last one follows new output.
    pub console_seen: usize,
    /// Counts stops and resumes: a load started before the last one is dropped.
    pub generation: u64,
    /// Playback, threads, history and time labels.
    pub spectator: Spectator,
    /// The per-line breakpoint engine, and the lineline.
    pub engine: engine::Engine,
    /// Debug mode (this fork): on while `Some`. Its keys come first, and its bar shows.
    pub mode: Option<DebugMode>,
}

/// Debug mode's own state: following the spectated thread, or looking around.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugMode {
    /// View mode (`v`): your own cursor, drawn as in select mode; the stops don't move the view.
    pub viewing: bool,
    /// What the last key did, or why it waited, for the bar.
    pub note: Option<String>,
}

/// The threads of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionThreads {
    pub session: DebugAdapterId,
    pub threads: Vec<Thread>,
}

impl DebugState {
    pub fn show(&mut self) {
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.focused = false;
    }

    pub fn focus(&mut self) {
        self.visible = true;
        self.focused = true;
    }

    pub fn unfocus(&mut self) {
        self.focused = false;
    }

    pub fn is_focused(&self) -> bool {
        self.visible && self.focused
    }

    /// Whether debug mode is on.
    pub fn mode_on(&self) -> bool {
        self.mode.is_some()
    }

    /// Debug mode is on and follows the spectated thread: the stops move the view.
    pub fn following(&self) -> bool {
        self.mode.as_ref().is_some_and(|mode| !mode.viewing)
    }

    /// Debug mode is on, in view mode: your own cursor.
    pub fn viewing(&self) -> bool {
        self.mode.as_ref().is_some_and(|mode| mode.viewing)
    }

    /// Sets the note on debug mode's bar (nothing outside debug mode).
    pub fn note(&mut self, note: impl Into<String>) {
        if let Some(mode) = &mut self.mode {
            mode.note = Some(note.into());
        }
    }

    pub fn cursor(&self, section: Section) -> Cursor {
        self.cursors.get(&section).copied().unwrap_or_default()
    }

    pub fn cursor_mut(&mut self, section: Section) -> &mut Cursor {
        self.cursors.entry(section).or_default()
    }

    /// Switches to `section`; in a wide panel, a data section also becomes the one shown next to
    /// the console.
    pub fn select(&mut self, section: Section) {
        self.section = section;
        if section != Section::Console {
            self.data_section = section;
        }
    }

    /// A new session starts: forgets the last one's values and output.
    pub fn start_session(&mut self, name: String) {
        self.status = Status::Starting;
        self.session_name = name;
        self.exit_code = None;
        self.threads.clear();
        self.scopes.clear();
        self.previous = None;
        for watch in &mut self.watches {
            watch.node = None;
        }
        self.console.clear();
        self.generation += 1;
        self.spectator.start_session();
        self.engine.start_session();
    }

    /// Applies `[editor.debugger]`: the defaults of playback and recording, and the engine.
    pub fn apply_config(&mut self, config: &config::DebuggerConfig) {
        let spectator = &mut self.spectator;
        spectator.speed = config.speed_index();
        spectator.follow = config.follow.into();
        spectator.caps = config.caps();
        spectator.history.trim(spectator.caps.history);
        spectator.timing.enabled = config.time_labels;
        spectator.timing.threshold =
            std::time::Duration::from_secs_f64(config.time_threshold.max(0.0) / 1000.0);
        self.engine.kind = config.engine.into();
        self.engine.record.set_cap(config.line_record);
    }

    /// The threads of `session`.
    pub fn threads_of(&self, session: DebugAdapterId) -> &[Thread] {
        self.threads
            .iter()
            .find(|entry| entry.session == session)
            .map_or(&[], |entry| entry.threads.as_slice())
    }

    /// Replaces the threads of `session` (a new session goes last).
    pub fn set_threads(&mut self, session: DebugAdapterId, threads: Vec<Thread>) {
        match self
            .threads
            .iter_mut()
            .find(|entry| entry.session == session)
        {
            Some(entry) => entry.threads = threads,
            None => self.threads.push(SessionThreads { session, threads }),
        }
    }

    /// Every thread of every session, in the call stack's order.
    pub fn all_threads(&self) -> Vec<(ThreadKey, &Thread)> {
        self.threads
            .iter()
            .flat_map(|entry| {
                entry.threads.iter().map(move |thread| {
                    (
                        ThreadKey {
                            session: entry.session,
                            thread: thread.id,
                        },
                        thread,
                    )
                })
            })
            .collect()
    }

    /// The name of a thread, or `thread 4`.
    pub fn thread_name(&self, key: ThreadKey) -> String {
        self.threads_of(key.session)
            .iter()
            .find(|thread| thread.id == key.thread)
            .map(|thread| thread.name.clone())
            .unwrap_or_else(|| format!("thread {}", key.thread))
    }

    /// The scopes shown: the live ones, or in rewind the recorded ones of the frame shown.
    pub fn shown_scopes(&self) -> &[VarNode] {
        match self.spectator.history.current() {
            Some(snapshot) => snapshot
                .scopes
                .get(self.spectator.history.frame)
                .and_then(Option::as_deref)
                .unwrap_or(&[]),
            None => &self.scopes,
        }
    }

    /// In rewind, the step the shown frame's variables were carried from, when they weren't
    /// fetched at this stop (the frame hadn't moved since).
    pub fn shown_carried(&self) -> Option<usize> {
        let snapshot = self.spectator.history.current()?;
        snapshot
            .carried
            .get(self.spectator.history.frame)
            .copied()
            .flatten()
    }

    /// The watches shown: the live ones, or in rewind the recorded ones.
    pub fn shown_watches(&self) -> &[Watch] {
        match self.spectator.history.current() {
            Some(snapshot) => &snapshot.watches,
            None => &self.watches,
        }
    }

    pub fn is_rewinding(&self) -> bool {
        self.spectator.history.is_rewinding()
    }

    /// The program runs again: values stay shown until the next stop, unmarked.
    pub fn resumed(&mut self) {
        self.status = Status::Running;
        self.generation += 1;
    }

    /// The session ended: the panel hides, the status says how.
    pub fn end_session(&mut self) {
        self.status = Status::Ended {
            code: self.exit_code,
        };
        self.generation += 1;
        self.threads.clear();
        self.scopes.clear();
        for watch in &mut self.watches {
            watch.node = None;
        }
        self.spectator.end_session();
        self.engine.end_session();
        self.hide();
    }

    /// Whether the node at `path` is shown expanded.
    pub fn is_expanded(&self, path: &VarPath) -> bool {
        if self.collapsed.contains(path) {
            return false;
        }
        self.expanded.contains(path) || self.opens_by_default(path)
    }

    /// The first scope that isn't expensive opens by itself.
    fn opens_by_default(&self, path: &VarPath) -> bool {
        path.tree == Tree::Scopes
            && path.names.len() == 1
            && self
                .shown_scopes()
                .iter()
                .find(|scope| !scope.expensive)
                .is_some_and(|scope| scope.name == path.names[0])
    }

    pub fn set_expanded(&mut self, path: &VarPath, expanded: bool) {
        if expanded {
            self.collapsed.remove(path);
            self.expanded.insert(path.clone());
        } else {
            self.expanded.remove(path);
            if self.opens_by_default(path) {
                self.collapsed.insert(path.clone());
            }
        }
    }

    /// The node at `path` as shown (recorded, in rewind).
    pub fn node(&self, path: &VarPath) -> Option<&VarNode> {
        let (first, rest) = path.names.split_first()?;
        let root = match path.tree {
            Tree::Scopes => self
                .shown_scopes()
                .iter()
                .find(|scope| &scope.name == first)?,
            Tree::Watch(index) => self.shown_watches().get(index)?.node.as_ref()?,
        };
        root.find(rest)
    }

    pub fn node_mut(&mut self, path: &VarPath) -> Option<&mut VarNode> {
        let (first, rest) = path.names.split_first()?;
        let root = match path.tree {
            Tree::Scopes => self.scopes.iter_mut().find(|scope| &scope.name == first)?,
            Tree::Watch(index) => self.watches.get_mut(index)?.node.as_mut()?,
        };
        root.find_mut(rest)
    }

    /// The `variablesReference` of the node that holds the one at `path`: what `setVariable`
    /// needs. `None` at the top of a tree.
    pub fn container_reference(&self, path: &VarPath) -> Option<usize> {
        if path.names.len() < 2 {
            return None;
        }
        let parent = VarPath {
            tree: path.tree.clone(),
            names: path.names[..path.names.len() - 1].to_vec(),
        };
        self.node(&parent).map(|node| node.reference)
    }

    /// The paths of every expanded node that has children, in the scopes and the watches:
    /// what a reload after a stop loads again.
    pub fn expanded_paths(&self) -> Vec<VarPath> {
        let mut paths: Vec<VarPath> = self.expanded.iter().cloned().collect();
        if let Some(first) = self.scopes.iter().find(|scope| !scope.expensive) {
            let path = VarPath {
                tree: Tree::Scopes,
                names: vec![first.name.clone()],
            };
            if !self.collapsed.contains(&path) {
                paths.push(path);
            }
        }
        paths.sort_by_key(|path| path.names.len());
        paths.dedup();
        paths
    }

    /// New scopes and watch values after a stop (or a frame switch): marks the values that
    /// changed since the previous stop, when the top frame is in the same function, and keeps
    /// these values for the next stop. `function` is `None` for another frame than the top one:
    /// nothing is marked and the previous values stay.
    pub fn apply_values(
        &mut self,
        function: Option<&str>,
        mut scopes: Vec<VarNode>,
        mut watches: Vec<Option<VarNode>>,
    ) {
        self.viewing_top = function.is_some();
        if let Some(function) = function {
            let mut values = HashMap::new();
            let same = self
                .previous
                .as_ref()
                .filter(|(previous, _)| previous == function)
                .map(|(_, values)| values);
            for scope in &mut scopes {
                mark(Tree::Scopes, Vec::new(), scope, same, &mut values);
            }
            for (index, watch) in watches.iter_mut().enumerate() {
                if let Some(node) = watch {
                    mark(Tree::Watch(index), Vec::new(), node, same, &mut values);
                }
            }
            self.previous = Some((function.to_owned(), values));
        }
        self.scopes = scopes;
        for (watch, node) in self.watches.iter_mut().zip(watches) {
            watch.node = node;
        }
    }

    /// The children of the node at `path`, loaded after the user expanded it. Their values are
    /// kept for the next stop when the top frame is shown.
    pub fn set_children(&mut self, path: &VarPath, children: Vec<VarNode>) {
        let viewing_top = self.viewing_top;
        if viewing_top {
            if let Some((_, values)) = &mut self.previous {
                for child in &children {
                    let mut names = path.names.clone();
                    names.push(child.name.clone());
                    values.insert(
                        VarPath {
                            tree: path.tree.clone(),
                            names,
                        },
                        child.value.clone(),
                    );
                }
            }
        }
        if let Some(node) = self.node_mut(path) {
            node.children = Some(children);
        }
    }

    /// The rows of a variables tree (the scopes, or the watches). `memory`: the adapter reads
    /// memory, so the values that point to some get a badge.
    pub fn var_rows(&self, watch: bool, memory: bool) -> Vec<Row> {
        let mut rows = Vec::new();
        if watch {
            let watches = self.shown_watches();
            if watches.is_empty() {
                rows.push(Row::hint(if self.is_rewinding() {
                    "No watches were recorded at this stop"
                } else {
                    "a: add an expression to watch"
                }));
            }
            for (index, watch) in watches.iter().enumerate() {
                let path = VarPath {
                    tree: Tree::Watch(index),
                    names: vec![watch.expression.clone()],
                };
                match &watch.node {
                    Some(node) => self.push_var_rows(&mut rows, path, node, 0, memory),
                    None => rows.push(Row {
                        depth: 0,
                        marker: Marker::None,
                        parts: vec![
                            (watch.expression.clone(), Role::Name),
                            (" = ".into(), Role::Dim),
                            ("not available".into(), Role::Dim),
                        ],
                        target: Target::Var(path),
                    }),
                }
            }
        } else {
            let scopes = self.shown_scopes();
            if scopes.is_empty() {
                rows.push(Row::hint(match self.status {
                    _ if self.is_rewinding() => "No variables were recorded for this frame",
                    Status::Stopped { .. } => "No variables in this frame",
                    Status::Running | Status::Starting => "Running: the variables show at a stop",
                    _ => "No debug session",
                }));
            }
            if let Some(step) = self.shown_carried() {
                rows.push(Row {
                    depth: 0,
                    marker: Marker::None,
                    parts: vec![(format!("carried from step {step}"), Role::Carried)],
                    target: Target::None,
                });
            }
            for scope in scopes {
                let path = VarPath {
                    tree: Tree::Scopes,
                    names: vec![scope.name.clone()],
                };
                self.push_var_rows(&mut rows, path, scope, 0, memory);
            }
        }
        rows
    }

    fn push_var_rows(
        &self,
        rows: &mut Vec<Row>,
        path: VarPath,
        node: &VarNode,
        depth: u16,
        memory: bool,
    ) {
        let expanded = node.has_children() && self.is_expanded(&path);
        let marker = match (node.has_children(), expanded) {
            (false, _) => Marker::None,
            (true, false) => Marker::Collapsed,
            (true, true) => Marker::Expanded,
        };
        let scope = path.tree == Tree::Scopes && path.names.len() == 1;
        let mut parts = Vec::new();
        if scope {
            parts.push((node.name.clone(), Role::Heading));
        } else {
            if path.tree == Tree::Scopes && self.spectator.marked.contains(&path.names) {
                parts.push(("◆ ".into(), Role::Changed));
            }
            parts.push((node.name.clone(), Role::Name));
            let value_role = if node.error {
                Role::Error
            } else if node.changed {
                Role::Changed
            } else {
                Role::Value
            };
            parts.push((if node.error { ": " } else { " = " }.into(), Role::Dim));
            parts.push((one_line(&node.value), value_role));
            if let Some(ty) = node.ty.as_deref().filter(|ty| !ty.is_empty()) {
                parts.push((format!("  {ty}"), Role::Type));
            }
            if memory && node.memory_reference.is_some() {
                parts.push((format!("  {MEMORY_BADGE}"), Role::Memory));
            }
        }
        rows.push(Row {
            depth,
            marker,
            parts,
            target: Target::Var(path.clone()),
        });
        if !expanded {
            return;
        }
        match &node.children {
            None => rows.push(Row {
                depth: depth + 1,
                marker: Marker::None,
                parts: vec![(
                    if self.is_rewinding() {
                        "not recorded (capture depth)"
                    } else {
                        "loading…"
                    }
                    .into(),
                    Role::Dim,
                )],
                target: Target::None,
            }),
            Some(children) if children.is_empty() => rows.push(Row {
                depth: depth + 1,
                marker: Marker::None,
                parts: vec![("empty".into(), Role::Dim)],
                target: Target::None,
            }),
            Some(children) => {
                for child in children {
                    let mut names = path.names.clone();
                    names.push(child.name.clone());
                    let child_path = VarPath {
                        tree: path.tree.clone(),
                        names,
                    };
                    self.push_var_rows(rows, child_path, child, depth + 1, memory);
                }
            }
        }
    }

    /// The rows of the console.
    pub fn console_rows(&self) -> Vec<Row> {
        if self.console.lines.is_empty() {
            return vec![Row::hint(
                "The program's output shows here; i: evaluate an expression",
            )];
        }
        self.console
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let parts = match line.kind {
                    LineKind::Stdout => vec![(line.text.clone(), Role::Stdout)],
                    LineKind::Stderr => vec![(line.text.clone(), Role::Stderr)],
                    LineKind::Info => vec![(line.text.clone(), Role::Info)],
                    LineKind::Input => {
                        vec![("› ".into(), Role::Dim), (line.text.clone(), Role::Input)]
                    }
                    LineKind::Result => vec![(line.text.clone(), Role::Value)],
                    LineKind::Error => vec![(line.text.clone(), Role::Error)],
                };
                Row {
                    depth: 0,
                    marker: Marker::None,
                    parts,
                    target: Target::ConsoleLine(index),
                }
            })
            .collect()
    }
}

/// Marks `node` and its loaded children as changed when `previous` had another value at the
/// same path, and records their values.
fn mark(
    tree: Tree,
    mut names: Vec<String>,
    node: &mut VarNode,
    previous: Option<&HashMap<VarPath, String>>,
    values: &mut HashMap<VarPath, String>,
) {
    names.push(node.name.clone());
    let path = VarPath { tree, names };
    node.changed = previous
        .and_then(|previous| previous.get(&path))
        .is_some_and(|value| *value != node.value);
    if let Some(children) = &mut node.children {
        for child in children {
            mark(
                path.tree.clone(),
                path.names.clone(),
                child,
                previous,
                values,
            );
        }
    }
    values.insert(path, node.value.clone());
}

/// Marks the values of `new` that differ from `old` at the same path (a recorded stop against the
/// one before it).
pub fn mark_changes(new: &mut [VarNode], old: &[VarNode]) {
    let mut previous = HashMap::new();
    for node in old {
        let mut ignored = HashMap::new();
        let mut copy = node.clone();
        mark(Tree::Scopes, Vec::new(), &mut copy, None, &mut ignored);
        previous.extend(ignored);
    }
    let mut values = HashMap::new();
    for node in new {
        mark(Tree::Scopes, Vec::new(), node, Some(&previous), &mut values);
    }
}

/// A value on one line: newlines shown as `⏎`.
fn one_line(value: &str) -> String {
    value.replace('\n', "⏎")
}

/// One session's part of the call stack.
pub struct StackSession<'a> {
    pub session: DebugAdapterId,
    pub name: &'a str,
    pub threads: &'a [Thread],
    pub frames: &'a HashMap<ThreadId, Vec<StackFrame>>,
    pub states: &'a HashMap<ThreadId, String>,
    /// The current thread and its selected frame, in the session the panel follows.
    pub current: Option<(ThreadId, Option<usize>)>,
}

fn frame_row(frame: &StackFrame, key: ThreadKey, index: usize, current: bool) -> Row {
    let mut parts = vec![(
        frame.name.clone(),
        if current { Role::Current } else { Role::Name },
    )];
    match frame.source.as_ref() {
        Some(source) => {
            let place = source
                .path
                .as_deref()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .or_else(|| source.name.clone())
                .unwrap_or_default();
            parts.push((format!("  {place}:{}", frame.line), Role::Dim));
        }
        None => parts.push(("  no source".into(), Role::Dim)),
    }
    Row {
        depth: 1,
        marker: if current {
            Marker::Current
        } else {
            Marker::None
        },
        parts,
        target: Target::Frame { key, index },
    }
}

/// The rows of the call stack: every thread of every session (under the session's name when
/// there are several), the current one expanded to its frames (and any other the user
/// expanded, once its frames are loaded), with the spectated and frozen threads marked.
pub fn stack_rows(
    sessions: &[StackSession],
    expanded: &HashSet<ThreadKey>,
    spectator: &Spectator,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let with_threads = sessions.iter().filter(|s| !s.threads.is_empty()).count();
    if with_threads == 0 {
        rows.push(Row::hint("No threads: the call stack shows at a stop"));
    }
    for session in sessions {
        if session.threads.is_empty() {
            continue;
        }
        if with_threads > 1 {
            rows.push(Row {
                depth: 0,
                marker: Marker::None,
                parts: vec![(session.name.to_owned(), Role::Heading)],
                target: Target::None,
            });
        }
        for thread in session.threads {
            let key = ThreadKey {
                session: session.session,
                thread: thread.id,
            };
            let current = session.current.filter(|(id, _)| *id == thread.id);
            let is_current = current.is_some();
            let open = is_current || expanded.contains(&key);
            let thread_frames = session.frames.get(&thread.id);
            let marker = if open && thread_frames.is_some() {
                Marker::Expanded
            } else {
                Marker::Collapsed
            };
            let state = session
                .states
                .get(&thread.id)
                .map(String::as_str)
                .unwrap_or(if is_current { "stopped" } else { "" });
            let mut parts = vec![(
                format!("{} {}", thread.id, thread.name),
                if is_current {
                    Role::Current
                } else {
                    Role::Name
                },
            )];
            if !state.is_empty() {
                parts.push((format!("  {state}"), Role::Dim));
            }
            if spectator.spectated == Some(key) {
                parts.push(("  ◉ spectating".into(), Role::Current));
                if spectator.solo {
                    parts.push((" alone".into(), Role::Current));
                }
            }
            if spectator.is_frozen(key) {
                parts.push(("  ❄ frozen".into(), Role::Frozen));
            }
            rows.push(Row {
                depth: 0,
                marker,
                parts,
                target: Target::Thread(key),
            });
            let Some(thread_frames) = thread_frames.filter(|_| open) else {
                continue;
            };
            let active = current.and_then(|(_, frame)| frame);
            for (index, frame) in thread_frames.iter().enumerate() {
                rows.push(frame_row(frame, key, index, active == Some(index)));
            }
        }
    }
    rows
}

/// The call stack of a recorded stop: its thread and frames, the frame shown marked.
pub fn rewind_stack_rows(snapshot: &history::Snapshot, frame: usize) -> Vec<Row> {
    let mut rows = vec![Row {
        depth: 0,
        marker: Marker::Expanded,
        parts: vec![
            (
                format!("{} {}", snapshot.key.thread, snapshot.thread_name),
                Role::Current,
            ),
            (format!("  {} (recorded)", snapshot.reason), Role::Dim),
        ],
        target: Target::Thread(snapshot.key),
    }];
    for (index, stack_frame) in snapshot.frames.iter().enumerate() {
        rows.push(frame_row(stack_frame, snapshot.key, index, index == frame));
    }
    rows
}

/// The rows of the breakpoints: by file then line, paths relative to `cwd`.
pub fn breakpoint_rows(breakpoints: &HashMap<PathBuf, Vec<Breakpoint>>, cwd: &Path) -> Vec<Row> {
    let mut all: Vec<(&PathBuf, &Breakpoint)> = breakpoints
        .iter()
        .flat_map(|(path, list)| list.iter().map(move |breakpoint| (path, breakpoint)))
        .collect();
    all.sort_by(|a, b| a.0.cmp(b.0).then(a.1.line.cmp(&b.1.line)));
    if all.is_empty() {
        return vec![Row::hint(
            "No breakpoints: in debug mode, b sets one on the cursor's line",
        )];
    }
    all.into_iter()
        .map(|(path, breakpoint)| {
            let symbol = breakpoint.kind.glyph(breakpoint.verified);
            let (symbol, role) = if breakpoint.disabled {
                ("○", Role::Disabled)
            } else if breakpoint.verified {
                (symbol, Role::Breakpoint)
            } else {
                (symbol, Role::Unverified)
            };
            let shown = path.strip_prefix(cwd).unwrap_or(path);
            let mut parts = vec![
                (format!("{symbol} "), role),
                (
                    format!("{}:{}", shown.display(), breakpoint.line + 1),
                    if breakpoint.disabled {
                        Role::Disabled
                    } else {
                        Role::Name
                    },
                ),
            ];
            if breakpoint.kind != crate::editor::BreakpointKind::Any {
                parts.push((format!("  {}", breakpoint.kind.label()), Role::Dim));
            }
            if let Some(condition) = &breakpoint.condition {
                parts.push(("  if ".into(), Role::Dim));
                parts.push((condition.clone(), Role::Value));
            }
            if let Some(hits) = &breakpoint.hit_condition {
                parts.push(("  hits ".into(), Role::Dim));
                parts.push((hits.clone(), Role::Value));
            }
            if let Some(log) = &breakpoint.log_message {
                parts.push(("  log ".into(), Role::Dim));
                parts.push((log.clone(), Role::Value));
            }
            if let Some(message) = breakpoint
                .message
                .as_deref()
                .filter(|_| !breakpoint.verified)
            {
                parts.push((format!("  {message}"), Role::Dim));
            }
            Row {
                depth: 0,
                marker: Marker::None,
                parts,
                target: Target::Breakpoint {
                    path: path.clone(),
                    line: breakpoint.line,
                },
            }
        })
        .collect()
}

/// The rows that contain `filter` (ignoring case); all of them for an empty filter.
pub fn filter_rows(rows: Vec<Row>, filter: Option<&str>) -> Vec<Row> {
    let Some(filter) = filter.filter(|filter| !filter.is_empty()) else {
        return rows;
    };
    let filter = filter.to_lowercase();
    rows.into_iter()
        .filter(|row| row.target != Target::None && row.text().to_lowercase().contains(&filter))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, value: &str) -> VarNode {
        VarNode {
            name: name.into(),
            value: value.into(),
            ..Default::default()
        }
    }

    fn parent(name: &str, children: Vec<VarNode>) -> VarNode {
        VarNode {
            name: name.into(),
            value: "{…}".into(),
            reference: 1,
            children: Some(children),
            ..Default::default()
        }
    }

    fn scopes_path(names: &[&str]) -> VarPath {
        VarPath {
            tree: Tree::Scopes,
            names: names.iter().map(|name| name.to_string()).collect(),
        }
    }

    #[test]
    fn output_is_split_into_lines_and_continues_an_open_line() {
        let mut console = Console::default();
        console.push(LineKind::Stdout, "hel");
        console.push(LineKind::Stdout, "lo\nwor");
        console.push(LineKind::Stdout, "ld\n");
        console.push(LineKind::Stderr, "oops\r\n");
        console.push(LineKind::Stdout, "next\n");
        let lines: Vec<_> = console.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(lines, ["hello", "world", "oops", "next"]);
        assert_eq!(console.lines[2].kind, LineKind::Stderr);
    }

    #[test]
    fn the_console_keeps_its_last_lines() {
        let mut console = Console::default();
        for i in 0..Console::LIMIT + 10 {
            console.line(LineKind::Result, i.to_string());
        }
        assert_eq!(console.lines.len(), Console::LIMIT);
        assert_eq!(console.lines.front().unwrap().text, "10");
    }

    #[test]
    fn values_that_changed_since_the_last_stop_in_the_same_function_are_marked() {
        let mut state = DebugState::default();
        let locals = |x: &str, y: &str| {
            vec![parent(
                "Locals",
                vec![var("x", x), parent("p", vec![var("y", y)])],
            )]
        };
        state.apply_values(Some("main"), locals("1", "2"), vec![]);
        assert!(!state.node(&scopes_path(&["Locals", "x"])).unwrap().changed);

        state.apply_values(Some("main"), locals("5", "2"), vec![]);
        assert!(state.node(&scopes_path(&["Locals", "x"])).unwrap().changed);
        assert!(
            !state
                .node(&scopes_path(&["Locals", "p", "y"]))
                .unwrap()
                .changed
        );

        // another function: nothing compares
        state.apply_values(Some("area"), locals("9", "9"), vec![]);
        assert!(!state.node(&scopes_path(&["Locals", "x"])).unwrap().changed);

        // another frame than the top one: shown, not marked, the previous values stay
        state.apply_values(None, locals("7", "7"), vec![]);
        state.apply_values(Some("area"), locals("9", "8"), vec![]);
        assert!(
            state
                .node(&scopes_path(&["Locals", "p", "y"]))
                .unwrap()
                .changed
        );
    }

    #[test]
    fn watches_are_marked_by_expression() {
        let mut state = DebugState::default();
        state.watches.push(Watch {
            expression: "p.x".into(),
            node: None,
        });
        state.apply_values(Some("f"), vec![], vec![Some(var("p.x", "1"))]);
        state.apply_values(Some("g"), vec![], vec![Some(var("p.x", "2"))]);
        assert!(!state.watches[0].node.as_ref().unwrap().changed);
        state.apply_values(Some("g"), vec![], vec![Some(var("p.x", "3"))]);
        assert!(state.watches[0].node.as_ref().unwrap().changed);
    }

    #[test]
    fn the_first_cheap_scope_opens_by_itself_and_can_be_closed() {
        let mut state = DebugState::default();
        let mut globals = parent("Globals", vec![var("g", "1")]);
        globals.expensive = true;
        state.scopes = vec![globals, parent("Locals", vec![var("x", "1")])];
        let rows = state.var_rows(false, false);
        let texts: Vec<_> = rows.iter().map(Row::text).collect();
        assert_eq!(texts, ["Globals", "Locals", "x = 1"]);
        assert_eq!(rows[0].marker, Marker::Collapsed);
        assert_eq!(rows[1].marker, Marker::Expanded);

        state.set_expanded(&scopes_path(&["Locals"]), false);
        assert_eq!(state.var_rows(false, false).len(), 2);
        assert!(!state.expanded_paths().contains(&scopes_path(&["Locals"])));

        state.set_expanded(&scopes_path(&["Globals"]), true);
        let texts: Vec<_> = state.var_rows(false, false).iter().map(Row::text).collect();
        assert_eq!(texts, ["Globals", "g = 1", "Locals"]);
    }

    #[test]
    fn a_node_expanded_before_its_children_load_says_so() {
        let mut state = DebugState::default();
        let mut p = parent("p", vec![]);
        p.children = None;
        state.scopes = vec![parent("Locals", vec![p])];
        state.set_expanded(&scopes_path(&["Locals", "p"]), true);
        let texts: Vec<_> = state.var_rows(false, false).iter().map(Row::text).collect();
        assert_eq!(texts, ["Locals", "p = {…}", "loading…"]);
        assert_eq!(
            state.container_reference(&scopes_path(&["Locals", "p"])),
            Some(1)
        );
        assert_eq!(state.container_reference(&scopes_path(&["Locals"])), None);
    }

    #[test]
    fn the_stack_shows_the_frames_of_the_current_thread() {
        let threads = vec![
            Thread {
                id: serde_json::from_str("1").unwrap(),
                name: "main".into(),
            },
            Thread {
                id: serde_json::from_str("2").unwrap(),
                name: "worker".into(),
            },
        ];
        let frame = |name: &str, line| StackFrame {
            id: 0,
            name: name.into(),
            source: Some(vortex_dap::Source {
                path: Some("/src/main.rs".into()),
                name: None,
                source_reference: None,
                presentation_hint: None,
                origin: None,
                sources: None,
                adapter_data: None,
                checksums: None,
            }),
            line,
            column: 1,
            end_line: None,
            end_column: None,
            can_restart: None,
            instruction_pointer_reference: None,
            module_id: None,
            presentation_hint: None,
        };
        let mut frames = HashMap::new();
        frames.insert(threads[0].id, vec![frame("area", 16), frame("main", 30)]);
        let mut states = HashMap::new();
        states.insert(threads[0].id, "breakpoint".to_string());
        let mut registry: slotmap::SlotMap<DebugAdapterId, ()> = slotmap::SlotMap::with_key();
        let session = registry.insert(());
        let child = registry.insert(());
        let no_frames = HashMap::new();
        let no_states = HashMap::new();
        let child_threads = vec![Thread {
            id: serde_json::from_str("1").unwrap(),
            name: "worker.js".into(),
        }];
        let sessions = |with_child: bool| {
            let mut sessions = vec![StackSession {
                session,
                name: "launch",
                threads: &threads,
                frames: &frames,
                states: &states,
                current: Some((threads[0].id, Some(1))),
            }];
            if with_child {
                sessions.push(StackSession {
                    session: child,
                    name: "worker",
                    threads: &child_threads,
                    frames: &no_frames,
                    states: &no_states,
                    current: None,
                });
            }
            sessions
        };
        let mut spectator = Spectator::default();
        let rows = stack_rows(&sessions(false), &HashSet::new(), &spectator);
        let texts: Vec<_> = rows.iter().map(Row::text).collect();
        assert_eq!(
            texts,
            [
                "1 main  breakpoint",
                "area  main.rs:16",
                "main  main.rs:30",
                "2 worker"
            ]
        );
        assert_eq!(rows[2].marker, Marker::Current);
        assert_eq!(rows[3].marker, Marker::Collapsed);

        // several sessions: under their names; the spectated and frozen threads are marked
        spectator.spectated = Some(ThreadKey {
            session,
            thread: threads[0].id,
        });
        spectator.toggle_frozen(ThreadKey {
            session,
            thread: threads[1].id,
        });
        let rows = stack_rows(&sessions(true), &HashSet::new(), &spectator);
        let texts: Vec<_> = rows.iter().map(Row::text).collect();
        assert_eq!(
            texts,
            [
                "launch",
                "1 main  breakpoint  ◉ spectating",
                "area  main.rs:16",
                "main  main.rs:30",
                "2 worker  ❄ frozen",
                "worker",
                "1 worker.js"
            ]
        );
        assert_eq!(
            rows[6].target,
            Target::Thread(ThreadKey {
                session: child,
                thread: child_threads[0].id
            })
        );
    }

    #[test]
    fn breakpoints_are_listed_by_file_and_line() {
        let mut breakpoints = HashMap::new();
        breakpoints.insert(
            PathBuf::from("/p/src/b.rs"),
            vec![Breakpoint {
                line: 4,
                verified: true,
                ..Default::default()
            }],
        );
        breakpoints.insert(
            PathBuf::from("/p/src/a.rs"),
            vec![
                Breakpoint {
                    line: 9,
                    condition: Some("x > 3".into()),
                    ..Default::default()
                },
                Breakpoint {
                    line: 2,
                    disabled: true,
                    ..Default::default()
                },
            ],
        );
        let texts: Vec<_> = breakpoint_rows(&breakpoints, Path::new("/p"))
            .iter()
            .map(Row::text)
            .collect();
        assert_eq!(
            texts,
            ["○ src/a.rs:3", "◯ src/a.rs:10  if x > 3", "● src/b.rs:5"]
        );
    }

    #[test]
    fn a_search_keeps_the_rows_that_contain_it() {
        let state = DebugState {
            scopes: vec![parent("Locals", vec![var("count", "1"), var("x", "2")])],
            ..Default::default()
        };
        let rows = filter_rows(state.var_rows(false, false), Some("COUNT"));
        let texts: Vec<_> = rows.iter().map(Row::text).collect();
        assert_eq!(texts, ["count = 1"]);
    }

    #[test]
    fn sections_cycle() {
        assert_eq!(Section::Variables.cycle(-1), Section::Console);
        assert_eq!(Section::Console.cycle(1), Section::Variables);
        assert_eq!(Section::Watch.cycle(2), Section::Breakpoints);
    }
}
