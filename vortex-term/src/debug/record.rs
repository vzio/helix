//! Recording every stop for rewind: the call stack of the thread and the variables of all its
//! frames (within the capture caps) and the watches, loaded in the background while the stop is
//! shown. Playback waits for the recording before its next step, because the references a stop
//! gives are only valid until the program moves.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use futures_util::future::{join_all, BoxFuture, FutureExt};
use tokio::sync::oneshot;
use vortex_dap::{registry::DebugAdapterId, Requester, StackFrame};
use vortex_view::{
    debug::{
        history::{Caps, MemoryWindow, Snapshot},
        mark_changes,
        spectator::ThreadKey,
        VarNode, Watch,
    },
    Editor,
};

use crate::job;

/// What one capture counts as it goes.
#[derive(Default)]
struct Counters {
    requests: AtomicUsize,
    truncated: AtomicBool,
}

/// Records the stop of `key` at `frames` in the background. The receiver fires once the
/// snapshot is kept; `None` when recording is off.
pub fn capture(
    editor: &mut Editor,
    session: DebugAdapterId,
    key: ThreadKey,
    reason: &str,
    frames: Vec<StackFrame>,
) -> Option<oneshot::Receiver<()>> {
    let caps = editor.debug.spectator.caps;
    if !caps.records() {
        return None;
    }
    let requester = editor.debug_adapters.get_client(session)?.requester();
    let watches: Vec<String> = editor
        .debug
        .watches
        .iter()
        .map(|watch| watch.expression.clone())
        .collect();
    // frames playback doesn't follow (the runtime, in `own` mode) keep their own variables, not
    // what is inside them: a deep runtime stack would cost hundreds of requests at every stop
    let rules = super::playback::origin_rules(editor, session);
    let follow = editor.debug.spectator.follow;
    let followed: Vec<bool> = frames
        .iter()
        .map(|frame| follow.allows(rules.classify(frame)))
        .collect();
    // the memory views of the session, around what they show
    let windows = super::memory::to_record(editor, session, caps.memory);
    let step = editor.debug.spectator.history.next_step();
    // the hit the engine recorded for this stop, if it is one
    let counter = editor
        .debug
        .engine
        .last_stop
        .filter(|(stopped, _)| *stopped == key)
        .map(|(_, at)| at);
    let thread_name = editor.debug.thread_name(key);
    let reason = reason.to_owned();
    let (done, receiver) = oneshot::channel();
    editor.debug.spectator.recording += 1;
    let capture_started = std::time::Instant::now();
    tokio::spawn(async move {
        let counters = Arc::new(Counters::default());
        let scopes = join_all(frames.iter().enumerate().map(|(index, frame)| {
            let caps = if followed[index] {
                caps
            } else {
                Caps {
                    depth: Some(caps.depth.map_or(1, |depth| depth.min(1))),
                    ..caps
                }
            };
            capture_frame(
                requester.clone(),
                frame.id,
                index == 0,
                caps,
                counters.clone(),
            )
        }))
        .await;
        let mut watch_values = Vec::with_capacity(watches.len());
        for expression in watches {
            let node = match frames.first() {
                Some(top) => {
                    counters.requests.fetch_add(1, Ordering::Relaxed);
                    match requester
                        .evaluate(expression.clone(), Some(top.id), Some("watch"))
                        .await
                    {
                        Ok(response) => {
                            let mut node = VarNode {
                                name: expression.clone(),
                                value: response.result,
                                ty: response.ty,
                                reference: response.variables_reference,
                                evaluate_name: Some(expression.clone()),
                                memory_reference: response.memory_reference,
                                ..Default::default()
                            };
                            if node.reference > 0 {
                                node.children = Some(
                                    children(
                                        requester.clone(),
                                        node.reference,
                                        None,
                                        caps.depth.map(|depth| depth.saturating_sub(1)),
                                        caps,
                                        counters.clone(),
                                    )
                                    .await,
                                );
                            }
                            Some(node)
                        }
                        Err(err) => Some(VarNode {
                            name: expression.clone(),
                            value: err.to_string(),
                            error: true,
                            ..Default::default()
                        }),
                    }
                }
                None => None,
            };
            watch_values.push(Watch { expression, node });
        }
        let mut memory = Vec::with_capacity(windows.len());
        for window in &windows {
            let (bytes, requests) = super::memory::read_window(&requester, window).await;
            counters.requests.fetch_add(requests, Ordering::Relaxed);
            memory.push(MemoryWindow {
                doc: window.doc,
                start: window.start,
                bytes,
            });
        }
        let mut snapshot = Snapshot {
            step,
            counter,
            key,
            thread_name,
            reason,
            frames,
            scopes,
            watches: watch_values,
            memory,
            requests: counters.requests.load(Ordering::Relaxed),
            bytes: 0,
            truncated: counters.truncated.load(Ordering::Relaxed),
        };
        snapshot.measure();
        log::info!(
            "TIMING capture {} requests in {:?}",
            snapshot.requests,
            capture_started.elapsed()
        );
        job::dispatch(move |editor, _| keep(editor, snapshot)).await;
        let _ = done.send(());
    });
    Some(receiver)
}

/// Keeps a snapshot, its changes marked against the stop before it in the same function.
fn keep(editor: &mut Editor, mut snapshot: Snapshot) {
    let spectator = &mut editor.debug.spectator;
    spectator.recording = spectator.recording.saturating_sub(1);
    let history = &mut editor.debug.spectator.history;
    if let Some(previous) = history.stops.back() {
        let same_function = previous.key == snapshot.key
            && previous.frames.first().map(|f| &f.name) == snapshot.frames.first().map(|f| &f.name);
        if same_function {
            if let (Some(Some(new)), Some(Some(old))) =
                (snapshot.scopes.first_mut(), previous.scopes.first())
            {
                mark_changes(new, old);
            }
        }
    }
    let limit = editor.debug.spectator.caps.history;
    editor.debug.spectator.history.push(snapshot, limit);
}

/// How deep a scope is recorded: the variables of a frame (`locals`, `arguments`, a return
/// value) down to the depth cap; an expensive scope (as the adapter marks it: js-debug's global
/// object) only its own variables, in the top frame; registers only their groups; and a scope
/// that is the same in every frame (globals, statics: no hint, in a frame that hints its locals)
/// only in the top frame.
fn scope_depth(
    scope: &vortex_dap::Scope,
    top: bool,
    hinted_frame: bool,
    caps: Caps,
) -> Option<usize> {
    match scope.presentation_hint.as_deref() {
        Some("registers") => Some(caps.depth.map_or(1, |depth| depth.min(1))),
        _ if scope.expensive && !top => Some(0),
        _ if scope.expensive => Some(caps.depth.map_or(1, |depth| depth.min(1))),
        Some("locals" | "arguments" | "returnValue") => caps.depth,
        _ if hinted_frame && !top => Some(0),
        _ => caps.depth,
    }
}

/// The scopes of a frame, each with its variables as deep as [`scope_depth`] says.
async fn capture_frame(
    requester: Requester,
    frame_id: usize,
    top: bool,
    caps: Caps,
    counters: Arc<Counters>,
) -> Option<Vec<VarNode>> {
    counters.requests.fetch_add(1, Ordering::Relaxed);
    let scopes = requester.scopes(frame_id).await.ok()?;
    let hinted_frame = scopes.iter().any(|scope| {
        matches!(
            scope.presentation_hint.as_deref(),
            Some("locals" | "arguments")
        )
    });
    let nodes = join_all(scopes.into_iter().map(|scope| {
        let requester = requester.clone();
        let counters = counters.clone();
        async move {
            let depth = scope_depth(&scope, top, hinted_frame, caps);
            let mut node = VarNode {
                name: scope.name,
                reference: scope.variables_reference,
                expensive: scope.expensive,
                ..Default::default()
            };
            if node.reference > 0 && depth == Some(0) {
                counters.truncated.store(true, Ordering::Relaxed);
            } else if node.reference > 0 {
                let total = match (scope.indexed_variables, scope.named_variables) {
                    (None, None) => None,
                    (indexed, named) => Some(indexed.unwrap_or(0) + named.unwrap_or(0)),
                };
                node.children =
                    Some(children(requester, node.reference, total, depth, caps, counters).await);
            }
            node
        }
    }))
    .await;
    Some(nodes)
}

/// The children of `reference`, `levels` deep (`None`: unlimited), at most the collection cap
/// of them, with a last row saying how many more there are.
fn children(
    requester: Requester,
    reference: usize,
    total: Option<usize>,
    levels: Option<usize>,
    caps: Caps,
    counters: Arc<Counters>,
) -> BoxFuture<'static, Vec<VarNode>> {
    async move {
        counters.requests.fetch_add(1, Ordering::Relaxed);
        let mut result = requester
            .variables_page(reference, caps.collection.map(|_| 0), caps.collection)
            .await;
        // an adapter that answers a page with nothing although the variable has children doesn't
        // page: ask for all of them
        if caps.collection.is_some()
            && total.is_some_and(|total| total > 0)
            && result.as_ref().is_ok_and(Vec::is_empty)
        {
            counters.requests.fetch_add(1, Ordering::Relaxed);
            result = requester.variables_page(reference, None, None).await;
        }
        let variables = match result {
            Ok(variables) => variables,
            Err(err) => {
                return vec![VarNode {
                    name: "error".into(),
                    value: err.to_string(),
                    error: true,
                    ..Default::default()
                }]
            }
        };
        let returned = variables.len();
        let shown = caps.collection.map_or(returned, |cap| returned.min(cap));
        let below = levels.map(|levels| levels.saturating_sub(1));
        let mut nodes = join_all(variables.into_iter().take(shown).map(|variable| {
            let requester = requester.clone();
            let counters = counters.clone();
            async move {
                let mut node = VarNode {
                    name: variable.name,
                    value: variable.value,
                    ty: variable.ty,
                    reference: variable.variables_reference,
                    evaluate_name: variable.evaluate_name,
                    memory_reference: variable.memory_reference,
                    ..Default::default()
                };
                if node.reference > 0 && below != Some(0) {
                    let total = match (variable.indexed_variables, variable.named_variables) {
                        (None, None) => None,
                        (indexed, named) => Some(indexed.unwrap_or(0) + named.unwrap_or(0)),
                    };
                    node.children = Some(
                        children(requester, node.reference, total, below, caps, counters).await,
                    );
                } else if node.reference > 0 {
                    counters.truncated.store(true, Ordering::Relaxed);
                }
                node
            }
        }))
        .await;
        let all = total.unwrap_or(returned).max(returned);
        if all > shown {
            counters.truncated.store(true, Ordering::Relaxed);
            nodes.push(VarNode {
                name: "…".into(),
                value: format!("{} more of {all} not recorded", all - shown),
                ..Default::default()
            });
        }
        nodes
    }
    .boxed()
}
