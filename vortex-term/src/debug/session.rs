//! Debug sessions: starting one, the adapter's events and reverse requests, and ending it.
//!
//! Nothing here waits on the adapter: requests whose answers matter run in background tasks
//! and come back through the job queue. A session may have children (the DAP
//! `startDebugging` request, which js-debug uses for the program it runs); the panel follows
//! the session that last stopped, and the session ends when its root ends.

use std::net::SocketAddr;

use serde_json::{json, Value};
use vortex_core::syntax::config::DebugAdapterConfig;
use vortex_dap::{
    self as dap, events, registry::DebugAdapterId, requests::DisconnectArguments, ConnectionType,
    Event, Payload, Request, ThreadId,
};
use vortex_view::{
    debug::{LineKind, Status},
    editor::SessionBreakpoint,
    Editor,
};

use super::{breakpoints, load, panel};
use crate::job;

/// Starts a debug session with `config`'s adapter (or the one listening on `socket`), and sends
/// it the `request` ("launch" or "attach") with `args`. The panel opens.
pub fn start(
    editor: &mut Editor,
    socket: Option<SocketAddr>,
    config: &DebugAdapterConfig,
    name: &str,
    request: &str,
    args: Value,
) -> anyhow::Result<()> {
    if editor.debug_adapters.iter().next().is_some() {
        anyhow::bail!("A debug session is already running: Space G t ends it");
    }
    if !matches!(request, "launch" | "attach") {
        anyhow::bail!("Unsupported request '{request}'");
    }
    let id = editor
        .debug_adapters
        .start_client(socket, config)
        .map_err(|err| anyhow::anyhow!("Failed to start the debug adapter: {err}"))?;
    let client = editor
        .debug_adapters
        .get_client_mut(id)
        .expect("the client was just started");
    client.name = name.to_owned();
    let requester = client.requester();
    client.set_start(
        if request == "launch" {
            ConnectionType::Launch
        } else {
            ConnectionType::Attach
        },
        args.clone(),
    );

    editor.debug_adapters.set_active_client(id);
    editor.debug.start_session(name.to_owned());
    panel::show(editor);

    let request = request.to_owned();
    tokio::spawn(async move {
        let result = if request == "launch" {
            requester.launch(args).await
        } else {
            requester.attach(args).await
        };
        if let Err(err) = result {
            job::dispatch(move |editor, _| {
                if editor.debug_adapters.get_client(id).is_some() {
                    editor
                        .debug
                        .console
                        .line(LineKind::Error, format!("{request} failed: {err}"));
                    editor.set_error(format!("Debug {request} failed: {err}"));
                    end(editor, id);
                }
            })
            .await;
        }
    });
    Ok(())
}

/// Handles a message of the adapter of session `id`. Returns whether to redraw.
pub async fn handle_message(editor: &mut Editor, id: DebugAdapterId, payload: Payload) -> bool {
    match payload {
        Payload::Event(event) => match Event::parse(&event.event, event.body) {
            Ok(event) => handle_event(editor, id, event),
            Err(dap::Error::Unhandled) => {
                log::debug!("Discarding unknown DAP event '{}'", event.event);
                false
            }
            Err(err) => {
                log::warn!("Discarding invalid DAP event '{}': {err}", event.event);
                false
            }
        },
        Payload::Request(request) => {
            handle_request(editor, id, request.seq, request.command, request.arguments);
            true
        }
        Payload::Response(_) => false,
    }
}

fn handle_event(editor: &mut Editor, id: DebugAdapterId, event: Event) -> bool {
    if editor.debug_adapters.get_client(id).is_none() {
        return false;
    }
    match event {
        Event::Initialized(_) => configure(editor, id),
        Event::Stopped(events::StoppedBody {
            thread_id,
            description,
            text,
            reason,
            all_threads_stopped,
            ..
        }) => {
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            let all = all_threads_stopped.unwrap_or_default();
            if let Some(thread) = thread_id {
                client.thread_states.insert(thread, reason.clone());
            }
            if all {
                for state in client.thread_states.values_mut() {
                    *state = reason.clone();
                }
            }
            let session_name = client.name.clone();
            editor.debug_adapters.set_active_client(id);
            let description = description.or(text).filter(|d| !d.is_empty());
            editor.debug.status = Status::Stopped {
                reason,
                description,
            };
            editor.debug.session_name = session_name;
            editor.debug.generation += 1;
            load::after_stop(editor, id, thread_id);
        }
        Event::Continued(events::ContinuedBody {
            thread_id,
            all_threads_continued,
        }) => {
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            if all_threads_continued.unwrap_or_default() {
                for state in client.thread_states.values_mut() {
                    *state = "running".to_owned();
                }
            }
            client.thread_states.insert(thread_id, "running".to_owned());
            let current =
                client.thread_id == Some(thread_id) || all_threads_continued.unwrap_or_default();
            if current {
                client.resume_application();
                if editor.debug_adapters.get_active_client().map(|c| c.id()) == Some(id) {
                    editor.debug.resumed();
                }
            }
        }
        Event::Exited(events::ExitedBody { exit_code }) => {
            editor.debug.exit_code = Some(exit_code);
            editor
                .debug
                .console
                .line(LineKind::Info, format!("Exited with code {exit_code}"));
        }
        Event::Terminated(body) => terminated(editor, id, body.and_then(|body| body.restart)),
        Event::Thread(events::ThreadBody { reason, thread_id }) => {
            thread_event(editor, id, &reason, thread_id)
        }
        Event::Output(events::OutputBody {
            category, output, ..
        }) => {
            let kind = match category.as_deref() {
                Some("telemetry") => return false,
                Some("stdout") => LineKind::Stdout,
                Some("stderr") => LineKind::Stderr,
                _ => LineKind::Info,
            };
            editor.debug.console.push(kind, &output);
        }
        Event::Breakpoint(events::BreakpointBody { reason, breakpoint }) => {
            breakpoints::event(editor, id, &reason, breakpoint)
        }
        Event::ProgressStart(body) => {
            let status = editor
                .debug_adapters
                .get_client_mut(id)
                .unwrap()
                .progress_start(body);
            editor.set_status(status);
        }
        Event::ProgressUpdate(body) => {
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            if let Some(status) = client.progress_update(body) {
                editor.set_status(status);
            }
        }
        Event::ProgressEnd(body) => {
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            if let Some(status) = client.progress_end(body) {
                editor.set_status(status);
            }
        }
        Event::Capabilities(events::CapabilitiesBody { capabilities }) => {
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            client.caps = Some(capabilities);
            return false;
        }
        Event::Process(_) | Event::Module(_) | Event::LoadedSource(_) | Event::Memory(_) => {
            return false;
        }
    }
    true
}

/// The adapter is ready for its configuration: the breakpoints, the exception filters, then
/// `configurationDone`. Sent once per session (js-debug announces `initialized` twice on a child
/// session, and configuring again would replace its breakpoints).
fn configure(editor: &mut Editor, id: DebugAdapterId) {
    let client = editor.debug_adapters.get_client_mut(id).unwrap();
    if client.configured {
        return;
    }
    client.configured = true;
    let requester = client.requester();
    let filters = client.caps.as_ref().and_then(|caps| {
        let available = caps.exception_breakpoint_filters.as_ref()?;
        Some(match &editor.debug.exception_filters {
            Some(chosen) => chosen.clone(),
            None => available
                .iter()
                .filter(|filter| filter.default.unwrap_or_default())
                .map(|filter| filter.filter.clone())
                .collect(),
        })
    });
    let files = breakpoints::to_send(editor);

    editor.debug_adapters.set_active_client(id);
    if editor.debug.status == Status::Starting {
        editor.debug.status = Status::Running;
    }

    tokio::spawn(async move {
        let mut results = Vec::new();
        for (path, sent) in files {
            let breakpoints = sent.iter().map(|(_, b)| b.clone()).collect();
            let result = requester.set_breakpoints(path.clone(), breakpoints).await;
            results.push((path, sent, result));
        }
        if let Some(filters) = filters {
            if let Err(err) = requester.set_exception_breakpoints(filters).await {
                log::warn!("setExceptionBreakpoints failed: {err}");
            }
        }
        let done = requester.configuration_done().await;
        job::dispatch(move |editor, _| {
            for (path, sent, result) in results {
                breakpoints::apply(editor, id, &path, &sent, result);
            }
            if let Err(err) = done {
                editor.set_error(format!("Debugger configuration failed: {err}"));
            }
        })
        .await;
    });
}

fn thread_event(editor: &mut Editor, id: DebugAdapterId, reason: &str, thread: ThreadId) {
    if editor.debug_adapters.get_active_client().map(|c| c.id()) != Some(id) {
        return;
    }
    match reason {
        "exited" => {
            editor.debug.threads.retain(|t| t.id != thread);
            if let Some(client) = editor.debug_adapters.get_client_mut(id) {
                client.thread_states.remove(&thread);
                client.stack_frames.remove(&thread);
            }
        }
        "started" => {
            if !editor.debug.threads.iter().any(|t| t.id == thread) {
                editor.debug.threads.push(dap::Thread {
                    id: thread,
                    name: format!("Thread {thread}"),
                });
            }
        }
        _ => {}
    }
}

/// A session ended. A child's end gives the panel back to its parent; the root's ends it all,
/// unless the adapter asks for a restart.
fn terminated(editor: &mut Editor, id: DebugAdapterId, restart: Option<Value>) {
    let Some(client) = editor.debug_adapters.get_client(id) else {
        return;
    };
    if let Some(parent) = client.parent {
        remove_tree(editor, id);
        if editor.debug_adapters.get_active_client().is_none() {
            // the panel followed the child: back to its parent, which runs
            editor.debug_adapters.set_active_client(parent);
            editor.debug.resumed();
            editor.debug.threads.clear();
            editor.debug.scopes.clear();
        }
        return;
    }

    match restart.filter(|restart| *restart != Value::Bool(false)) {
        Some(args) => {
            // the adapter asked to start again with these arguments (`true`: the same ones)
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            let args = if args.is_object() {
                args
            } else {
                client
                    .starting_request_args()
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            let requester = client.requester();
            let launch = client.connection_type() != Some(ConnectionType::Attach);
            editor
                .debug
                .start_session(editor.debug.session_name.clone());
            tokio::spawn(async move {
                let result = if launch {
                    requester.launch(args).await
                } else {
                    requester.attach(args).await
                };
                if let Err(err) = result {
                    job::dispatch(move |editor, _| {
                        editor.set_error(format!("Failed to restart the debug session: {err}"));
                        end(editor, id);
                    })
                    .await;
                }
            });
        }
        None => end(editor, id),
    }
}

/// Ends the session `id` belongs to: disconnects and removes it with all its children, hides the
/// panel, and says how the program ended.
pub fn end(editor: &mut Editor, id: DebugAdapterId) {
    let root = editor.debug_adapters.root(id);
    remove_tree(editor, root);
    // anything left (a session started beside the root) goes too
    let rest: Vec<_> = editor.debug_adapters.iter().map(|(id, _)| id).collect();
    for id in rest {
        remove_tree(editor, id);
    }
    editor.debug_adapters.unset_active_client();
    for list in editor.breakpoints.values_mut() {
        for breakpoint in list {
            breakpoint.sessions.clear();
            breakpoint.refresh_verified();
        }
    }
    editor.debug.end_session();
    editor.set_status(match editor.debug.exit_code {
        Some(code) => format!("Debug session ended: exited with code {code}"),
        None => "Debug session ended".to_owned(),
    });
}

/// Disconnects session `id` and its children, and forgets them.
fn remove_tree(editor: &mut Editor, id: DebugAdapterId) {
    for child in editor.debug_adapters.children(id) {
        remove_tree(editor, child);
    }
    if let Some(client) = editor.debug_adapters.get_client(id) {
        let requester = client.requester();
        tokio::spawn(async move {
            let args = DisconnectArguments {
                restart: Some(false),
                terminate_debuggee: None,
                suspend_debuggee: None,
            };
            if let Err(err) = requester.disconnect(Some(args)).await {
                log::debug!("disconnect: {err}");
            }
        });
    }
    editor.debug_adapters.remove_client(id);
    for list in editor.breakpoints.values_mut() {
        for breakpoint in list {
            breakpoint
                .sessions
                .retain(|session: &SessionBreakpoint| session.session != id);
            breakpoint.refresh_verified();
        }
    }
}

fn handle_request(
    editor: &mut Editor,
    id: DebugAdapterId,
    seq: u64,
    command: String,
    arguments: Option<Value>,
) {
    let reply = match Request::parse(&command, arguments) {
        Ok(Request::RunInTerminal(arguments)) => run_in_terminal(editor, arguments),
        Ok(Request::StartDebugging(arguments)) => start_child(editor, id, arguments),
        Err(err) => Err(err),
    };
    if let Some(client) = editor.debug_adapters.get_client(id) {
        let reply = client.reply(seq, &command, reply);
        tokio::spawn(async move {
            if let Err(err) = reply.await {
                log::debug!("reply to {command}: {err}");
            }
        });
    }
}

fn run_in_terminal(
    editor: &mut Editor,
    arguments: dap::requests::RunInTerminalArguments,
) -> dap::Result<Value> {
    let config = editor.config();
    let Some(config) = config.terminal.external() else {
        editor.set_error("No external terminal defined ([editor.terminal] command)");
        return Err(dap::Error::Other(anyhow::anyhow!(
            "no external terminal defined"
        )));
    };
    let process = std::process::Command::new(&config.command)
        .args(&config.args)
        .args(&arguments.args)
        .spawn()
        .map_err(|err| {
            editor.set_error(format!("Error starting external terminal: {err}"));
            dap::Error::Other(err.into())
        })?;
    Ok(json!(dap::requests::RunInTerminalResponse {
        process_id: Some(process.id()),
        shell_process_id: None,
    }))
}

/// `startDebugging`: the adapter wants a new session for `configuration`. The child is started
/// and initialized now; its launch (or attach) goes in the background, because adapters answer
/// it only after the child's configuration, which comes with its `initialized` event.
fn start_child(
    editor: &mut Editor,
    parent: DebugAdapterId,
    arguments: dap::requests::StartDebuggingArguments,
) -> dap::Result<Value> {
    let child = match editor
        .debug_adapters
        .start_child(parent, &arguments.configuration)
    {
        Ok(child) => child,
        Err(err) => {
            editor.set_error(format!("Failed to start a child debug session: {err}"));
            return Err(err);
        }
    };
    let client = editor.debug_adapters.get_client_mut(child).unwrap();
    let requester = client.requester();
    let launch = matches!(arguments.request, ConnectionType::Launch);
    let configuration = arguments.configuration.clone();
    client.set_start(arguments.request, configuration.clone());
    tokio::spawn(async move {
        let result = if launch {
            requester.launch(configuration).await
        } else {
            requester.attach(configuration).await
        };
        if let Err(err) = result {
            job::dispatch(move |editor, _| {
                if editor.debug_adapters.get_client(child).is_some() {
                    editor.set_error(format!("A child debug session failed to start: {err}"));
                    remove_tree(editor, child);
                }
            })
            .await;
        }
    });
    Ok(json!({}))
}
