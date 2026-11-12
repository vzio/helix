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
        history::{Caps, Snapshot},
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
    let step = editor.debug.spectator.history.next_step();
    let thread_name = editor.debug.thread_name(key);
    let reason = reason.to_owned();
    let (done, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let counters = Arc::new(Counters::default());
        let scopes = join_all(
            frames
                .iter()
                .map(|frame| capture_frame(requester.clone(), frame.id, caps, counters.clone())),
        )
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
        let mut snapshot = Snapshot {
            step,
            key,
            thread_name,
            reason,
            frames,
            scopes,
            watches: watch_values,
            requests: counters.requests.load(Ordering::Relaxed),
            bytes: 0,
            truncated: counters.truncated.load(Ordering::Relaxed),
        };
        snapshot.measure();
        job::dispatch(move |editor, _| keep(editor, snapshot)).await;
        let _ = done.send(());
    });
    Some(receiver)
}

/// Keeps a snapshot, its changes marked against the stop before it in the same function.
fn keep(editor: &mut Editor, mut snapshot: Snapshot) {
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

/// The scopes of a frame, each with its variables down to the depth cap. An expensive scope
/// (registers, globals, as the adapter marks them) gets only its own variables.
async fn capture_frame(
    requester: Requester,
    frame_id: usize,
    caps: Caps,
    counters: Arc<Counters>,
) -> Option<Vec<VarNode>> {
    counters.requests.fetch_add(1, Ordering::Relaxed);
    let scopes = requester.scopes(frame_id).await.ok()?;
    let nodes = join_all(scopes.into_iter().map(|scope| {
        let requester = requester.clone();
        let counters = counters.clone();
        async move {
            let depth = if scope.expensive {
                caps.depth.map(|depth| depth.min(1)).or(Some(1))
            } else {
                caps.depth
            };
            let mut node = VarNode {
                name: scope.name,
                reference: scope.variables_reference,
                expensive: scope.expensive,
                ..Default::default()
            };
            if node.reference > 0 && depth != Some(0) {
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
        let variables = match requester
            .variables_page(reference, None, caps.collection)
            .await
        {
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
