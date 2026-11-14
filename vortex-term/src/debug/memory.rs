//! A debugged program's memory in the hex view: opening it from a value or an address, reading
//! what the views show (`readMemory`), refreshing it at each stop, overwriting it while paused
//! (`writeMemory`), and recording it for rewind.
//!
//! Everything is generic DAP: whether memory is offered depends only on the adapter's
//! capabilities (`supportsReadMemoryRequest`, `supportsWriteMemoryRequest`) and on each value's
//! `memoryReference`. The bytes themselves live in the document's
//! [`MemorySource`](vortex_view::hex::MemorySource): drawing notes the pages it lacks, and
//! [`load_wanted`], called after each frame, reads them.

use std::time::{Duration, Instant};

use vortex_dap::{parse_address, registry::DebugAdapterId, Requester};
use vortex_view::{
    debug::{history::MemoryWindow, spectator::Mode, Target},
    editor::Action,
    hex::{
        memory::{page_bytes, span_bytes, Recorded, PAGE},
        MemorySource, Rows,
    },
    Document, DocumentId, Editor,
};

use super::panel;
use crate::{job, ui::hex_view};

/// While playing, memory views are read at most this often (about the screen's rate), not at
/// every step.
const REFRESH_WHILE_PLAYING: Duration = Duration::from_millis(33);
/// Bytes recorded around what a memory view shows, on each side.
const RECORD_MARGIN: u64 = 512;

fn caps(editor: &Editor, session: DebugAdapterId) -> Option<&vortex_dap::DebuggerCapabilities> {
    editor.debug_adapters.get_client(session)?.caps.as_ref()
}

/// The adapter of `session` reads memory.
pub fn supports_read(editor: &Editor, session: DebugAdapterId) -> bool {
    caps(editor, session)
        .and_then(|caps| caps.supports_read_memory_request)
        .unwrap_or_default()
}

/// The adapter of `session` writes memory.
pub fn supports_write(editor: &Editor, session: DebugAdapterId) -> bool {
    caps(editor, session)
        .and_then(|caps| caps.supports_write_memory_request)
        .unwrap_or_default()
}

/// The session the panel follows.
fn session(editor: &Editor) -> Option<DebugAdapterId> {
    editor
        .debug_adapters
        .get_active_client()
        .map(|client| client.id())
}

/// Whether the panel shows memory badges: the session it follows reads memory.
pub fn shown(editor: &Editor) -> bool {
    session(editor).is_some_and(|session| supports_read(editor, session))
}

/// The debugger's name, for messages: `lldb-dap`, `js-debug`.
fn debugger_name(editor: &Editor, session: DebugAdapterId) -> String {
    editor
        .debug_adapters
        .get_client(session)
        .map(|client| match &client.config {
            Some(config) if !config.name.is_empty() => config.name.clone(),
            _ => client.name.clone(),
        })
        .unwrap_or_else(|| "this session".into())
}

fn unsupported(editor: &Editor, session: DebugAdapterId) -> String {
    format!(
        "this debugger ({}) doesn't support viewing memory",
        debugger_name(editor, session)
    )
}

/// Whether the program of `session` is stopped, so its memory can be read or written.
fn session_stopped(editor: &Editor, session: DebugAdapterId) -> bool {
    let Some(client) = editor.debug_adapters.get_client(session) else {
        return false;
    };
    if client
        .thread_states
        .values()
        .any(|state| state != "running")
    {
        return true;
    }
    client.thread_states.is_empty()
        && matches!(
            editor.debug.status,
            vortex_view::debug::Status::Stopped { .. }
        )
        && session_of_panel(editor) == Some(session)
}

fn session_of_panel(editor: &Editor) -> Option<DebugAdapterId> {
    session(editor)
}

// Opening

/// `M` in the panel: the memory of the value under the cursor, in Variables or Watch.
pub fn open_selected(editor: &mut Editor) {
    let Some(session) = session(editor) else {
        crate::debug::mode::say(editor, "No debug session");
        return;
    };
    if !supports_read(editor, session) {
        crate::debug::mode::say(editor, unsupported(editor, session));
        return;
    }
    let node = match panel::current_target(editor) {
        Some(Target::Var(path)) => editor.debug.node(&path).cloned(),
        _ => None,
    };
    let Some(node) = node else {
        editor.set_error("no value with memory here");
        return;
    };
    if editor.debug.is_rewinding() {
        crate::debug::mode::say(editor, REWIND_OPEN);
        return;
    }
    match node.memory_reference {
        Some(reference) => open(editor, session, reference, node.name),
        None => crate::debug::mode::say(editor, format!("{} has no memory to show", node.name)),
    }
}

/// What opening memory says in rewind: the references of a recorded stop are gone.
const REWIND_OPEN: &str = "REWIND: a recorded stop's memory can't be read any more";

/// `:debug-memory <expression|0xaddress>`: an address opens there, an expression opens the
/// memory its value points to. Without an argument, the value under the panel's cursor.
pub fn command(editor: &mut Editor, input: Option<&str>) -> anyhow::Result<()> {
    let Some(input) = input.map(str::trim).filter(|input| !input.is_empty()) else {
        open_selected(editor);
        return Ok(());
    };
    let Some(session) = session(editor) else {
        anyhow::bail!("No debug session");
    };
    if !supports_read(editor, session) {
        crate::debug::mode::say(editor, unsupported(editor, session));
        return Ok(());
    }
    if editor.debug.is_rewinding() {
        crate::debug::mode::say(editor, REWIND_OPEN);
        return Ok(());
    }
    if let Some(address) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
        .and_then(|hex| u64::from_str_radix(&hex.replace('_', ""), 16).ok())
    {
        open_address(editor, session, address);
        return Ok(());
    }
    // an expression, in the current frame
    let Some(client) = editor.debug_adapters.get_client(session) else {
        anyhow::bail!("No debug session");
    };
    let frame = client
        .thread_id
        .and_then(|thread| client.stack_frames.get(&thread))
        .and_then(|frames| frames.get(client.active_frame.unwrap_or(0)))
        .map(|frame| frame.id);
    let requester = client.requester();
    let expression = input.to_owned();
    tokio::spawn(async move {
        let result = requester
            .evaluate(expression.clone(), frame, Some("watch"))
            .await;
        job::dispatch(move |editor, _| match result {
            Ok(response) => match response.memory_reference {
                Some(reference) => open(editor, session, reference, expression),
                None => {
                    crate::debug::mode::say(editor, format!("{expression} doesn't point to memory"))
                }
            },
            Err(err) => editor.set_error(format!("{expression}: {err}")),
        })
        .await;
    });
    Ok(())
}

/// Opens memory at an address. Reads need a memory reference: the reference of a memory view
/// of the session already open, with an offset (DAP's references point into one address
/// space), else the address itself, which is what most adapters' references are.
fn open_address(editor: &mut Editor, session: DebugAdapterId, address: u64) {
    let known = memory_docs(editor)
        .into_iter()
        .filter_map(|doc| editor.documents.get(&doc)?.memory())
        .find(|memory| memory.session == session && !memory.ended)
        .map(|memory| (memory.reference.clone(), memory.base));
    let label = format!("0x{address:x}");
    match known {
        Some((reference, base)) => show(editor, session, reference, base, address, label),
        None => resolve(editor, session, label.clone(), label, Some(address)),
    }
}

/// Opens the memory `reference` points to.
fn open(editor: &mut Editor, session: DebugAdapterId, reference: String, label: String) {
    resolve(editor, session, reference, label, None);
}

/// Finds the address `reference` points to (the address `readMemory` answers with), then
/// shows it.
fn resolve(
    editor: &mut Editor,
    session: DebugAdapterId,
    reference: String,
    label: String,
    address: Option<u64>,
) {
    let Some(client) = editor.debug_adapters.get_client(session) else {
        return;
    };
    let requester = client.requester();
    tokio::spawn(async move {
        let read = requester.read_memory(reference.clone(), 0, 1).await;
        job::dispatch(move |editor, _| {
            let base = match &read {
                Ok(Some(read)) => Some(read.address),
                _ => parse_address(&reference),
            };
            match base {
                Some(base) => {
                    let at = address.unwrap_or(base);
                    show(editor, session, reference, base, at, label)
                }
                None => {
                    let why = match read {
                        Err(err) => err.to_string(),
                        _ => "the debugger didn't say where it is".into(),
                    };
                    editor.set_error(format!("can't open the memory of {label}: {why}"))
                }
            }
        })
        .await;
    });
}

/// The documents showing memory.
fn memory_docs(editor: &Editor) -> Vec<DocumentId> {
    editor
        .documents()
        .filter(|doc| doc.memory().is_some())
        .map(Document::id)
        .collect()
}

/// Shows memory of `session` at `address` in a split (or the view showing it already), the
/// address in the middle.
fn show(
    editor: &mut Editor,
    session: DebugAdapterId,
    reference: String,
    base: u64,
    address: u64,
    label: String,
) {
    if editor.debug_adapters.get_client(session).is_none() {
        return;
    }
    editor.debug.unfocus();
    let existing = memory_docs(editor).into_iter().find(|&id| {
        editor.documents[&id]
            .memory()
            .is_some_and(|memory| memory.session == session && memory.base == base && !memory.ended)
    });
    match existing {
        Some(doc) => {
            let view = editor
                .tree
                .views()
                .find(|(view, _)| view.doc == doc)
                .map(|(view, _)| view.id);
            match view {
                Some(view) => editor.focus(view),
                None => editor.switch(doc, Action::VerticalSplit),
            }
        }
        None => {
            let memory = MemorySource::new(
                session,
                reference,
                base,
                label.clone(),
                supports_write(editor, session),
            );
            let doc =
                Document::open_memory(memory, editor.config.clone(), editor.syn_loader.clone());
            editor.new_file_from_document(Action::VerticalSplit, doc);
        }
    }
    center(editor, address);
}

/// Puts the cursor of the focused memory view on `address`, in the middle of the view.
fn center(editor: &mut Editor, address: u64) {
    let (view, doc) = current!(editor);
    let area = hex_view::rows_area(view);
    let view_id = view.id;
    let Some(hex) = doc.hex_mut() else {
        return;
    };
    let layout = hex.cursor(view_id).layout;
    let rows = Rows::new(layout, hex.len(), area.width);
    let per_row = rows.bytes_per_row as u64;
    let cursor = hex.cursor_mut(view_id);
    cursor.offset = address;
    cursor.anchor = None;
    cursor.overwrite = false;
    let row = rows.row(address);
    cursor.top = row.saturating_sub(area.height as u64 / 2) * per_row;
}

// Reading

/// Reads the pages the memory views drew without having them (or had from an earlier stop),
/// while their program is stopped. Called after each frame. In rewind, they show the bytes
/// recorded at the stop shown instead.
pub fn load_wanted(editor: &mut Editor) {
    let playing = editor.debug.spectator.mode != Mode::Paused;
    let docs = memory_docs(editor);
    for id in docs {
        let Some(memory) = editor.documents[&id].memory() else {
            continue;
        };
        if memory.recorded.is_some() || memory.ended || !memory.has_wanted() {
            continue;
        }
        let session = memory.session;
        if !session_stopped(editor, session) {
            continue;
        }
        let Some(requester) = editor
            .debug_adapters
            .get_client(session)
            .map(|client| client.requester())
        else {
            continue;
        };
        let now = Instant::now();
        if let Some(last) = memory.refreshed.filter(|_| playing) {
            let since = now.saturating_duration_since(last);
            if since < REFRESH_WHILE_PLAYING {
                // the next frame after the wait reads them
                let wait = REFRESH_WHILE_PLAYING - since;
                tokio::spawn(async move {
                    tokio::time::sleep(wait).await;
                    vortex_event::request_redraw();
                });
                continue;
            }
        }
        let doc = editor.documents.get_mut(&id).unwrap();
        let Some(memory) = doc.hex_mut().and_then(|hex| hex.memory_mut()) else {
            continue;
        };
        memory.refreshed = Some(now);
        let stop = memory.stop();
        let reference = memory.reference.clone();
        let reads: Vec<(u64, i64)> = memory
            .take_wanted()
            .into_iter()
            .map(|page| (page, memory.offset_of(page)))
            .collect();
        for (page, offset) in reads {
            read_page(requester.clone(), id, reference.clone(), page, offset, stop);
        }
    }
}

/// Reads one page in the background and puts it back in its document.
fn read_page(
    requester: Requester,
    doc: DocumentId,
    reference: String,
    page: u64,
    offset: i64,
    stop: u64,
) {
    tokio::spawn(async move {
        let bytes = match requester
            .read_memory(reference, offset, PAGE as usize)
            .await
        {
            Ok(Some(read)) => page_bytes(page, read.address, &read.data),
            // nothing readable there, or a read that failed: `??`, never an error
            Ok(None) | Err(_) => vec![None; PAGE as usize],
        };
        job::dispatch(move |editor, _| {
            if let Some(memory) = editor
                .documents
                .get_mut(&doc)
                .and_then(|doc| doc.hex_mut())
                .and_then(|hex| hex.memory_mut())
            {
                memory.fill(page, bytes, stop);
            }
        })
        .await;
    });
}

/// The program of `session` stopped: its memory views read what they show again.
pub fn on_stop(editor: &mut Editor, session: DebugAdapterId) {
    for id in memory_docs(editor) {
        if let Some(memory) = editor
            .documents
            .get_mut(&id)
            .and_then(|doc| doc.hex_mut())
            .and_then(|hex| hex.memory_mut())
            .filter(|memory| memory.session == session)
        {
            memory.next_stop();
        }
    }
}

/// The adapter says memory changed (the `memory` event): read it again.
pub fn on_memory_event(editor: &mut Editor, session: DebugAdapterId) {
    on_stop(editor, session);
}

/// Session `session` ended: its memory views keep their last bytes.
pub fn session_ended(editor: &mut Editor, session: DebugAdapterId) {
    for id in memory_docs(editor) {
        if let Some(memory) = editor
            .documents
            .get_mut(&id)
            .and_then(|doc| doc.hex_mut())
            .and_then(|hex| hex.memory_mut())
            .filter(|memory| memory.session == session)
        {
            memory.ended = true;
        }
    }
}

/// What the statusline says about a memory view, after the position: the stop it shows when
/// that isn't the live one.
pub fn note(editor: &Editor, doc: &Document) -> Option<String> {
    let memory = doc.memory()?;
    // (the statusline's debug element says REWIND and the step)
    if let Some(recorded) = &memory.recorded {
        return Some(if recorded.bytes.is_empty() {
            "not recorded at this stop".to_owned()
        } else {
            format!(
                "{} recorded at this stop",
                vortex_view::debug::history::format_bytes(recorded.bytes.len())
            )
        });
    }
    if memory.ended {
        return Some("session ended · its last bytes".into());
    }
    if !session_stopped(editor, memory.session) {
        return Some("running · the last stop's bytes".into());
    }
    None
}

// Rewind

/// In rewind, shows each memory view's bytes recorded at the stop shown (nothing for a view
/// that wasn't recorded then); back live, memory again.
pub fn sync_rewind(editor: &mut Editor) {
    let history = &editor.debug.spectator.history;
    let shown = history.at.and_then(|at| {
        let snapshot = history.stops.get(at)?;
        let previous = at
            .checked_sub(1)
            .and_then(|before| history.stops.get(before));
        Some((
            snapshot.step,
            snapshot.memory.clone(),
            previous.map(|p| p.memory.clone()),
        ))
    });
    for id in memory_docs(editor) {
        let Some(memory) = editor
            .documents
            .get_mut(&id)
            .and_then(|doc| doc.hex_mut())
            .and_then(|hex| hex.memory_mut())
        else {
            continue;
        };
        match &shown {
            None => memory.recorded = None,
            Some((step, windows, previous)) => {
                if memory.recorded.as_ref().is_some_and(|r| r.step == *step) {
                    continue;
                }
                let window = windows.iter().find(|window| window.doc == id);
                let before = previous
                    .as_ref()
                    .and_then(|windows| windows.iter().find(|window| window.doc == id));
                memory.recorded = Some(match window {
                    Some(window) => Recorded {
                        step: *step,
                        start: window.start,
                        changed: changed_since(window, before),
                        bytes: window.bytes.clone(),
                    },
                    None => Recorded {
                        step: *step,
                        start: 0,
                        bytes: Vec::new(),
                        changed: Vec::new(),
                    },
                });
            }
        }
    }
}

/// The bytes of `window` that differ from the same addresses in `before`.
fn changed_since(window: &MemoryWindow, before: Option<&MemoryWindow>) -> Vec<bool> {
    let Some(before) = before else {
        return vec![false; window.bytes.len()];
    };
    window
        .bytes
        .iter()
        .enumerate()
        .map(|(i, byte)| {
            let address = window.start + i as u64;
            let old = address
                .checked_sub(before.start)
                .and_then(|index| before.bytes.get(index as usize))
                .copied()
                .flatten();
            byte.is_some() && old.is_some() && *byte != old
        })
        .collect()
}

/// A memory view's window to record at a stop.
pub struct ToRecord {
    pub doc: DocumentId,
    pub reference: String,
    pub base: u64,
    pub start: u64,
    pub len: usize,
}

/// The windows of the memory views of `session` to record at a stop: what each view shows and
/// a margin, at most `cap` bytes per memory document (`None`: all of it).
pub fn to_record(editor: &Editor, session: DebugAdapterId, cap: Option<usize>) -> Vec<ToRecord> {
    if cap == Some(0) {
        return Vec::new();
    }
    let mut windows = Vec::new();
    for id in memory_docs(editor) {
        let doc = &editor.documents[&id];
        let (Some(hex), Some(memory)) = (doc.hex(), doc.memory()) else {
            continue;
        };
        if memory.session != session || memory.ended {
            continue;
        }
        // what its views show
        let mut span: Option<(u64, u64)> = None;
        for (view, _) in editor.tree.views().filter(|(view, _)| view.doc == id) {
            let cursor = hex.cursor(view.id);
            let area = hex_view::rows_area(view);
            let rows = Rows::new(cursor.layout, hex.len(), area.width);
            let per_row = rows.bytes_per_row as u64;
            let start = rows.row(cursor.top) * per_row;
            let end = start.saturating_add(area.height as u64 * per_row);
            span = Some(match span {
                Some((from, to)) => (from.min(start), to.max(end)),
                None => (start, end),
            });
        }
        let Some((from, to)) = span else {
            continue;
        };
        let mut start = from.saturating_sub(RECORD_MARGIN);
        let mut end = to.saturating_add(RECORD_MARGIN);
        if let Some(cap) = cap.map(|cap| cap as u64) {
            if end - start > cap {
                // the middle of what is shown
                let middle = from + (to - from) / 2;
                start = middle.saturating_sub(cap / 2);
                end = start.saturating_add(cap);
            }
        }
        windows.push(ToRecord {
            doc: id,
            reference: memory.reference.clone(),
            base: memory.base,
            start,
            len: (end - start) as usize,
        });
    }
    windows
}

/// Reads a window to record, a page at a time (so an unreadable page doesn't hide its
/// neighbours). Returns its bytes and the number of requests.
pub async fn read_window(requester: &Requester, window: &ToRecord) -> (Vec<Option<u8>>, usize) {
    let mut bytes = Vec::with_capacity(window.len);
    let mut requests = 0;
    let end = window.start.saturating_add(window.len as u64);
    let mut at = window.start;
    while at < end {
        let chunk_end = ((at / PAGE + 1) * PAGE).min(end);
        let len = (chunk_end - at) as usize;
        let offset = at.wrapping_sub(window.base) as i64;
        requests += 1;
        match requester
            .read_memory(window.reference.clone(), offset, len)
            .await
        {
            Ok(Some(read)) => bytes.extend(span_bytes(at, len, read.address, &read.data)),
            Ok(None) | Err(_) => bytes.extend(std::iter::repeat_n(None, len)),
        }
        at = chunk_end;
    }
    (bytes, requests)
}

/// The number of memory views of the session the panel follows, for the capture's cost.
pub fn open_views(editor: &Editor) -> usize {
    let Some(session) = session(editor) else {
        return 0;
    };
    memory_docs(editor)
        .into_iter()
        .filter(|id| {
            editor.documents[id]
                .memory()
                .is_some_and(|memory| memory.session == session && !memory.ended)
                && editor.tree.views().any(|(view, _)| view.doc == *id)
        })
        .count()
}

// Overwriting

/// Why the focused memory view can't overwrite memory now, if it can't.
pub fn not_editable(editor: &Editor) -> Option<String> {
    let doc = doc!(editor);
    let memory = doc.memory()?;
    if !memory.can_write {
        return Some(format!(
            "this debugger ({}) can't write memory",
            debugger_name(editor, memory.session)
        ));
    }
    if editor.debug.is_rewinding() {
        return Some("REWIND: nothing is executing, so memory can't be changed".into());
    }
    if memory.ended {
        return Some("the debug session ended: its memory is gone".into());
    }
    match editor.debug.spectator.mode {
        Mode::Playing => return Some("playing: pause to overwrite memory".into()),
        Mode::FastForward => return Some("fast forwarding: stop it to overwrite memory".into()),
        Mode::Paused => {}
    }
    if !session_stopped(editor, memory.session) {
        return Some("the program is running: pause it to overwrite memory".into());
    }
    None
}

/// `:w` in a memory view: sends the overwritten bytes with `writeMemory`, and says how many
/// the debugger wrote.
pub fn write(editor: &mut Editor) -> anyhow::Result<()> {
    if let Some(why) = not_editable(editor) {
        anyhow::bail!(why);
    }
    let doc = doc!(editor);
    let id = doc.id();
    let Some(hex) = doc.hex() else {
        return Ok(());
    };
    let runs = hex.edit_runs(None);
    if runs.is_empty() {
        crate::debug::mode::say(editor, "no overwritten bytes to write");
        return Ok(());
    }
    send(editor, id, runs, None, "wrote");
    Ok(())
}

/// After `u` or `U` changed bytes already written to memory: writes them again, so memory
/// has what the view shows.
pub fn write_back(editor: &mut Editor, offsets: Vec<u64>, undo: bool) {
    let doc = doc!(editor);
    let id = doc.id();
    let Some(hex) = doc.hex() else {
        return;
    };
    let runs = hex.edit_runs(Some(&offsets));
    if !runs.is_empty() {
        let what = if undo { "wrote back" } else { "wrote again" };
        send(editor, id, runs, Some(offsets), what);
    }
}

/// Writes `runs` of bytes to the memory of document `id`, then marks them written (only
/// `offsets` when given, else all the overwritten bytes).
fn send(
    editor: &mut Editor,
    id: DocumentId,
    runs: Vec<(u64, Vec<u8>)>,
    offsets: Option<Vec<u64>>,
    what: &'static str,
) {
    let Some(memory) = editor.documents[&id].memory() else {
        return;
    };
    let Some(requester) = editor
        .debug_adapters
        .get_client(memory.session)
        .map(|client| client.requester())
    else {
        editor.set_error("the debug session ended: its memory is gone");
        return;
    };
    let reference = memory.reference.clone();
    let base = memory.base;
    let sent: usize = runs.iter().map(|(_, bytes)| bytes.len()).sum();
    let written_offsets: Vec<u64> = runs
        .iter()
        .flat_map(|(start, bytes)| (0..bytes.len() as u64).map(move |i| start + i))
        .collect();
    tokio::spawn(async move {
        let mut written = 0;
        let mut failure = None;
        for (start, bytes) in runs {
            let offset = start.wrapping_sub(base) as i64;
            match requester
                .write_memory(reference.clone(), offset, &bytes)
                .await
            {
                Ok(count) => written += count,
                Err(err) => {
                    failure = Some(format!("at 0x{start:x}: {err}"));
                    break;
                }
            }
        }
        job::dispatch(move |editor, _| {
            if let Some(err) = failure {
                editor.set_error(format!("writing memory failed {err}"));
                return;
            }
            if let Some(hex) = editor.documents.get_mut(&id).and_then(|doc| doc.hex_mut()) {
                match offsets {
                    Some(_) => hex.memory_written(Some(&written_offsets)),
                    None if hex
                        .edit_runs(None)
                        .iter()
                        .map(|(_, b)| b.len())
                        .sum::<usize>()
                        == sent =>
                    {
                        hex.memory_written(None)
                    }
                    // more bytes overwritten meanwhile: only the ones sent are written
                    None => hex.memory_written(Some(&written_offsets)),
                }
            }
            // the values in the panel may live in those bytes
            super::load::reload_frame(editor);
            let s = if sent == 1 { "" } else { "s" };
            crate::debug::mode::say(
                editor,
                format!("{what} {sent} byte{s} to memory: the debugger wrote {written}"),
            );
        })
        .await;
    });
}
