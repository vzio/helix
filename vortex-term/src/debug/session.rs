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
    Event, Payload, Request,
};
use vortex_view::{
    debug::{LineKind, Status},
    editor::SessionBreakpoint,
    Editor,
};

use super::{breakpoints, load, panel, playback, threads};
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
        anyhow::bail!("A debug session is already running");
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
    // debug mode follows the new session
    super::mode::session_started(editor);
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
                    end(editor, id);
                    editor.set_error(format!("Debug {request} failed: {err}"));
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
            Ok(parsed) => handle_event(editor, id, parsed, event.taken),
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

fn handle_event(
    editor: &mut Editor,
    id: DebugAdapterId,
    event: Event,
    taken: Option<vortex_dap::hits::Taken>,
) -> bool {
    // output still counts after its session ended (a child's last words may come after its
    // parent's `terminated`)
    if let Event::Output(events::OutputBody {
        category, output, ..
    }) = &event
    {
        let kind = match category.as_deref() {
            Some("telemetry") => return false,
            Some("stdout") => LineKind::Stdout,
            Some("stderr") => LineKind::Stderr,
            _ => LineKind::Info,
        };
        editor.debug.console.push(kind, output);
        return true;
    }
    if editor.debug_adapters.get_client(id).is_none() {
        return false;
    }
    let redraw = handle_session_event(editor, id, event, taken);
    super::engine::sync_watch(editor);
    redraw
}

fn handle_session_event(
    editor: &mut Editor,
    id: DebugAdapterId,
    event: Event,
    taken: Option<vortex_dap::hits::Taken>,
) -> bool {
    match event {
        Event::Initialized(_) => configure(editor, id),
        Event::Stopped(body) => {
            // the engine takes the hits of its hidden breakpoints first
            let reason = match super::engine::on_stopped(editor, id, &body, taken) {
                super::engine::Outcome::Done(redraw) => return redraw,
                super::engine::Outcome::Proceed(reason) => reason,
            };
            return handle_stop(editor, id, events::StoppedBody { reason, ..body });
        }
        event => return handle_other_event(editor, id, event),
    }
    true
}

fn handle_stop(editor: &mut Editor, id: DebugAdapterId, body: events::StoppedBody) -> bool {
    let events::StoppedBody {
        thread_id,
        description,
        text,
        reason,
        all_threads_stopped,
        preserve_focus_hint,
        ..
    } = body;
    // An adapter that stops every thread may report each of them, hinting that only one
    // caused the stop (lldb-dap: the others keep the reason of their last stop). Those
    // only update their thread's state, unless no other stop comes soon after.
    if preserve_focus_hint == Some(true) {
        let client = editor.debug_adapters.get_client_mut(id).unwrap();
        if let Some(thread) = thread_id {
            client.thread_states.insert(thread, reason.clone());
        }
        let generation = editor.debug.generation;
        let body = events::StoppedBody {
            reason,
            description,
            thread_id,
            preserve_focus_hint: Some(false),
            text,
            all_threads_stopped,
            hit_breakpoint_ids: None,
        };
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            job::dispatch(move |editor, _| {
                if editor.debug.generation == generation
                    && editor.debug_adapters.get_client(id).is_some()
                {
                    handle_event(editor, id, Event::Stopped(body), None);
                }
            })
            .await;
        });
        return true;
    }
    let reason_for_playback = reason.clone();
    editor.debug.spectator.stop_arrived = Some(
        editor
            .debug_adapters
            .get_client(id)
            .and_then(|client| client.last_stop_received())
            .unwrap_or_else(std::time::Instant::now),
    );
    match all_threads_stopped {
        Some(false) => {
            editor.debug.spectator.non_stop.insert(id);
        }
        Some(true) => {
            editor.debug.spectator.non_stop.remove(&id);
        }
        None => {}
    }
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
    super::memory::on_stop(editor, id);
    load::after_stop(editor, id, thread_id, reason_for_playback);
    true
}

fn handle_other_event(editor: &mut Editor, id: DebugAdapterId, event: Event) -> bool {
    match event {
        Event::Continued(events::ContinuedBody {
            thread_id,
            all_threads_continued,
        }) => {
            let status = editor.debug.status.clone();
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            let all = all_threads_continued.unwrap_or_default();
            // the hits the engine continues bring one of these each: they change nothing that
            // shows, and redrawing for each would cost more than the hit
            let stopped_before = client
                .thread_states
                .iter()
                .any(|(thread, state)| (all || *thread == thread_id) && state != "running");
            if all {
                for state in client.thread_states.values_mut() {
                    *state = "running".to_owned();
                }
            }
            client.thread_states.insert(thread_id, "running".to_owned());
            let current = client.thread_id == Some(thread_id) || all;
            if current {
                client.resume_application();
                if editor.debug_adapters.get_active_client().map(|c| c.id()) == Some(id) {
                    editor.debug.resumed();
                }
            }
            return stopped_before || editor.debug.status != status;
        }
        Event::Exited(events::ExitedBody { exit_code }) => {
            editor
                .debug
                .engine
                .record
                .mark(vortex_view::debug::lineline::MarkKind::End);
            editor.debug.exit_code = Some(exit_code);
            editor
                .debug
                .console
                .line(LineKind::Info, format!("Exited with code {exit_code}"));
        }
        Event::Terminated(body) => terminated(editor, id, body.and_then(|body| body.restart)),
        Event::Thread(events::ThreadBody { reason, thread_id }) => {
            threads::event(editor, id, &reason, thread_id)
        }
        Event::Output(_) => {}
        Event::Breakpoint(events::BreakpointBody { reason, breakpoint }) => {
            return breakpoints::event(editor, id, &reason, breakpoint);
        }
        Event::ProgressStart(body) => {
            let status = editor
                .debug_adapters
                .get_client_mut(id)
                .unwrap()
                .progress_start(body);
            log::info!("{status}");
        }
        Event::ProgressUpdate(body) => {
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            if let Some(status) = client.progress_update(body) {
                log::info!("{status}");
            }
        }
        Event::ProgressEnd(body) => {
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            if let Some(status) = client.progress_end(body) {
                log::info!("{status}");
            }
        }
        Event::Capabilities(events::CapabilitiesBody { capabilities }) => {
            // only the capabilities that changed (DAP): the others stay, and so do the ones the
            // configuration assumes
            let client = editor.debug_adapters.get_client_mut(id).unwrap();
            let overrides = client.config.as_ref().and_then(|c| c.capabilities.clone());
            client.caps = Some(merge_capabilities(
                client.caps.take(),
                capabilities,
                overrides,
            ));
            return false;
        }
        Event::Process(events::ProcessBody {
            system_process_id,
            is_local_process,
            ..
        }) => {
            // a local debuggee: its CPU time can be read
            if is_local_process != Some(false) {
                editor.debug.spectator.pid = system_process_id.map(|pid| pid as u32);
            }
            return false;
        }
        Event::Memory(_) => super::memory::on_memory_event(editor, id),
        Event::Module(_) | Event::LoadedSource(_) => {
            return false;
        }
        Event::Initialized(_) | Event::Stopped(_) => {}
    }
    true
}

/// `changed` over `caps`, then the `overrides` of the configuration over both.
fn merge_capabilities(
    caps: Option<vortex_dap::DebuggerCapabilities>,
    changed: vortex_dap::DebuggerCapabilities,
    overrides: Option<serde_json::Map<String, Value>>,
) -> vortex_dap::DebuggerCapabilities {
    let object = |caps: &vortex_dap::DebuggerCapabilities| match serde_json::to_value(caps) {
        Ok(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    let mut merged = caps.as_ref().map(object).unwrap_or_default();
    merged.extend(object(&changed).into_iter().filter(|(_, v)| !v.is_null()));
    merged.extend(overrides.unwrap_or_default());
    serde_json::from_value(Value::Object(merged)).unwrap_or(changed)
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
    editor.debug_adapters.set_active_client(id);
    if editor.debug.status == Status::Starting {
        editor.debug.status = Status::Running;
    }
    if super::engine::wanted(editor) {
        // the hidden breakpoints first (with yours in the files they cover), then the rest
        super::engine::install(editor, id, move |editor, covered| {
            send_configuration(editor, id, covered)
        });
    } else {
        send_configuration(editor, id, Default::default());
    }
}

/// The rest of the configuration: your breakpoints in the files the engine didn't cover, the
/// exception filters, then `configurationDone`.
fn send_configuration(
    editor: &mut Editor,
    id: DebugAdapterId,
    covered: std::collections::HashSet<std::path::PathBuf>,
) {
    let Some(client) = editor.debug_adapters.get_client(id) else {
        return;
    };
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
    let files: Vec<_> = breakpoints::to_send(editor)
        .into_iter()
        .filter(|(path, _)| !covered.contains(path))
        .collect();

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
    end_with(editor, id, true);
}

/// [`end`], without disconnecting when that was done already (debug mode's `S` on an attached
/// program).
pub fn end_with(editor: &mut Editor, id: DebugAdapterId, disconnect: bool) {
    playback::session_ended(editor);
    let root = editor.debug_adapters.root(id);
    remove_tree_with(editor, root, disconnect);
    // anything left (a session started beside the root) goes too
    let rest: Vec<_> = editor.debug_adapters.iter().map(|(id, _)| id).collect();
    for id in rest {
        remove_tree_with(editor, id, disconnect);
    }
    editor.debug_adapters.unset_active_client();
    for list in editor.breakpoints.values_mut() {
        for breakpoint in list {
            breakpoint.sessions.clear();
            breakpoint.refresh_verified();
        }
    }
    editor.debug.end_session();
    // an error that ended it (a failed launch) stays shown
    if !editor.is_err() {
        crate::debug::mode::say(
            editor,
            match editor.debug.exit_code {
                Some(code) => format!("Debug session ended: exited with code {code}"),
                None => "Debug session ended".to_owned(),
            },
        );
    }
}

/// Disconnects session `id` and its children (unless `disconnect` is false: it was done
/// already), forgets them, and stops the adapter processes they started once they answered.
fn remove_tree_with(editor: &mut Editor, id: DebugAdapterId, disconnect: bool) {
    for child in editor.debug_adapters.children(id) {
        remove_tree_with(editor, child, disconnect);
    }
    if let Some(client) = editor.debug_adapters.get_client_mut(id) {
        let requester = client.requester();
        let process = client.take_process();
        tokio::spawn(async move {
            if disconnect {
                let args = DisconnectArguments {
                    restart: Some(false),
                    terminate_debuggee: None,
                    suspend_debuggee: None,
                };
                let answer = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    requester.disconnect(Some(args)),
                );
                if let Ok(Err(err)) = answer.await {
                    log::debug!("disconnect: {err}");
                }
            }
            if let Some(mut process) = process {
                // let it exit by itself first
                let exited =
                    tokio::time::timeout(std::time::Duration::from_secs(1), process.wait()).await;
                if exited.is_err() {
                    let _ = process.kill().await;
                }
            }
        });
    }
    editor.debug_adapters.remove_client(id);
    super::memory::session_ended(editor, id);
    editor.debug.threads.retain(|entry| entry.session != id);
    let spectator = &mut editor.debug.spectator;
    spectator.frozen.retain(|key| key.session != id);
    if spectator.spectated.is_some_and(|key| key.session == id) {
        spectator.spectated = None;
    }
    if spectator
        .step
        .as_ref()
        .is_some_and(|step| step.key.session == id)
    {
        spectator.finish_step();
        spectator.pause();
    }
    for list in editor.breakpoints.values_mut() {
        for breakpoint in list {
            breakpoint
                .sessions
                .retain(|session: &SessionBreakpoint| session.session != id);
            breakpoint.refresh_verified();
        }
    }
}

fn remove_tree(editor: &mut Editor, id: DebugAdapterId) {
    remove_tree_with(editor, id, true);
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

#[cfg(test)]
mod test {
    use super::*;

    fn caps(json: Value) -> vortex_dap::DebuggerCapabilities {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_capabilities_event_changes_only_what_it_names() {
        let initial = caps(json!({"supportsReadMemoryRequest": true, "supportsSetVariable": true}));
        let changed = caps(json!({"supportsRestartRequest": true}));
        let merged = merge_capabilities(Some(initial), changed, None);
        assert_eq!(merged.supports_read_memory_request, Some(true));
        assert_eq!(merged.supports_set_variable, Some(true));
        assert_eq!(merged.supports_restart_request, Some(true));

        // the configuration's assumptions still win
        let overrides = json!({"supportsSingleThreadExecutionRequests": true});
        let merged = merge_capabilities(
            Some(merged),
            caps(json!({"supportsSingleThreadExecutionRequests": false})),
            overrides.as_object().cloned(),
        );
        assert_eq!(merged.supports_single_thread_execution_requests, Some(true));
    }
}
