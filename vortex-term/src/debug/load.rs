//! Loading what the panel shows from the adapter, in background tasks: the threads, frames,
//! scopes and variables at a stop, the children of a node the user opens, the watches, and the
//! console's evaluations. A load started before the last stop or resume is dropped.

use vortex_dap::{registry::DebugAdapterId, requests::EvaluateResponse, Requester, ThreadId};
use vortex_view::{
    debug::{spectator::ThreadKey, LineKind, Tree, VarNode, VarPath},
    handlers::dap::jump_to_stack_frame,
    Editor,
};

use super::{playback, rewind};
use crate::job;

/// At most this many `variables` requests per reload, so a deep expanded tree can't flood the
/// adapter.
const MAX_LOADS: usize = 200;

fn variable_node(variable: vortex_dap::Variable) -> VarNode {
    VarNode {
        name: variable.name,
        value: variable.value,
        ty: variable.ty,
        reference: variable.variables_reference,
        evaluate_name: variable.evaluate_name,
        ..Default::default()
    }
}

fn evaluation_node(expression: &str, result: vortex_dap::Result<EvaluateResponse>) -> VarNode {
    match result {
        Ok(response) => VarNode {
            name: expression.to_owned(),
            value: response.result,
            ty: response.ty,
            reference: response.variables_reference,
            evaluate_name: Some(expression.to_owned()),
            ..Default::default()
        },
        Err(err) => VarNode {
            name: expression.to_owned(),
            value: err.to_string(),
            error: true,
            ..Default::default()
        },
    }
}

/// The node at the end of `names`, from `nodes` down.
fn find_mut<'a>(nodes: &'a mut [VarNode], names: &[String]) -> Option<&'a mut VarNode> {
    let (first, rest) = names.split_first()?;
    let node = nodes.iter_mut().find(|node| &node.name == first)?;
    if rest.is_empty() {
        Some(node)
    } else {
        find_mut(node.children.as_deref_mut()?, rest)
    }
}

/// Loads the children of every expanded path, shortest first, in the trees given.
async fn load_expanded(
    requester: &Requester,
    scopes: &mut [VarNode],
    watches: &mut [Option<VarNode>],
    expanded: &[VarPath],
) {
    let mut loads = 0;
    for path in expanded {
        if loads >= MAX_LOADS {
            break;
        }
        let Some(node) = (match path.tree {
            Tree::Scopes => find_mut(scopes, &path.names),
            Tree::Watch(index) => watches
                .get_mut(index)
                .and_then(Option::as_mut)
                .and_then(|node| find_mut(std::slice::from_mut(node), &path.names)),
        }) else {
            continue;
        };
        if node.reference == 0 || node.children.is_some() {
            continue;
        }
        loads += 1;
        node.children = Some(match requester.variables(node.reference).await {
            Ok(variables) => variables.into_iter().map(variable_node).collect(),
            Err(err) => vec![VarNode {
                name: "error".into(),
                value: err.to_string(),
                error: true,
                ..Default::default()
            }],
        });
    }
}

/// The scopes and watches of `frame_id`, with every expanded node loaded.
async fn load_frame(
    requester: Requester,
    frame_id: usize,
    expanded: Vec<VarPath>,
    collapsed: std::collections::HashSet<VarPath>,
    watches: Vec<String>,
) -> (Vec<VarNode>, Vec<Option<VarNode>>) {
    let mut scopes: Vec<VarNode> = match requester.scopes(frame_id).await {
        Ok(scopes) => scopes
            .into_iter()
            .map(|scope| VarNode {
                name: scope.name,
                reference: scope.variables_reference,
                expensive: scope.expensive,
                ..Default::default()
            })
            .collect(),
        Err(err) => vec![VarNode {
            name: "scopes".into(),
            value: err.to_string(),
            error: true,
            ..Default::default()
        }],
    };
    let mut values = Vec::with_capacity(watches.len());
    for expression in &watches {
        let result = requester
            .evaluate(expression.clone(), Some(frame_id), Some("watch"))
            .await;
        values.push(Some(evaluation_node(expression, result)));
    }
    // the first cheap scope opens by itself, unless the user closed it
    let mut expanded = expanded;
    if let Some(first) = scopes.iter().find(|scope| !scope.expensive && !scope.error) {
        let path = VarPath {
            tree: Tree::Scopes,
            names: vec![first.name.clone()],
        };
        if !collapsed.contains(&path) && !expanded.contains(&path) {
            expanded.insert(0, path);
        }
    }
    load_expanded(&requester, &mut scopes, &mut values, &expanded).await;
    (scopes, values)
}

/// What the frame load needs from the editor: the requester of the session the panel follows
/// and the selected frame.
fn frame_context(editor: &Editor) -> Option<(Requester, usize, String, bool)> {
    let client = editor.debug_adapters.get_active_client()?;
    let thread = client.thread_id?;
    let index = client.active_frame?;
    let frame = client.stack_frames.get(&thread)?.get(index)?;
    Some((client.requester(), frame.id, frame.name.clone(), index == 0))
}

/// The paths a reload loads: the expanded ones, and the way down to every marked variable.
fn paths_to_load(editor: &Editor) -> Vec<VarPath> {
    let mut paths = editor.debug.expanded_paths();
    for names in &editor.debug.spectator.marked {
        for len in 1..names.len() {
            let path = VarPath {
                tree: Tree::Scopes,
                names: names[..len].to_vec(),
            };
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths.sort_by_key(|path| path.names.len());
    paths
}

/// The value of the variable at `names` (a scope's name, then the variables down to it) in
/// frame `frame_id`, loading the way down.
pub async fn fetch_path(
    requester: &Requester,
    frame_id: usize,
    names: &[String],
) -> Option<String> {
    let (scope_name, rest) = names.split_first()?;
    let scopes = requester.scopes(frame_id).await.ok()?;
    let mut reference = scopes
        .into_iter()
        .find(|scope| &scope.name == scope_name)?
        .variables_reference;
    let mut value = None;
    for name in rest {
        if reference == 0 {
            return None;
        }
        let variable = requester
            .variables(reference)
            .await
            .ok()?
            .into_iter()
            .find(|variable| &variable.name == name)?;
        reference = variable.variables_reference;
        value = Some(variable.value);
    }
    value
}

/// Reloads the variables and watches of the selected frame (after a frame switch, an edit, or a
/// new watch).
pub fn reload_frame(editor: &mut Editor) {
    let Some((requester, frame_id, function, top)) = frame_context(editor) else {
        // nothing is stopped: the watches wait for the next stop
        return;
    };
    let generation = editor.debug.generation;
    let expanded = paths_to_load(editor);
    let collapsed = editor.debug.collapsed.clone();
    let watches = editor
        .debug
        .watches
        .iter()
        .map(|w| w.expression.clone())
        .collect();
    tokio::spawn(async move {
        let (scopes, watches) = load_frame(requester, frame_id, expanded, collapsed, watches).await;
        job::dispatch(move |editor, _| {
            if editor.debug.generation == generation {
                editor
                    .debug
                    .apply_values(top.then_some(function.as_str()), scopes, watches);
            }
        })
        .await;
    });
}

/// A session stopped: loads its threads and the stack of the thread that stopped; playback
/// decides what to make of the stop, and unless it is a stop on the way the editor jumps to the
/// top frame and its variables and the watches load.
pub fn after_stop(
    editor: &mut Editor,
    id: DebugAdapterId,
    thread: Option<ThreadId>,
    reason: String,
) {
    let Some(client) = editor.debug_adapters.get_client(id) else {
        return;
    };
    let requester = client.requester();
    let known = client.thread_id;
    let generation = editor.debug.generation;
    tokio::spawn(async move {
        let threads = requester.threads().await.unwrap_or_default();
        let Some(thread) = thread
            .or(known)
            .or_else(|| threads.first().map(|thread| thread.id))
        else {
            return;
        };
        let frames = requester
            .stack_trace(thread)
            .await
            .map(|(frames, _)| frames)
            .unwrap_or_default();
        job::dispatch(move |editor, _| {
            if editor.debug.generation != generation {
                return;
            }
            let Some(client) = editor.debug_adapters.get_client_mut(id) else {
                return;
            };
            client.stack_frames.insert(thread, frames.clone());
            client.thread_id = Some(thread);
            client.active_frame = Some(0);
            if !threads.is_empty() {
                editor.debug.set_threads(id, threads);
            }
            match playback::on_stopped(editor, id, thread, &reason, &frames) {
                playback::Show::Quiet => {}
                playback::Show::Full { follow } => {
                    if editor.debug.is_rewinding() {
                        // the rewind stays where it is; the live values load behind it
                    } else if let Some(frame) = frames.first() {
                        playback::show_frame(editor, frame, follow);
                    }
                    reload_frame(editor);
                }
            }
        })
        .await;
    });
}

/// Selects frame `index` of the thread `key` (making it the current thread, and its session
/// the one the panel follows): jumps to its code and shows its variables. Loads the thread's
/// stack first when it isn't loaded. In rewind, selects a frame of the recorded stop.
pub fn select_frame(editor: &mut Editor, key: ThreadKey, index: usize) {
    if editor.debug.is_rewinding() {
        rewind::select_frame(editor, index);
        return;
    }
    if editor.debug_adapters.get_client(key.session).is_none() {
        return;
    }
    editor.debug_adapters.set_active_client(key.session);
    let thread = key.thread;
    let client = editor.debug_adapters.get_client_mut(key.session).unwrap();
    if let Some(frame) = client
        .stack_frames
        .get(&thread)
        .and_then(|frames| frames.get(index))
        .cloned()
    {
        client.thread_id = Some(thread);
        client.active_frame = Some(index);
        editor.debug.session_name = client.name.clone();
        editor.debug.generation += 1;
        jump_to_stack_frame(editor, &frame);
        reload_frame(editor);
        return;
    }
    // another thread, not loaded yet
    let id = client.id();
    let requester = client.requester();
    editor.debug.generation += 1;
    let generation = editor.debug.generation;
    tokio::spawn(async move {
        let frames = match requester.stack_trace(thread).await {
            Ok((frames, _)) => frames,
            Err(err) => {
                job::dispatch(move |editor, _| {
                    editor.set_error(format!("Couldn't load the stack of thread {thread}: {err}"))
                })
                .await;
                return;
            }
        };
        job::dispatch(move |editor, _| {
            if editor.debug.generation != generation {
                return;
            }
            let Some(client) = editor.debug_adapters.get_client_mut(id) else {
                return;
            };
            let has_frame = frames.len() > index;
            client.stack_frames.insert(thread, frames);
            if has_frame {
                select_frame(editor, key, index);
            }
        })
        .await;
    });
}

/// Loads the frames of the thread `key` to show them in the call stack, without selecting it.
pub fn load_thread(editor: &mut Editor, key: ThreadKey) {
    let Some(client) = editor.debug_adapters.get_client(key.session) else {
        return;
    };
    let id = client.id();
    let requester = client.requester();
    let thread = key.thread;
    tokio::spawn(async move {
        let Ok((frames, _)) = requester.stack_trace(thread).await else {
            return;
        };
        job::dispatch(move |editor, _| {
            if let Some(client) = editor.debug_adapters.get_client_mut(id) {
                client.stack_frames.insert(thread, frames);
            }
        })
        .await;
    });
}

/// Loads the children of the node at `path`, which the user just opened.
pub fn expand(editor: &mut Editor, path: VarPath) {
    // in rewind the tree is the recorded one: what wasn't recorded can't be fetched any more
    if editor.debug.is_rewinding() {
        return;
    }
    let Some(node) = editor.debug.node(&path) else {
        return;
    };
    if node.reference == 0 || node.children.is_some() {
        return;
    }
    let reference = node.reference;
    let Some(client) = editor.debug_adapters.get_active_client() else {
        return;
    };
    let requester = client.requester();
    let generation = editor.debug.generation;
    tokio::spawn(async move {
        let children = match requester.variables(reference).await {
            Ok(variables) => variables.into_iter().map(variable_node).collect(),
            Err(err) => vec![VarNode {
                name: "error".into(),
                value: err.to_string(),
                error: true,
                ..Default::default()
            }],
        };
        job::dispatch(move |editor, _| {
            if editor.debug.generation == generation {
                editor.debug.set_children(&path, children);
            }
        })
        .await;
    });
}

/// Evaluates `expression` in the selected frame (DAP `evaluate`, context "repl") and writes it
/// and its value in the console. The values shown are reloaded after, as the expression may
/// have changed them.
pub fn evaluate(editor: &mut Editor, expression: String) {
    if editor.debug.is_rewinding() {
        editor.set_error(rewind::READ_ONLY);
        return;
    }
    let Some(client) = editor.debug_adapters.get_active_client() else {
        editor
            .debug
            .console
            .line(LineKind::Error, "No debug session to evaluate in");
        return;
    };
    let requester = client.requester();
    let frame_id = frame_context(editor).map(|(_, frame_id, _, _)| frame_id);
    editor
        .debug
        .console
        .line(LineKind::Input, expression.clone());
    tokio::spawn(async move {
        let result = requester.evaluate(expression, frame_id, Some("repl")).await;
        job::dispatch(move |editor, _| {
            match result {
                Ok(response) => {
                    let text = match response.ty.as_deref().filter(|ty| !ty.is_empty()) {
                        Some(ty) => format!("{}  ({ty})", response.result),
                        None => response.result,
                    };
                    editor.debug.console.line(LineKind::Result, text);
                }
                Err(err) => editor.debug.console.line(LineKind::Error, err.to_string()),
            }
            reload_frame(editor);
        })
        .await;
    });
}

/// Sets the value of the variable or watch at `path` (DAP `setVariable`, or `setExpression`
/// for a watch or when the adapter only has that), then reloads the values.
pub fn set_value(editor: &mut Editor, path: VarPath, value: String) -> anyhow::Result<()> {
    if editor.debug.is_rewinding() {
        anyhow::bail!(rewind::READ_ONLY);
    }
    let client = editor
        .debug_adapters
        .get_active_client()
        .ok_or_else(|| anyhow::anyhow!("No debug session"))?;
    let caps = client.caps.clone();
    let requester = client.requester();
    let frame_id = frame_context(editor).map(|(_, frame_id, _, _)| frame_id);
    let node = editor
        .debug
        .node(&path)
        .ok_or_else(|| anyhow::anyhow!("The value is gone"))?;
    let container = editor.debug.container_reference(&path);
    let set_variable = caps
        .as_ref()
        .and_then(|caps| caps.supports_set_variable)
        .unwrap_or_default();
    let set_expression = caps
        .as_ref()
        .and_then(|caps| caps.supports_set_expression)
        .unwrap_or_default();
    let name = node.name.clone();
    let expression = node.evaluate_name.clone();

    enum How {
        Variable(usize, String),
        Expression(String),
    }
    let how = match (container, expression) {
        (Some(container), _) if set_variable => How::Variable(container, name),
        (_, Some(expression)) if set_expression => How::Expression(expression),
        (None, _) if path.names.len() == 1 && path.tree != Tree::Scopes && set_expression => {
            How::Expression(name)
        }
        _ if !set_variable && !set_expression => {
            anyhow::bail!("The debug adapter can't change values")
        }
        _ => anyhow::bail!("This value can't be changed"),
    };
    tokio::spawn(async move {
        let result = match how {
            How::Variable(container, name) => requester
                .set_variable(container, name, value)
                .await
                .map(|_| ()),
            How::Expression(expression) => requester
                .set_expression(expression, value, frame_id)
                .await
                .map(|_| ()),
        };
        job::dispatch(move |editor, _| match result {
            Ok(()) => reload_frame(editor),
            Err(err) => {
                editor.set_error(format!("Couldn't change the value: {err}"));
                // it may have changed anyway
                reload_frame(editor);
            }
        })
        .await;
    });
    Ok(())
}
