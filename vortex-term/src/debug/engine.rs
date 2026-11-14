//! The per-line breakpoint engine: hidden breakpoints on every executable line of the code the
//! follow mode covers, so the program runs (`continue`) and stops at each line, and Vortex decides
//! at each hit whether to pause there or continue, and when. The state is `vortex-view`'s
//! `debug::engine`; this module installs the breakpoints, lifts and restores them for full-speed
//! runs, and takes the hits.
//!
//! **Which lines.** The files: in `own` mode every source file of the session's languages under
//! the folders of your code (git-tracked and untracked, not ignored); `lib` and `std` (work in
//! progress) add the sources the adapter lists (`loadedSources`) and the files of the frames
//! seen. The lines of each file: DAP `breakpointLocations` where the adapter offers it and answers
//! with lines; else a breakpoint on every line, keeping the ones the adapter verifies on the line
//! asked for (adapters move a breakpoint on a line without code to the next line with code: those
//! duplicates are dropped). A line where you have a breakpoint gets no hidden one: adapters merge
//! two breakpoints on a line into one.
//!
//! **Merged with yours.** `setBreakpoints` replaces every breakpoint of a source, so each request
//! carries your breakpoints first and the hidden ones after; the answer is split the same way.
//!
//! **At each hit.** Where a hit is comes from its breakpoint id. Hits of threads nobody watches
//! are continued by the DAP client itself (`vortex_dap::hits`) and only recorded here; the
//! spectated thread's hits stop and show; your breakpoints go through
//! [`engine::breakpoint_pauses`].

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use vortex_dap::{
    self as dap, events::StoppedBody, hits::Taken, registry::DebugAdapterId, Requester,
    SourceBreakpoint, StackFrame, StepKind,
};
use vortex_view::{
    debug::{
        engine::{
            self, decide_line, Found, FullSpeed, Hidden, HiddenFile, HitKind, LineDecision,
            ManualStep, SessionEngine, SessionState, TaskKind, LINE_REASON,
        },
        lineline::{MarkKind, Why},
        spectator::{Follow, OriginRules, ThreadKey},
        LineKind,
    },
    Editor,
};

use super::{breakpoints, breakpoints::Sent, playback, threads};
use crate::job;

/// How long `breakpointLocations` may take before a file falls back to verifying a breakpoint
/// on every line (js-debug answers only once the script is loaded).
const LOCATIONS_TIMEOUT: Duration = Duration::from_millis(1000);
/// How long a file's `setBreakpoints` may take: lldb-dap takes over 20 seconds (the usual
/// limit) for a big file of a big program once it runs, and a request that gives up still sets
/// its breakpoints, under ids nobody knows (their hits would stop playback as strangers).
const SEND_TIMEOUT: Duration = Duration::from_secs(300);
/// How long `loadedSources` may take.
const SOURCES_TIMEOUT: Duration = Duration::from_millis(2000);
/// At most this many files get hidden breakpoints.
const MAX_FILES: usize = 5000;
/// Requests in flight at once while installing: adapters answer them one by one (lldb-dap), and
/// a request that waits too long behind the others times out.
const IN_FLIGHT: usize = 4;
/// At most this many breakpoints in one `setBreakpoints`: about 60 KB of JSON. lldb-dap 21 can't
/// read a request over 64 KiB (it ends the session); a file with more executable lines gets
/// hidden breakpoints on its first lines only, and says so.
pub const MAX_REQUEST_LINES: usize = 4000;

// ---------------------------------------------------------------------------------------------
// What the engine covers
// ---------------------------------------------------------------------------------------------

/// Whether new sessions get hidden breakpoints.
pub fn wanted(editor: &Editor) -> bool {
    editor.debug.engine.kind == engine::EngineKind::Breakpoints
}

/// The file extensions of the session's languages: the languages whose debugger is the session's
/// adapter, else (an adapter no language names) the languages of the open documents.
fn extensions(editor: &Editor, session: DebugAdapterId) -> HashSet<String> {
    let name = editor
        .debug_adapters
        .get_client(session)
        .and_then(|client| client.config.as_ref())
        .map(|config| config.name.clone())
        .unwrap_or_default();
    let loader = editor.syn_loader.load();
    let of = |config: &vortex_core::syntax::config::LanguageConfiguration| {
        config
            .file_types
            .iter()
            .filter_map(|file_type| match file_type {
                vortex_core::syntax::config::FileType::Extension(ext) => Some(ext.clone()),
                vortex_core::syntax::config::FileType::Glob(_) => None,
            })
            .collect::<Vec<_>>()
    };
    let mut extensions: HashSet<String> = loader
        .language_configs()
        .filter(|config| {
            config
                .debugger
                .as_ref()
                .is_some_and(|debugger| debugger.name == name)
        })
        .flat_map(of)
        .collect();
    if extensions.is_empty() {
        extensions = editor
            .documents()
            .filter_map(|doc| doc.language_config())
            .flat_map(of)
            .collect();
    }
    extensions
}

/// The number of lines of a file on disk.
fn count_lines(path: &Path) -> Option<usize> {
    let bytes = std::fs::read(path).ok()?;
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    Some(newlines + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n")))
}

/// The files to cover, with their number of lines: the source files of `extensions` under the
/// roots that `follow` allows, and the `extra` ones (the adapter's sources, frames seen).
fn gather(
    roots: Vec<PathBuf>,
    extensions: HashSet<String>,
    rules: OriginRules,
    follow: Follow,
    extra: Vec<PathBuf>,
) -> (Vec<(PathBuf, usize)>, bool) {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    let mut add = |path: PathBuf, files: &mut Vec<(PathBuf, usize)>| -> bool {
        if files.len() >= MAX_FILES {
            return false;
        }
        if !seen.insert(path.clone()) || !follow.allows(rules.classify_path(&path)) {
            return true;
        }
        if let Some(lines) = count_lines(&path).filter(|lines| *lines > 0) {
            files.push((path, lines));
        }
        true
    };
    for root in &roots {
        let Ok(root) = std::fs::canonicalize(root) else {
            continue;
        };
        if !root.is_dir() {
            continue;
        }
        for entry in ignore::WalkBuilder::new(&root).build().flatten() {
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default();
            if !extensions.contains(ext) {
                continue;
            }
            if !add(path.to_owned(), &mut files) {
                return (files, true);
            }
        }
    }
    for path in extra {
        if path.is_absolute() && path.is_file() && !add(path, &mut files) {
            return (files, true);
        }
    }
    (files, false)
}

// ---------------------------------------------------------------------------------------------
// Sending hidden breakpoints, file by file
// ---------------------------------------------------------------------------------------------

/// What to do with a file.
#[derive(Debug)]
enum Action {
    /// Find its executable lines (unless they are known) and install them.
    Discover {
        lines: usize,
        known: Option<(Found, Vec<usize>)>,
    },
    /// Send these hidden lines (found before).
    Send {
        hidden: Vec<usize>,
        found: (Found, Vec<usize>),
    },
    /// Send your breakpoints alone, keeping what was found (a full-speed run).
    Lift,
    /// Send your breakpoints alone, and forget the file (the follow mode no longer covers it).
    Remove,
}

#[derive(Debug)]
struct FileJob {
    path: PathBuf,
    mine: Vec<Sent>,
    action: Action,
}

struct FileResult {
    path: PathBuf,
    mine: Vec<Sent>,
    /// The hidden lines sent last, in the order of the answer.
    hidden: Vec<usize>,
    /// The executable lines found (`None`: lifted or removed).
    found: Option<(Found, Vec<usize>)>,
    remove: bool,
    answer: dap::Result<Option<Vec<dap::Breakpoint>>>,
}

fn hidden_breakpoint(line: usize) -> SourceBreakpoint {
    SourceBreakpoint {
        line: line + 1,
        column: None,
        condition: None,
        hit_condition: None,
        log_message: None,
    }
}

async fn send(
    requester: &Requester,
    path: &Path,
    mine: &[Sent],
    hidden: &[usize],
) -> dap::Result<Option<Vec<dap::Breakpoint>>> {
    let breakpoints = mine
        .iter()
        .map(|(_, breakpoint)| breakpoint.clone())
        .chain(hidden.iter().map(|line| hidden_breakpoint(*line)))
        .collect();
    requester
        .set_breakpoints_within(path.to_owned(), breakpoints, SEND_TIMEOUT)
        .await
}

async fn run_job(requester: Requester, locations: bool, job: FileJob) -> FileResult {
    let FileJob { path, mine, action } = job;
    let mine_lines: Vec<usize> = mine.iter().map(|(line, _)| *line).collect();
    let without_mine = |lines: &[usize]| -> Vec<usize> {
        lines
            .iter()
            .copied()
            .filter(|line| !mine_lines.contains(line))
            .collect()
    };
    let result = |hidden, found, remove, answer| FileResult {
        path: path.clone(),
        mine: mine.clone(),
        hidden,
        found,
        remove,
        answer,
    };
    match action {
        Action::Lift | Action::Remove => {
            let answer = send(&requester, &path, &mine, &[]).await;
            let remove = matches!(action, Action::Remove);
            result(Vec::new(), None, remove, answer)
        }
        Action::Send { hidden, found } => {
            let answer = send(&requester, &path, &mine, &hidden).await;
            result(hidden, Some(found), false, answer)
        }
        Action::Discover { lines, known } => {
            let mut found = known;
            if found.is_none() && locations {
                let request = requester.breakpoint_locations(path.clone(), 1, lines.max(1));
                if let Ok(Ok(lines)) = tokio::time::timeout(LOCATIONS_TIMEOUT, request).await {
                    if !lines.is_empty() {
                        found = Some((
                            Found::Locations,
                            lines
                                .into_iter()
                                .map(|line| line.saturating_sub(1))
                                .collect(),
                        ));
                    }
                }
            }
            if let Some(found) = found {
                let hidden = fit(without_mine(&found.1), mine.len());
                let answer = send(&requester, &path, &mine, &hidden).await;
                return result(hidden, Some(found), false, answer);
            }
            // a breakpoint on every line, keeping the ones verified on the line asked for: in
            // chunks, since a request may not be too big (each replaces the one before)
            let all = without_mine(&(0..lines).collect::<Vec<_>>());
            let mut verified = Vec::new();
            let mut pending = Vec::new();
            let mut last = None;
            for chunk in all.chunks(MAX_REQUEST_LINES.saturating_sub(mine.len()).max(1)) {
                let answer = send(&requester, &path, &mine, chunk).await;
                let Ok(Some(answers)) = &answer else {
                    return result(chunk.to_vec(), None, false, answer);
                };
                for (index, answer) in answers.iter().enumerate() {
                    let asked = match index.checked_sub(mine.len()) {
                        Some(index) => chunk.get(index).copied(),
                        // your breakpoints say whether their lines have code too
                        None => mine.get(index).map(|(line, _)| *line),
                    };
                    let Some(asked) = asked else {
                        continue;
                    };
                    match (answer.verified, answer.line) {
                        (true, Some(line)) if line == asked + 1 => verified.push(asked),
                        (true, None) => verified.push(asked),
                        // not resolved yet (js-debug, before the script loads): its event says
                        (false, None) if index >= mine.len() => pending.push(asked),
                        _ => {}
                    }
                }
                last = Some((chunk.to_vec(), answer));
            }
            verified.sort_unstable();
            verified.dedup();
            let found = if !verified.is_empty() || pending.is_empty() {
                (Found::Verified, verified)
            } else {
                (Found::Pending, pending)
            };
            let hidden = fit(without_mine(&found.1), mine.len());
            if let Some((sent, answer)) = last.filter(|(sent, _)| *sent == hidden) {
                return result(sent, Some(found), false, answer);
            }
            let answer = send(&requester, &path, &mine, &hidden).await;
            result(hidden, Some(found), false, answer)
        }
    }
}

/// The hidden lines that fit in one request with `mine` of yours.
fn fit(mut hidden: Vec<usize>, mine: usize) -> Vec<usize> {
    hidden.truncate(MAX_REQUEST_LINES.saturating_sub(mine));
    hidden
}

/// A file's answer: your breakpoints' part goes to them, the hidden part to the engine.
fn apply_result(editor: &mut Editor, session: DebugAdapterId, token: u64, result: FileResult) {
    let FileResult {
        path,
        mine,
        hidden,
        found,
        remove,
        answer,
    } = result;
    if !editor.debug.engine.sessions.contains_key(&session) {
        return;
    }
    let answers = match answer {
        Ok(answers) => answers.unwrap_or_default(),
        Err(err) => {
            breakpoints::apply(editor, session, &path, &mine, Err(err));
            return;
        }
    };
    let (yours, theirs) = answers.split_at(mine.len().min(answers.len()));
    breakpoints::apply(editor, session, &path, &mine, Ok(Some(yours.to_vec())));
    let state = editor.debug.engine.sessions.get_mut(&session).unwrap();
    if remove {
        state.files.remove(&path);
    } else if let Some((how, lines)) = found {
        state.known.insert(path.clone(), (how, lines.clone()));
        if how != Found::Pending {
            if let Ok(modified) = std::fs::metadata(&path).and_then(|meta| meta.modified()) {
                editor
                    .debug
                    .engine
                    .remembered
                    .insert(path.clone(), (modified, how, lines.clone()));
            }
        }
        let state = editor.debug.engine.sessions.get_mut(&session).unwrap();
        let strays = state
            .files
            .get(&path)
            .map(|file| file.strays.clone())
            .unwrap_or_default();
        state.files.insert(
            path.clone(),
            HiddenFile {
                lines,
                found: how,
                strays,
            },
        );
    }
    record_ids(editor, session, &path, &hidden, theirs);
    let executable = editor.debug.engine.sessions[&session]
        .files
        .get(&path)
        .map_or(0, |file| file.lines.len());
    if executable > hidden.len() + mine.len() {
        editor.debug.console.line(
            LineKind::Info,
            format!(
                "{}: {executable} lines with code, the first {} get hidden breakpoints (a \
                 request may hold {MAX_REQUEST_LINES})",
                path.display(),
                hidden.len()
            ),
        );
    }
    if let Some(task) = editor.debug.engine.task_mut(token) {
        task.files_done += 1;
        task.lines += hidden.len();
    }
}

/// Keeps the ids of the hidden breakpoints of `path` from an answer, and marks the ones the
/// adapter moved off their line as strays. The client's fast path learns them at once.
fn record_ids(
    editor: &mut Editor,
    session: DebugAdapterId,
    path: &Path,
    hidden: &[usize],
    answers: &[dap::Breakpoint],
) {
    let Some(state) = editor.debug.engine.sessions.get_mut(&session) else {
        return;
    };
    let shared: Arc<Path> = Arc::from(path);
    let old = state.file_ids.remove(path).unwrap_or_default();
    for id in &old {
        state.ids.remove(id);
    }
    let mut new = Vec::with_capacity(hidden.len());
    let mut strays = Vec::new();
    for (line, answer) in hidden.iter().zip(answers) {
        let Some(id) = answer.id else {
            continue;
        };
        let stray = answer.verified && answer.line.is_some_and(|moved| moved != line + 1);
        if stray {
            strays.push(*line);
        }
        new.push(id);
        state.ids.insert(
            id,
            Hidden {
                path: shared.clone(),
                line: *line,
                stray,
            },
        );
    }
    if let Some(file) = state.files.get_mut(path) {
        file.strays.extend(strays);
    }
    let Some(client) = editor.debug_adapters.get_client(session) else {
        state.file_ids.insert(path.to_owned(), new);
        return;
    };
    let mut filter = client.hit_filter().lock().unwrap();
    for id in &old {
        filter.lines.remove(id);
        filter.strays.remove(id);
    }
    for id in &new {
        if state.ids[id].stray {
            filter.strays.insert(*id);
        } else {
            filter.lines.insert(*id);
        }
    }
    drop(filter);
    state.file_ids.insert(path.to_owned(), new);
}

/// The files likely to run first, which go in before the program is let go: the open buffers,
/// the launch's program when it is a source file, the files with your breakpoints, and the file
/// the spectated thread is in.
fn first_files(editor: &Editor, session: DebugAdapterId) -> HashSet<PathBuf> {
    let mut first: HashSet<PathBuf> = editor
        .documents()
        .filter_map(|doc| doc.path().map(Path::to_path_buf))
        .collect();
    first.extend(
        editor
            .breakpoints
            .iter()
            .filter(|(_, breakpoints)| !breakpoints.is_empty())
            .map(|(path, _)| path.clone()),
    );
    let args = editor
        .debug_adapters
        .get_client(session)
        .and_then(|client| client.starting_request_args());
    if let Some(program) = args.and_then(|args| args.get("program")?.as_str()) {
        let cwd = args.and_then(|args| args.get("cwd")?.as_str());
        let program = Path::new(program);
        first.insert(match cwd {
            Some(cwd) if program.is_relative() => Path::new(cwd).join(program),
            _ => program.to_owned(),
        });
    }
    if let Some(key) = spectated(editor).filter(|key| key.session == session) {
        let frames = editor
            .debug_adapters
            .get_client(session)
            .and_then(|client| client.stack_frames.get(&key.thread));
        if let Some(path) = frames
            .and_then(|frames| frames.first())
            .and_then(|frame| frame.source.as_ref()?.path.clone())
        {
            first.insert(path);
        }
    }
    first
}

/// Puts the jobs of the files likely to run first ahead of the rest, and returns how many there
/// are. None of them: all of them count as first (nothing tells which code matters, so the
/// program waits for all of it, as small programs do anyway).
fn first_in_line(jobs: &mut [FileJob], first: &HashSet<PathBuf>) -> usize {
    jobs.sort_by_key(|job| !first.contains(&job.path));
    match jobs.iter().filter(|job| first.contains(&job.path)).count() {
        0 => jobs.len(),
        count => count,
    }
}

/// Runs `jobs` in the background for task `token`, then `then` in the editor.
fn run_files(
    editor: &mut Editor,
    session: DebugAdapterId,
    token: u64,
    jobs: Vec<FileJob>,
    then: impl FnOnce(&mut Editor) + Send + 'static,
) {
    let all = jobs.len();
    run_files_staged(editor, session, token, jobs, all, |_| {}, then);
}

/// Runs `jobs` in the background for task `token`: `at_first` in the editor once the first
/// `first` jobs are done (the rest still going), then `then` once they all are.
fn run_files_staged(
    editor: &mut Editor,
    session: DebugAdapterId,
    token: u64,
    jobs: Vec<FileJob>,
    first: usize,
    at_first: impl FnOnce(&mut Editor) + Send + 'static,
    then: impl FnOnce(&mut Editor) + Send + 'static,
) {
    let Some(client) = editor.debug_adapters.get_client(session) else {
        return;
    };
    let requester = client.requester();
    let locations = client
        .caps
        .as_ref()
        .and_then(|caps| caps.supports_breakpoint_locations_request)
        .unwrap_or_default();
    if let Some(task) = editor.debug.engine.task_mut(token) {
        task.files_total = jobs.len();
    }
    tokio::spawn(async move {
        let mut results =
            futures_util::stream::iter(jobs.into_iter().enumerate().map(|(index, job)| {
                let requester = requester.clone();
                async move { (index < first, run_job(requester, locations, job).await) }
            }))
            .buffer_unordered(IN_FLIGHT);
        let mut at_first = Some(at_first);
        let mut first_left = first;
        if first_left == 0 {
            if let Some(at_first) = at_first.take() {
                job::dispatch(move |editor, _| at_first(editor)).await;
            }
        }
        while let Some((is_first, result)) = results.next().await {
            job::dispatch(move |editor, _| apply_result(editor, session, token, result)).await;
            if is_first {
                first_left -= 1;
                if first_left == 0 {
                    if let Some(at_first) = at_first.take() {
                        job::dispatch(move |editor, _| at_first(editor)).await;
                    }
                }
            }
        }
        if let Some(at_first) = at_first.take() {
            job::dispatch(move |editor, _| at_first(editor)).await;
        }
        job::dispatch(move |editor, _| then(editor)).await;
    });
}

/// The executable lines of `path` found in an earlier session, if the file hasn't changed since.
fn remembered(editor: &Editor, path: &Path) -> Option<(Found, Vec<usize>)> {
    let (modified, found, lines) = editor.debug.engine.remembered.get(path)?;
    let now = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?;
    (now == *modified).then(|| (*found, lines.clone()))
}

/// Your breakpoints in `path`, as sent.
fn mine(editor: &Editor, path: &Path) -> Vec<Sent> {
    breakpoints::path_to_send(editor, path)
}

/// Redraws a few times a second while a task runs, so the loading animation moves.
fn animate(editor: &mut Editor) {
    let alive = Arc::new(AtomicBool::new(true));
    if let Some(old) = editor.debug.engine.animation.replace(alive.clone()) {
        old.store(false, Ordering::Relaxed);
    }
    tokio::spawn(async move {
        while alive.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
            job::dispatch(|_, _| {}).await;
        }
    });
}

/// The task `token` is over: the engine is ready (or off), and what waited on it goes on.
fn finish(editor: &mut Editor, session: DebugAdapterId, token: u64, kind: TaskKind) {
    let elapsed = editor
        .debug
        .engine
        .task_mut(token)
        .map(|task| task.started.elapsed());
    if elapsed.is_some() {
        editor.debug.engine.task = None;
        if let Some(alive) = editor.debug.engine.animation.take() {
            alive.store(false, Ordering::Relaxed);
        }
    }
    let Some(state) = editor.debug.engine.sessions.get_mut(&session) else {
        return;
    };
    if std::mem::take(&mut state.partial) {
        // the program ran while the last files went in: coverage is whole from here
        editor.debug.engine.record.mark(MarkKind::Covered);
    }
    let Some(state) = editor.debug.engine.sessions.get_mut(&session) else {
        return;
    };
    let count = state.count();
    let files = state
        .files
        .values()
        .filter(|file| !file.lines.is_empty())
        .count();
    state.state = match kind {
        TaskKind::Lift => SessionState::Lifted,
        _ if count == 0 => SessionState::Off,
        _ => SessionState::Ready,
    };
    let off = state.state == SessionState::Off;
    sync_ids(editor, session);
    sync_watch(editor);
    log::info!("hidden breakpoints: {count} in {files} files, {kind:?} done after {elapsed:?}");
    let time = elapsed.map(vortex_view::debug::timing::format_duration);
    match kind {
        TaskKind::Install | TaskKind::Reinstall if off => {
            let text = "No line breakpoints verified in your code: playback steps instead";
            editor.debug.console.line(LineKind::Info, text.to_owned());
            crate::debug::mode::say(editor, text);
        }
        TaskKind::Install | TaskKind::Reinstall => {
            let text = format!(
                "{} hidden breakpoint{} in {files} file{}{}",
                engine::thousands(count),
                if count == 1 { "" } else { "s" },
                if files == 1 { "" } else { "s" },
                time.map(|time| format!(" ({time})")).unwrap_or_default()
            );
            editor.debug.console.line(LineKind::Info, text);
        }
        _ => {}
    }
    if kind != TaskKind::Lift && editor.debug.engine.refollow {
        editor.debug.engine.refollow = false;
        follow_changed(editor);
    }
    if kind != TaskKind::Lift {
        if let Some(key) = editor.debug.engine.pending_full_speed.take() {
            if !full_speed(editor, key) {
                threads::continue_thread(editor, key);
            }
            return;
        }
    }
    if kind != TaskKind::Lift && std::mem::take(&mut editor.debug.engine.pending_advance) {
        let spectator = &editor.debug.spectator;
        if spectator.is_playing() && spectator.step.is_none() {
            playback::advance(editor);
        }
    }
}

/// Installs the hidden breakpoints of a new session (at its configuration, before
/// `configurationDone`), then calls `then` with the files it covered: the rest of your
/// breakpoints are sent by the configuration.
pub fn install(
    editor: &mut Editor,
    session: DebugAdapterId,
    then: impl FnOnce(&mut Editor, HashSet<PathBuf>) + Send + 'static,
) {
    let follow = editor.debug.spectator.follow;
    editor
        .debug
        .engine
        .sessions
        .insert(session, SessionEngine::new(follow));
    let token = editor
        .debug
        .engine
        .start_task(TaskKind::Install, session, 0);
    animate(editor);
    gather_then(editor, session, follow, move |editor, files, capped| {
        warn_capped(editor, capped);
        let jobs = files
            .into_iter()
            .map(|(path, lines)| FileJob {
                mine: mine(editor, &path),
                action: Action::Discover {
                    lines,
                    known: remembered(editor, &path),
                },
                path,
            })
            .collect::<Vec<_>>();
        let mut jobs = jobs;
        let first = first_in_line(&mut jobs, &first_files(editor, session));
        let rest = first < jobs.len();
        let covered: HashSet<PathBuf> = jobs.iter().map(|job| job.path.clone()).collect();
        // the program starts once the files likely to run first are in; the rest go in behind it
        run_files_staged(
            editor,
            session,
            token,
            jobs,
            first,
            move |editor| {
                go(editor, session, token, rest);
                then(editor, covered);
            },
            move |editor| finish(editor, session, token, TaskKind::Install),
        );
    });
}

/// The files first in line are in: the hidden breakpoints work from here, while the rest (if
/// any) go in behind the program, whose lines in them aren't hit until they do.
fn go(editor: &mut Editor, session: DebugAdapterId, token: u64, rest: bool) {
    let Some(state) = editor.debug.engine.sessions.get_mut(&session) else {
        return;
    };
    if state.state == SessionState::Installing {
        state.state = SessionState::Ready;
    }
    state.partial = rest;
    if let Some(task) = editor.debug.engine.task_mut(token) {
        task.running = rest;
        log::info!(
            "hidden breakpoints: {}/{} files in after {:?}, the program goes on",
            task.files_done,
            task.files_total,
            task.started.elapsed()
        );
        if rest {
            let text = format!(
                "{}/{} files have their hidden breakpoints: the program goes on, and lines in \
                 the rest aren't counted until they are in",
                task.files_done, task.files_total
            );
            editor.debug.console.line(LineKind::Info, text);
        }
    }
    sync_ids(editor, session);
    sync_watch(editor);
    if std::mem::take(&mut editor.debug.engine.pending_advance) {
        let spectator = &editor.debug.spectator;
        if spectator.is_playing() && spectator.step.is_none() {
            playback::advance(editor);
        }
    }
}

fn warn_capped(editor: &mut Editor, capped: bool) {
    if capped {
        let text = format!(
            "Only the first {MAX_FILES} files get hidden breakpoints (own-paths in the debugger \
             config narrows the folders)"
        );
        editor.debug.console.line(LineKind::Info, text.clone());
        crate::debug::mode::say(editor, text);
    }
}

/// Gathers the files `follow` covers in the background, then calls `then` in the editor.
fn gather_then(
    editor: &mut Editor,
    session: DebugAdapterId,
    follow: Follow,
    then: impl FnOnce(&mut Editor, Vec<(PathBuf, usize)>, bool) + Send + 'static,
) {
    let Some(client) = editor.debug_adapters.get_client(session) else {
        return;
    };
    let rules = playback::origin_rules(editor, session);
    let roots = rules.roots.clone();
    let extensions = extensions(editor, session);
    let requester = client.requester();
    let sources = follow != Follow::Own
        && client
            .caps
            .as_ref()
            .and_then(|caps| caps.supports_loaded_sources_request)
            .unwrap_or_default();
    // files of the frames seen that no hidden breakpoints cover yet
    let mut extra: Vec<PathBuf> = editor
        .debug
        .engine
        .sessions
        .get(&session)
        .map(|state| state.uncovered.iter().cloned().collect())
        .unwrap_or_default();
    tokio::spawn(async move {
        if sources {
            if let Ok(Ok(list)) =
                tokio::time::timeout(SOURCES_TIMEOUT, requester.loaded_sources()).await
            {
                extra.extend(list.into_iter().filter_map(|source| source.path));
            }
        }
        let gathered =
            tokio::task::spawn_blocking(move || gather(roots, extensions, rules, follow, extra))
                .await
                .unwrap_or_default();
        job::dispatch(move |editor, _| then(editor, gathered.0, gathered.1)).await;
    });
}

/// `e`: the follow mode changed. Sessions with hidden breakpoints get the new set, sent again
/// whole: the files it no longer covers lose theirs.
pub fn follow_changed(editor: &mut Editor) {
    let follow = editor.debug.spectator.follow;
    if follow != Follow::Own {
        let text = format!(
            "{}: hidden breakpoints on every line of {} too (work in progress): absurdly slow, \
             and programs with timeouts may fail",
            follow.label(),
            if follow == Follow::Lib {
                "the libraries"
            } else {
                "the libraries and the runtime"
            }
        );
        editor.debug.console.line(LineKind::Error, text.clone());
        editor.set_error(text);
    }
    let sessions: Vec<DebugAdapterId> = editor
        .debug
        .engine
        .sessions
        .iter()
        .filter(|(_, state)| state.state != SessionState::Off || !state.known.is_empty())
        .map(|(id, _)| *id)
        .collect();
    for session in sessions {
        let state = &editor.debug.engine.sessions[&session];
        if state.follow == follow {
            continue;
        }
        if editor.debug.engine.task.is_some() || state.state == SessionState::Lifted {
            // after the task (or the full-speed run): it reinstalls then
            editor.debug.engine.refollow = true;
            continue;
        }
        reinstall(editor, session, follow);
    }
}

fn reinstall(editor: &mut Editor, session: DebugAdapterId, follow: Follow) {
    let token = editor
        .debug
        .engine
        .start_task(TaskKind::Reinstall, session, 0);
    if let Some(state) = editor.debug.engine.sessions.get_mut(&session) {
        state.follow = follow;
        state.state = SessionState::Installing;
    }
    animate(editor);
    gather_then(editor, session, follow, move |editor, files, capped| {
        warn_capped(editor, capped);
        let Some(state) = editor.debug.engine.sessions.get(&session) else {
            return;
        };
        let wanted: HashSet<&PathBuf> = files.iter().map(|(path, _)| path).collect();
        let mut jobs: Vec<FileJob> = state
            .files
            .keys()
            .filter(|path| !wanted.contains(path))
            .map(|path| FileJob {
                path: path.clone(),
                mine: Vec::new(),
                action: Action::Remove,
            })
            .collect();
        // the whole set again (what was found before isn't asked again)
        for (path, lines) in &files {
            jobs.push(FileJob {
                path: path.clone(),
                mine: Vec::new(),
                action: Action::Discover {
                    lines: *lines,
                    known: state.known.get(path).cloned(),
                },
            });
        }
        for job in &mut jobs {
            job.mine = mine(editor, &job.path);
        }
        let first = first_in_line(&mut jobs, &first_files(editor, session));
        let rest = first < jobs.len();
        run_files_staged(
            editor,
            session,
            token,
            jobs,
            first,
            move |editor| go(editor, session, token, rest),
            move |editor| finish(editor, session, token, TaskKind::Reinstall),
        );
    });
}

/// The hidden lines of `path` that a send of your breakpoints to `session` carries along.
pub fn hidden_lines(
    editor: &Editor,
    session: DebugAdapterId,
    path: &Path,
    mine: &[Sent],
) -> Vec<usize> {
    let Some(state) = editor.debug.engine.sessions.get(&session) else {
        return Vec::new();
    };
    if !matches!(state.state, SessionState::Ready | SessionState::Installing) {
        return Vec::new();
    }
    let count = mine.len();
    let mine: Vec<usize> = mine.iter().map(|(line, _)| *line).collect();
    fit(state.lines_to_send(path, &mine), count)
}

/// The hidden part of an answer to a send of your breakpoints (see [`hidden_lines`]).
pub fn apply_hidden(
    editor: &mut Editor,
    session: DebugAdapterId,
    path: &Path,
    hidden: &[usize],
    answers: &[dap::Breakpoint],
) {
    if hidden.is_empty()
        && !editor
            .debug
            .engine
            .sessions
            .get(&session)
            .is_some_and(|state| state.file_ids.contains_key(path))
    {
        return;
    }
    record_ids(editor, session, path, hidden, answers);
}

/// The adapter's `breakpoint` event about a hidden breakpoint: verified where asked, moved (a
/// stray, dropped at the next send), or removed. Returns whether the id was a hidden one.
pub fn breakpoint_event(
    editor: &mut Editor,
    session: DebugAdapterId,
    reason: &str,
    answer: &dap::Breakpoint,
) -> bool {
    let Some(id) = answer.id else {
        return false;
    };
    let Some(state) = editor.debug.engine.sessions.get_mut(&session) else {
        return false;
    };
    let Some(entry) = state.ids.get_mut(&id) else {
        return false;
    };
    match reason {
        "removed" => {
            if let Some(entry) = state.ids.remove(&id) {
                if let Some(ids) = state.file_ids.get_mut(&*entry.path) {
                    ids.retain(|other| *other != id);
                }
            }
        }
        _ => {
            let stray = answer.verified && answer.line.is_some_and(|moved| moved != entry.line + 1);
            if stray && !entry.stray {
                entry.stray = true;
                let (path, line) = (entry.path.to_path_buf(), entry.line);
                if let Some(file) = state.files.get_mut(&path) {
                    file.strays.insert(line);
                }
                drop_strays_soon(editor, path);
            }
        }
    }
    sync_ids(editor, session);
    true
}

/// Sends a file again without its strays, once the adapter's events about it settled.
fn drop_strays_soon(editor: &mut Editor, path: PathBuf) {
    if !editor.debug.engine.redo.insert(path) || editor.debug.engine.redo.len() > 1 {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        job::dispatch(|editor, _| {
            let paths = std::mem::take(&mut editor.debug.engine.redo);
            for path in paths {
                breakpoints::sync(editor, &path);
            }
        })
        .await;
    });
}

// ---------------------------------------------------------------------------------------------
// Full-speed runs
// ---------------------------------------------------------------------------------------------

/// Debug mode's `L` (the engine's continue): lifts every hidden breakpoint of the session, continues `key` at full speed, and
/// puts them back at the next stop. Returns false when the session has no hidden breakpoints in
/// place (the caller continues as usual).
pub fn full_speed(editor: &mut Editor, key: ThreadKey) -> bool {
    if editor.debug.engine.task.is_some() && editor.debug.engine.is_on(key.session) {
        // a continue now would only reach the next hidden breakpoint: it runs once the set is in
        editor.debug.engine.pending_full_speed = Some(key);
        crate::debug::mode::say(editor, "full speed once the hidden breakpoints are in");
        return true;
    }
    if !editor.debug.engine.is_ready(key.session) {
        return false;
    }
    let session = key.session;
    let files: Vec<PathBuf> = editor.debug.engine.sessions[&session]
        .files
        .keys()
        .cloned()
        .collect();
    let token = editor
        .debug
        .engine
        .start_task(TaskKind::Lift, session, files.len());
    if let Some(state) = editor.debug.engine.sessions.get_mut(&session) {
        state.state = SessionState::Lifted;
        state.ids.clear();
        state.file_ids.clear();
    }
    sync_ids(editor, session);
    animate(editor);
    let jobs = files
        .into_iter()
        .map(|path| FileJob {
            mine: mine(editor, &path),
            path,
            action: Action::Lift,
        })
        .collect();
    let at = editor.debug.engine.record.counter();
    log::info!("continuing at full speed to the next breakpoint");
    run_files(editor, session, token, jobs, move |editor| {
        finish(editor, session, token, TaskKind::Lift);
        if editor.debug_adapters.get_client(session).is_none() {
            return;
        }
        editor.debug.engine.full_speed = Some(FullSpeed {
            session,
            since: Some(Instant::now()),
            at,
        });
        super::threads::continue_thread(editor, key);
    });
    true
}

/// The full-speed run of `session` ended at a stop: its gap is marked, and the hidden
/// breakpoints go back.
fn end_full_speed(editor: &mut Editor, session: DebugAdapterId) {
    let Some(run) = editor
        .debug
        .engine
        .full_speed
        .take_if(|run| run.session == session)
    else {
        return;
    };
    let duration = run.since.map(|since| since.elapsed()).unwrap_or_default();
    editor
        .debug
        .engine
        .record
        .mark(MarkKind::FullSpeed { duration });
    restore(editor, session);
}

/// Puts the hidden breakpoints of `session` back after a full-speed run.
fn restore(editor: &mut Editor, session: DebugAdapterId) {
    let Some(state) = editor.debug.engine.sessions.get(&session) else {
        return;
    };
    let files: Vec<(PathBuf, HiddenFile)> = state
        .files
        .iter()
        .map(|(path, file)| (path.clone(), file.clone()))
        .collect();
    let token = editor
        .debug
        .engine
        .start_task(TaskKind::Restore, session, files.len());
    if let Some(state) = editor.debug.engine.sessions.get_mut(&session) {
        state.state = SessionState::Installing;
    }
    animate(editor);
    let jobs = files
        .into_iter()
        .map(|(path, file)| {
            let mine = mine(editor, &path);
            let mine_lines: Vec<usize> = mine.iter().map(|(line, _)| *line).collect();
            let hidden = fit(
                editor.debug.engine.sessions[&session].lines_to_send(&path, &mine_lines),
                mine.len(),
            );
            FileJob {
                path,
                mine,
                action: Action::Send {
                    hidden,
                    found: (file.found, file.lines),
                },
            }
        })
        .collect::<Vec<_>>();
    let mut jobs = jobs;
    let first = first_in_line(&mut jobs, &first_files(editor, session));
    let rest = first < jobs.len();
    run_files_staged(
        editor,
        session,
        token,
        jobs,
        first,
        move |editor| go(editor, session, token, rest),
        move |editor| finish(editor, session, token, TaskKind::Restore),
    );
}

// ---------------------------------------------------------------------------------------------
// The client's fast path
// ---------------------------------------------------------------------------------------------

/// Gives the client of `session` the ids of the hidden breakpoints (and turns its fast path on
/// or off).
pub fn sync_ids(editor: &Editor, session: DebugAdapterId) {
    let Some(client) = editor.debug_adapters.get_client(session) else {
        return;
    };
    let mut filter = client.hit_filter().lock().unwrap();
    filter.enabled = editor.debug.engine.is_ready(session);
    filter.lines.clear();
    filter.strays.clear();
    if let Some(state) = editor.debug.engine.sessions.get(&session) {
        for (id, entry) in &state.ids {
            if entry.stray {
                filter.strays.insert(*id);
            } else {
                filter.lines.insert(*id);
            }
        }
    }
}

/// Tells every client which thread the editor watches (the spectated one).
pub fn sync_watch(editor: &Editor) {
    if editor.debug.engine.sessions.is_empty() {
        return;
    }
    let spectated = spectated(editor);
    for (id, client) in editor.debug_adapters.iter() {
        let mut filter = client.hit_filter().lock().unwrap();
        filter.pick = spectated.is_none();
        filter.watched = spectated
            .filter(|key| key.session == id)
            .map(|key| key.thread);
        filter.alone = hits_alone(editor, id);
    }
}

/// Whether the hits the engine continues by itself go with `singleThread`: in a session whose
/// threads stop one by one (each continues alone), and in any session with a frozen thread, so
/// that no continue of another thread's hit runs it (an all-stop adapter otherwise runs every
/// thread at a continue).
fn hits_alone(editor: &Editor, session: DebugAdapterId) -> bool {
    let spectator = &editor.debug.spectator;
    playback::supports_single_thread(editor, session)
        && (spectator.non_stop.contains(&session)
            || spectator.frozen.iter().any(|key| key.session == session))
}

/// The spectated thread, while its session lives (no fallback to the current thread: at the
/// program's first line nothing is spectated yet).
fn spectated(editor: &Editor) -> Option<ThreadKey> {
    editor
        .debug
        .spectator
        .spectated
        .filter(|key| editor.debug_adapters.get_client(key.session).is_some())
}

// ---------------------------------------------------------------------------------------------
// Hits
// ---------------------------------------------------------------------------------------------

/// What a `stopped` event became.
#[derive(Debug)]
pub enum Outcome {
    /// Taken by the engine (recorded, continued): nothing else to do. Whether to redraw.
    Done(bool),
    /// A stop to handle as usual, with this reason ([`LINE_REASON`] for a line of the spectated
    /// thread).
    Proceed(String),
}

/// Whether `id` is one of your breakpoints in `session`, and which.
fn my_breakpoint(
    editor: &Editor,
    session: DebugAdapterId,
    id: usize,
) -> Option<(&Path, &vortex_view::editor::Breakpoint)> {
    editor.breakpoints.iter().find_map(|(path, list)| {
        list.iter()
            .find(|breakpoint| {
                breakpoint
                    .sessions
                    .iter()
                    .any(|entry| entry.session == session && entry.id == Some(id))
            })
            .map(|breakpoint| (path.as_path(), breakpoint))
    })
}

/// Continues a thread whose hit the engine took.
fn continue_hit(editor: &Editor, key: ThreadKey) {
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        return;
    };
    let alone = hits_alone(editor, key.session);
    let request = client.requester().continue_with(key.thread, alone);
    tokio::spawn(async move {
        if let Err(err) = request.await {
            log::debug!("continue of a hit: {err}");
        }
    });
}

/// A `stopped` event of `session`, before anything else sees it.
pub fn on_stopped(
    editor: &mut Editor,
    session: DebugAdapterId,
    body: &StoppedBody,
    taken: Option<Taken>,
) -> Outcome {
    let reason = body.reason.clone();
    if !editor.debug.engine.is_on(session) && taken.is_none() {
        return Outcome::Proceed(reason);
    }
    let Some(thread) = body.thread_id else {
        return Outcome::Proceed(reason);
    };
    let key = ThreadKey { session, thread };
    let ids: Vec<usize> = body
        .hit_breakpoint_ids
        .iter()
        .flatten()
        .filter_map(|id| usize::try_from(*id).ok())
        .collect();
    let spectated = spectated(editor);
    match taken {
        Some(Taken::Continued) => {
            // the client continued it: only the record
            if let HitKind::Line { path, line } =
                editor.debug.engine.classify(session, &ids, |_| false)
            {
                let engine = &mut editor.debug.engine;
                engine
                    .record
                    .hit(key, Some((&path, line)), Why::Line, spectated);
                engine.others_since += 1;
            }
            return Outcome::Done(editor.debug.engine.redraw_due());
        }
        Some(Taken::Stray) => return Outcome::Done(false),
        None => {}
    }
    if body.preserve_focus_hint == Some(true) && body.all_threads_stopped != Some(false) {
        // an adapter that stops every thread reports the others too (lldb-dap): with the hidden
        // breakpoints in place that happens at every hit, and says nothing new
        return Outcome::Done(false);
    }
    // a full-speed run ends at a stop that stays: not at a stray hit, nor at a hit of one of your
    // breakpoints whose type doesn't match (both are continued)
    if body.reason == "breakpoint" && !ids.is_empty() {
        let kind = editor.debug.engine.classify(session, &ids, |id| {
            my_breakpoint(editor, session, id).is_some()
        });
        // during a full-speed run, a hit of a breakpoint nobody knows is a hidden one's that was on
        // its way when they were lifted (run to cursor's temporary one isn't set then): a stray
        let lifted = editor
            .debug
            .engine
            .full_speed
            .as_ref()
            .is_some_and(|run| run.session == session)
            && editor.debug.spectator.run_to.is_none();
        let kind = match kind {
            HitKind::Unknown if lifted => HitKind::Stray,
            kind => kind,
        };
        if !matches!(kind, HitKind::Stray | HitKind::Mine(_)) {
            end_full_speed(editor, session);
        }
        match kind {
            HitKind::Stray => {
                continue_hit(editor, key);
                return Outcome::Done(false);
            }
            HitKind::Line { path, line } => {
                let spectator = &editor.debug.spectator;
                let running_to = spectator.run_to.is_some();
                let decision = decide_line(key, spectated, running_to, spectator.mode);
                let watched = match decision {
                    LineDecision::Spectate => Some(key),
                    _ => spectated,
                };
                let engine = &mut editor.debug.engine;
                let at = engine
                    .record
                    .hit(key, Some((&path, line)), Why::Line, watched);
                match decision {
                    LineDecision::Continue => {
                        engine.others_since += 1;
                        continue_hit(editor, key);
                        return Outcome::Done(editor.debug.engine.redraw_due());
                    }
                    LineDecision::Check => {
                        let target = editor.debug.spectator.run_to.as_ref();
                        if !target.is_some_and(|target| target.line == line && target.path == *path)
                        {
                            continue_hit(editor, key);
                            return Outcome::Done(false);
                        }
                    }
                    LineDecision::Spectate => {
                        editor.debug.spectator.spectated = Some(key);
                        sync_watch(editor);
                        crate::debug::mode::say(
                            editor,
                            "Stopped at the first line of your code that ran",
                        );
                    }
                    LineDecision::Stop => {}
                }
                editor.debug.engine.last_stop = Some((key, at));
                return Outcome::Proceed(LINE_REASON.to_owned());
            }
            HitKind::Mine(mine) => {
                let main = editor.debug.engine.record.main_thread;
                let place = my_breakpoint(editor, session, mine[0])
                    .map(|(path, breakpoint)| (path.to_owned(), breakpoint.line));
                let pauses = mine.iter().any(|id| {
                    my_breakpoint(editor, session, *id).is_some_and(|(_, breakpoint)| {
                        engine::breakpoint_pauses(breakpoint, key, main)
                    })
                });
                // a stop at yours ends a full-speed run (its gap is marked before this hit), and
                // takes the camera: the switch is marked at this hit
                if pauses {
                    end_full_speed(editor, session);
                }
                let watched = if pauses { Some(key) } else { spectated };
                let at = editor.debug.engine.record.hit(
                    key,
                    place.as_ref().map(|(path, line)| (path.as_path(), *line)),
                    Why::Breakpoint,
                    watched,
                );
                if !pauses {
                    continue_hit(editor, key);
                    return Outcome::Done(false);
                }
                editor.debug.engine.manual = None;
                editor.debug.engine.last_stop = Some((key, at));
                return Outcome::Proceed(reason);
            }
            HitKind::Unknown => {}
        }
    }
    // an exception, a pause, a step, the entry: recorded, placed once its frames load (a
    // breakpoint neither yours nor hidden is run to cursor's temporary one)
    end_full_speed(editor, session);
    editor.debug.engine.manual = None;
    let why = match Why::from_reason(&reason) {
        Why::Breakpoint => Why::Other,
        why => why,
    };
    let at = editor.debug.engine.record.hit(key, None, why, spectated);
    editor.debug.engine.last_stop = Some((key, at));
    editor.debug.engine.unplaced = Some((key, at));
    Outcome::Proceed(reason)
}

/// The frames of a stop loaded: places a stop that wasn't a line on the lineline, and notes the
/// files of followed frames no hidden breakpoints cover (the stepping fallback covers them; `lib`
/// and `std` install them in the background).
pub fn on_frames(editor: &mut Editor, key: ThreadKey, frames: &[StackFrame]) {
    let place = |frame: &StackFrame| {
        Some((
            frame.source.as_ref()?.path.clone()?,
            frame.line.saturating_sub(1),
        ))
    };
    if let Some((unplaced_key, at)) = editor.debug.engine.unplaced.take() {
        if unplaced_key == key {
            if let Some((path, line)) = frames.first().and_then(place) {
                editor.debug.engine.record.relocate(at, &path, line);
            }
        }
    }
    if !editor.debug.engine.is_ready(key.session) {
        return;
    }
    let follow = editor.debug.spectator.follow;
    if follow == Follow::Own {
        return;
    }
    let rules = playback::origin_rules(editor, key.session);
    let Some(state) = editor.debug.engine.sessions.get_mut(&key.session) else {
        return;
    };
    let mut new = BTreeSet::new();
    for frame in frames {
        let Some((path, _)) = place(frame) else {
            continue;
        };
        if follow.allows(rules.classify(frame))
            && !state.covers(&path)
            && state.uncovered.insert(path.clone())
            && path.is_file()
        {
            new.insert(path);
        }
    }
    if !new.is_empty() && editor.debug.engine.task.is_none() {
        install_more(editor, key.session, new);
    }
}

/// Installs a few more files (frames seen in `lib` and `std` mode), in the background.
fn install_more(editor: &mut Editor, session: DebugAdapterId, files: BTreeSet<PathBuf>) {
    let token = editor
        .debug
        .engine
        .start_task(TaskKind::Reinstall, session, files.len());
    animate(editor);
    let jobs = files
        .into_iter()
        .filter_map(|path| {
            let lines = count_lines(&path)?;
            Some(FileJob {
                mine: mine(editor, &path),
                action: Action::Discover { lines, known: None },
                path,
            })
        })
        .collect();
    run_files(editor, session, token, jobs, move |editor| {
        if let Some(state) = editor.debug.engine.sessions.get_mut(&session) {
            let covered: Vec<PathBuf> = state.files.keys().cloned().collect();
            for path in covered {
                state.uncovered.remove(&path);
            }
        }
        finish(editor, session, token, TaskKind::Reinstall)
    });
}

/// How playback advances the spectated thread from `top`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// `continue` to its next line.
    Continue,
    /// A step: no hidden breakpoints (or none in the followed file it is in: the fallback).
    Step,
    /// Wait: the hidden breakpoints are being put back.
    Wait,
}

pub fn advance_kind(editor: &Editor, key: ThreadKey, top: Option<&StackFrame>) -> Advance {
    let engine = &editor.debug.engine;
    if engine.kind != engine::EngineKind::Breakpoints {
        return Advance::Step;
    }
    let Some(state) = engine.sessions.get(&key.session) else {
        return Advance::Step;
    };
    match state.state {
        SessionState::Off => return Advance::Step,
        SessionState::Installing | SessionState::Lifted => return Advance::Wait,
        SessionState::Ready => {}
    }
    let uncovered = top.is_some_and(|frame| {
        let Some(path) = frame
            .source
            .as_ref()
            .and_then(|source| source.path.as_ref())
        else {
            return false;
        };
        let rules = playback::origin_rules(editor, key.session);
        editor.debug.spectator.follow.allows(rules.classify(frame))
            && !state.covers(path)
            && path.is_file()
    });
    if uncovered {
        Advance::Step
    } else {
        Advance::Continue
    }
}

/// A manual step (debug mode's `.`, `i`, `o`) of `key` with the hidden breakpoints in place: the
/// thread continues and its next hits end the step where a step would have. Returns false when
/// the engine doesn't take it (the caller sends the step).
pub fn manual_step(editor: &mut Editor, key: ThreadKey, kind: StepKind) -> bool {
    if !editor.debug.engine.is_ready(key.session) {
        return false;
    }
    let depth = editor
        .debug_adapters
        .get_client(key.session)
        .and_then(|client| client.stack_frames.get(&key.thread))
        .map_or(0, Vec::len);
    if depth == 0 {
        return false;
    }
    editor.debug.engine.manual = Some(ManualStep { key, kind, depth });
    editor.debug.spectator.spectated = Some(key);
    sync_watch(editor);
    super::threads::continue_thread(editor, key);
    true
}

/// A line of the stepping thread during a manual step: whether the step ends here (else it
/// continues, quietly).
pub fn manual_step_ends(editor: &mut Editor, key: ThreadKey, frames: &[StackFrame]) -> bool {
    let Some(step) = editor.debug.engine.manual.filter(|step| step.key == key) else {
        return true;
    };
    if step.ends_at(frames.len()) {
        editor.debug.engine.manual = None;
        return true;
    }
    continue_hit(editor, key);
    false
}
