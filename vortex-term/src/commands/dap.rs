use super::{Context, Editor};
use crate::{
    compositor::{self, Compositor},
    debug::{self, breakpoints, load, panel},
    job::{Callback, Jobs},
    ui::{self, Picker, Prompt, PromptEvent},
};
use dap::{StackFrame, Thread, ThreadStates};
use vortex_core::syntax::config::{DebugConfigCompletion, DebugTemplate};
use vortex_dap::{
    self as dap,
    requests::{DisconnectArguments, TerminateArguments},
};
use vortex_view::debug::Section;

use serde_json::{to_value, Value};

use std::collections::HashMap;
use std::future::Future;

use anyhow::{anyhow, bail};

fn thread_picker(
    cx: &mut Context,
    callback_fn: impl Fn(&mut Editor, &dap::Thread) + Send + 'static,
) {
    let debugger = debugger!(cx.editor);

    let future = debugger.requester().threads();
    let callback = Box::pin(async move {
        let threads = future.await?;
        let call: Callback = Callback::EditorCompositor(Box::new(
            move |editor: &mut Editor, compositor: &mut Compositor| {
                if threads.len() == 1 {
                    callback_fn(editor, &threads[0]);
                    return;
                }
                let debugger = debugger!(editor);

                let thread_states = debugger.thread_states.clone();
                let columns = [
                    ui::PickerColumn::new("name", |item: &Thread, _| item.name.as_str().into()),
                    ui::PickerColumn::new(
                        "state",
                        |item: &Thread, thread_states: &ThreadStates| {
                            thread_states
                                .get(&item.id)
                                .map(|state| state.as_str())
                                .unwrap_or("unknown")
                                .into()
                        },
                    ),
                ];
                let picker = Picker::new(
                    columns,
                    0,
                    threads,
                    thread_states,
                    move |cx, thread, _action| callback_fn(cx.editor, thread),
                )
                .with_preview(move |editor, thread| {
                    let frames = editor
                        .debug_adapters
                        .get_active_client()
                        .as_ref()?
                        .stack_frames
                        .get(&thread.id)?;
                    let frame = frames.first()?;
                    let path = frame.source.as_ref()?.path.as_ref()?.as_path();
                    let pos = Some((
                        frame.line.saturating_sub(1),
                        frame.end_line.unwrap_or(frame.line).saturating_sub(1),
                    ));
                    Some((path.into(), pos))
                });
                compositor.push(Box::new(picker));
            },
        ));
        Ok(call)
    });
    cx.jobs.callback(callback);
}

// -- DAP

fn dap_callback<T, F>(
    jobs: &mut Jobs,
    call: impl Future<Output = vortex_dap::Result<T>> + 'static + Send,
    callback: F,
) where
    T: Send + 'static,
    F: FnOnce(&mut Editor, &mut Compositor, T) + Send + 'static,
{
    let callback = Box::pin(async move {
        let response = call.await?;
        let call: Callback = Callback::EditorCompositor(Box::new(
            move |editor: &mut Editor, compositor: &mut Compositor| {
                callback(editor, compositor, response)
            },
        ));
        Ok(call)
    });

    jobs.callback(callback);
}

pub fn dap_start_impl(
    cx: &mut compositor::Context,
    name: Option<&str>,
    socket: Option<std::net::SocketAddr>,
    params: Option<Vec<std::borrow::Cow<str>>>,
) -> Result<(), anyhow::Error> {
    let doc = doc!(cx.editor);
    let config = doc
        .language_config()
        .and_then(|config| config.debugger.as_ref())
        .ok_or_else(|| anyhow!("No debug adapter available for language"))?
        .clone();

    let template = match name {
        Some(name) => config.templates.iter().find(|t| t.name == name),
        None => config.templates.first(),
    }
    .ok_or_else(|| anyhow!("No debug config with given name"))?
    .clone();

    let params: Vec<String> = params
        .map(|params| params.iter().map(|param| param.to_string()).collect())
        .unwrap_or_default();
    start_session(cx.editor, socket, &config, &template, &params)
}

/// Starts a session of `template` of the debugger `config`, its prompts answered with `params`
/// (this fork: `vx --debug`, and the templates of any language).
pub fn start_with(
    editor: &mut Editor,
    config: &vortex_core::syntax::config::DebugAdapterConfig,
    template: &DebugTemplate,
    params: &[String],
) -> anyhow::Result<()> {
    start_session(editor, None, config, template, params)
}

fn start_session(
    editor: &mut Editor,
    socket: Option<std::net::SocketAddr>,
    config: &vortex_core::syntax::config::DebugAdapterConfig,
    template: &DebugTemplate,
    params: &[String],
) -> anyhow::Result<()> {
    // the workspace of the current file, or of the working directory
    let workspace = match doc!(editor).path() {
        Some(_) => doc!(editor).workspace_root().to_path_buf(),
        None => vortex_loader::find_workspace().0,
    };
    // Refuse to spawn a debug adapter in workspace trust restricted mode.
    if !editor
        .workspace_trust
        .query(&workspace, vortex_loader::workspace_trust::TrustQuery::Dap)
        .is_trusted()
    {
        bail!("Workspace is not trusted. Run `:workspace-trust` to enable the debug adapter.");
    }

    let params: Vec<std::borrow::Cow<str>> = params.iter().map(|p| p.as_str().into()).collect();
    let params = prepare_dap_params(template, &params);
    let mut args: HashMap<&str, Value> = template
        .args
        .iter()
        .map(|(k, v)| (k.as_str(), map_value(v, &params, &workspace)))
        .collect();

    // a template's own working directory wins
    args.entry("cwd")
        .or_insert(to_value(vortex_stdx::env::current_working_dir())?);
    // like a launch configuration of VS Code: adapters may read them (js-debug attaches only when
    // `request` says so)
    args.entry("request")
        .or_insert(Value::String(template.request.clone()));
    args.entry("name")
        .or_insert(Value::String(template.name.clone()));

    let args = to_value(args).unwrap();
    let session_name = format!("{}: {}", config.name, template.name);
    let request = template.request.clone();
    debug::start(editor, socket, config, &session_name, &request, args)
}

/// Launches `template`: at once when it has no prompts, else after asking them.
pub fn launch_template(
    cx: &mut compositor::Context,
    config: vortex_core::syntax::config::DebugAdapterConfig,
    template: DebugTemplate,
) {
    if template.completion.is_empty() {
        if let Err(err) = start_with(cx.editor, &config, &template, &[]) {
            cx.editor.set_error(err.to_string());
        }
        return;
    }
    let callback = Box::pin(async move {
        let call: Callback = Callback::EditorCompositor(Box::new(move |_editor, compositor| {
            let prompt = debug_parameter_prompt(config, template, Vec::new());
            compositor.push(Box::new(prompt));
        }));
        Ok(call)
    });
    cx.jobs.callback(callback);
}

fn prepare_dap_params(template: &DebugTemplate, params: &[std::borrow::Cow<str>]) -> Vec<String> {
    params
        .iter()
        .enumerate()
        .map(|(i, x)| {
            let mut param = x.to_string();
            if let Some(DebugConfigCompletion::Advanced(cfg)) = template.completion.get(i) {
                if matches!(cfg.completion.as_deref(), Some("filename" | "directory")) {
                    param = std::fs::canonicalize(x.as_ref())
                        .ok()
                        .and_then(|pb| pb.into_os_string().into_string().ok())
                        .unwrap_or_else(|| x.to_string());
                }
            }
            param
        })
        .collect()
}

/// Fills a template's arguments: `{0}`, `{1}`... are the parameters the user gave and
/// `${workspaceFolder}` the workspace's root. In a list, an item that is exactly `{N}` becomes
/// the parameter's words (quotes group words), so `args = ["{1}"]` takes a command line of
/// arguments.
fn map_value(value: &Value, params: &[String], workspace: &std::path::Path) -> Value {
    let fill = |string: &str| {
        let mut string = string.replace("${workspaceFolder}", &workspace.to_string_lossy());
        for (i, x) in params.iter().enumerate() {
            let pattern = format!("{{{}}}", i);
            string = string.replace(&pattern, x);
        }
        string
    };
    match value {
        Value::String(string) => {
            let string = fill(string);
            if let Ok(integer) = string.parse::<usize>() {
                to_value(integer).unwrap()
            } else {
                to_value(string).unwrap()
            }
        }
        Value::Array(array) => {
            let mut items = Vec::new();
            for item in array {
                let whole_param = item.as_str().and_then(|s| {
                    let index = s
                        .strip_prefix('{')?
                        .strip_suffix('}')?
                        .parse::<usize>()
                        .ok()?;
                    Some(index)
                });
                match whole_param {
                    Some(index) => {
                        let words = params.get(index).map(String::as_str).unwrap_or("");
                        items.extend(
                            vortex_core::command_line::Tokenizer::new(words, false)
                                .filter_map(Result::ok)
                                .map(|token| Value::String(token.content.into_owned())),
                        );
                    }
                    None => items.push(map_value(item, params, workspace)),
                }
            }
            Value::Array(items)
        }
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(k, v)| (k.clone(), map_value(v, params, workspace)))
                .collect(),
        ),

        _ => value.clone(),
    }
}

pub fn dap_launch(cx: &mut Context) {
    if cx.editor.debug_adapters.iter().next().is_some() {
        cx.editor.set_error("A debug session is already running");
        return;
    }

    // the templates of the file's language; without a debugger for it, every language's
    let doc = doc!(cx.editor);
    let candidates: Vec<debug::launch::Candidate> = match doc
        .language_config()
        .and_then(|config| Some((config.language_id.clone(), config.debugger.clone()?)))
    {
        Some((language, config)) => config
            .templates
            .iter()
            .map(|template| debug::launch::Candidate {
                language: language.clone(),
                config: config.clone(),
                template: template.clone(),
            })
            .collect(),
        None => debug::launch::all(cx.editor),
    };
    if candidates.is_empty() {
        cx.editor.set_error("No debug adapter available");
        return;
    }
    cx.push_layer(debug::launch::picker(candidates, None));
}

pub fn dap_restart(cx: &mut Context) {
    let debugger = match cx.editor.debug_adapters.get_active_client() {
        Some(debugger) => debugger,
        None => {
            cx.editor.set_error("Debugger is not running");
            return;
        }
    };
    let root = cx.editor.debug_adapters.root(debugger.id());
    let debugger = cx.editor.debug_adapters.get_client(root).unwrap();
    if !debugger
        .capabilities()
        .supports_restart_request
        .unwrap_or(false)
    {
        cx.editor
            .set_error("Debugger does not support session restarts");
        return;
    }
    if debugger.starting_request_args().is_none() {
        cx.editor
            .set_error("No arguments found with which to restart the sessions");
        return;
    }

    dap_callback(
        cx.jobs,
        debugger.restart(),
        |editor, _compositor, _resp: Value| {
            crate::debug::mode::say(editor, "Debugging session restarted")
        },
    );
}

fn debug_parameter_prompt(
    config: vortex_core::syntax::config::DebugAdapterConfig,
    template: DebugTemplate,
    mut params: Vec<String>,
) -> Prompt {
    let completions = template.completion.clone();
    let completion = completions.get(params.len()).unwrap();
    let field_type = if let DebugConfigCompletion::Advanced(cfg) = completion {
        cfg.completion.as_deref().unwrap_or("")
    } else {
        ""
    };
    let name = match completion {
        DebugConfigCompletion::Advanced(cfg) => cfg.name.as_deref().unwrap_or(field_type),
        DebugConfigCompletion::Named(name) => name.as_str(),
    };
    let default_val = match completion {
        DebugConfigCompletion::Advanced(cfg) => cfg.default.as_deref().unwrap_or(""),
        _ => "",
    }
    .to_owned();

    let completer = match field_type {
        "filename" => |editor: &Editor, input: &str| {
            ui::completers::filename_with_git_ignore(editor, input, false)
        },
        "directory" => |editor: &Editor, input: &str| {
            ui::completers::directory_with_git_ignore(editor, input, false)
        },
        _ => ui::completers::none,
    };

    Prompt::new(
        format!("{}: ", name).into(),
        None,
        completer,
        move |cx, input: &str, event: PromptEvent| {
            if event != PromptEvent::Validate {
                return;
            }

            let mut value = input.to_owned();
            if value.is_empty() {
                value = default_val.clone();
            }
            params.push(value);

            if params.len() < completions.len() {
                let config = config.clone();
                let template = template.clone();
                let params = params.clone();
                let callback = Box::pin(async move {
                    let call: Callback =
                        Callback::EditorCompositor(Box::new(move |_editor, compositor| {
                            let prompt = debug_parameter_prompt(config, template, params);
                            compositor.push(Box::new(prompt));
                        }));
                    Ok(call)
                });
                cx.jobs.callback(callback);
            } else if let Err(err) = start_with(cx.editor, &config, &template, &params) {
                cx.editor.set_error(err.to_string());
            }
        },
    )
}

pub fn dap_toggle_breakpoint(cx: &mut Context) {
    if breakpoints::outside_debug_mode(cx.editor) {
        return;
    }
    let Some((path, line)) = panel::cursor_line(cx.editor) else {
        cx.editor
            .set_error("Can't set breakpoint: document has no path");
        return;
    };
    breakpoints::toggle(cx.editor, path, line);
}

pub fn dap_continue(cx: &mut Context) {
    debug::rewind::to_live(cx.editor);
    if cx.editor.debug.spectator.is_playing() {
        debug::playback::pause(cx.editor);
    }
    let debugger = debugger!(cx.editor);
    let Some(thread) = debugger.thread_id else {
        cx.editor.set_error("The current thread is not stopped");
        return;
    };
    let key = debug::spectator_key(debugger.id(), thread);
    // the engine lifts its hidden breakpoints and runs at full speed to your next one
    if !debug::engine::full_speed(cx.editor, key) {
        debug::threads::continue_thread(cx.editor, key);
    }
}

pub fn dap_pause(cx: &mut Context) {
    thread_picker(cx, |editor, thread| {
        let debugger = debugger!(editor);
        // the adapter sends a "stopped" event
        let request = debugger.requester().pause(thread.id);
        tokio::spawn(async move {
            if let Err(err) = request.await {
                crate::job::dispatch(move |editor, _| {
                    editor.set_error(format!("Failed to pause: {err}"))
                })
                .await;
            }
        });
    })
}

pub fn dap_step_in(cx: &mut Context) {
    debug::threads::step(cx.editor, dap::StepKind::In);
}

pub fn dap_step_out(cx: &mut Context) {
    debug::threads::step(cx.editor, dap::StepKind::Out);
}

pub fn dap_next(cx: &mut Context) {
    debug::threads::step(cx.editor, dap::StepKind::Over);
}

// spectator mode, from debug mode's keys

pub fn dap_play(cx: &mut Context) {
    debug::playback::toggle(cx.editor);
}

pub fn dap_play_step(cx: &mut Context) {
    debug::playback::step_once(cx.editor);
}

pub fn dap_faster(cx: &mut Context) {
    cx.editor.debug.spectator.faster();
    let speed = cx.editor.debug.spectator.speed_label();
    crate::debug::mode::say(cx.editor, format!("Playback speed: {speed}"));
}

pub fn dap_slower(cx: &mut Context) {
    cx.editor.debug.spectator.slower();
    let speed = cx.editor.debug.spectator.speed_label();
    crate::debug::mode::say(cx.editor, format!("Playback speed: {speed}"));
}

pub fn dap_fast_forward(cx: &mut Context) {
    if cx.editor.debug.spectator.mode == vortex_view::debug::spectator::Mode::FastForward {
        debug::playback::pause(cx.editor);
    } else {
        debug::playback::play(cx.editor, vortex_view::debug::spectator::Mode::FastForward);
    }
}

pub fn dap_run_to_cursor(cx: &mut Context) {
    debug::playback::run_to_cursor(cx.editor);
}

pub fn dap_next_thread(cx: &mut Context) {
    debug::threads::cycle(cx.editor, 1);
}

pub fn dap_previous_thread(cx: &mut Context) {
    debug::threads::cycle(cx.editor, -1);
}

pub fn dap_follow_mode(cx: &mut Context) {
    let spectator = &mut cx.editor.debug.spectator;
    spectator.follow = spectator.follow.next();
    let label = spectator.follow.label();
    crate::debug::mode::say(cx.editor, format!("Playback follows: {label}"));
    debug::engine::follow_changed(cx.editor);
}

pub fn dap_freeze_thread(cx: &mut Context) {
    match debug::playback::spectated(cx.editor) {
        Some(key) => debug::threads::toggle_freeze(cx.editor, key),
        None => cx.editor.set_error("No thread to freeze"),
    }
}

pub fn dap_solo_thread(cx: &mut Context) {
    debug::threads::toggle_solo(cx.editor);
}

pub fn dap_rewind_back(cx: &mut Context) {
    debug::rewind::back(cx.editor, cx.count());
}

pub fn dap_rewind_forward(cx: &mut Context) {
    debug::rewind::forward(cx.editor, cx.count());
}

pub fn dap_live(cx: &mut Context) {
    debug::rewind::to_live(cx.editor);
}

pub fn dap_capture_settings(cx: &mut Context) {
    debug::focus_section(cx.editor, cx.editor.debug.section);
    cx.editor.debug.spectator.settings = Some(0);
}

pub fn dap_clear_time_labels(cx: &mut Context) {
    cx.editor.debug.spectator.timing.clear_labels();
    crate::debug::mode::say(cx.editor, "Time labels cleared");
}

pub fn dap_memory(cx: &mut Context) {
    debug::memory::open_selected(cx.editor);
}

pub fn dap_variables(cx: &mut Context) {
    debug::focus_section(cx.editor, Section::Variables);
}

pub fn dap_terminate(cx: &mut Context) {
    let Some(active) = cx.editor.debug_adapters.get_active_client() else {
        cx.editor.set_error("No debug session");
        return;
    };
    let root = cx.editor.debug_adapters.root(active.id());
    let debugger = cx.editor.debug_adapters.get_client(root).unwrap();
    let requester = debugger.requester();
    // an attached program is left running (detached); a launched one ends
    let attached = debugger.connection_type() == Some(dap::ConnectionType::Attach);
    let terminate = !attached
        && debugger
            .caps
            .as_ref()
            .is_some_and(|c| c.supports_terminate_request.unwrap_or_default());
    log::info!(
        "{}",
        if attached {
            "detaching from the program"
        } else {
            "ending the debug session"
        }
    );
    tokio::spawn(async move {
        let terminated = terminate
            && requester
                .terminate(Some(TerminateArguments {
                    restart: Some(false),
                }))
                .await
                .is_ok();
        if terminated {
            // the adapter's `terminated` event ends the session; if it never comes, end it here
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            crate::job::dispatch(move |editor, _| {
                if editor.debug_adapters.get_client(root).is_some() {
                    debug::end(editor, root);
                }
            })
            .await;
            return;
        }
        // without `terminate` (or when it failed, or to detach): disconnect, which ends a
        // launched program
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            requester.disconnect(Some(DisconnectArguments {
                restart: Some(false),
                terminate_debuggee: Some(!attached),
                suspend_debuggee: None,
            })),
        )
        .await;
        crate::job::dispatch(move |editor, _| {
            if editor.debug_adapters.get_client(root).is_some() {
                debug::end_with(editor, root, false);
            }
        })
        .await;
    });
}

/// Sends the exception filters to every configured session, and keeps them for new ones.
fn set_exception_filters(cx: &mut Context, enable: bool) {
    let debugger = debugger!(cx.editor);
    let filters: Vec<String> = if enable {
        match &debugger.capabilities().exception_breakpoint_filters {
            Some(filters) => filters.iter().map(|f| f.filter.clone()).collect(),
            None => {
                cx.editor
                    .set_error("The debug adapter has no exception breakpoints");
                return;
            }
        }
    } else {
        Vec::new()
    };
    cx.editor.debug.exception_filters = Some(filters.clone());
    let requesters: Vec<_> = cx
        .editor
        .debug_adapters
        .iter()
        .filter(|(_, client)| client.configured)
        .map(|(_, client)| client.requester())
        .collect();
    for requester in requesters {
        let filters = filters.clone();
        tokio::spawn(async move {
            if let Err(err) = requester.set_exception_breakpoints(filters).await {
                log::warn!("setExceptionBreakpoints: {err}");
            }
        });
    }
    crate::debug::mode::say(
        cx.editor,
        if enable {
            "Stopping on exceptions"
        } else {
            "Not stopping on exceptions"
        },
    );
}

pub fn dap_enable_exceptions(cx: &mut Context) {
    set_exception_filters(cx, true);
}

pub fn dap_disable_exceptions(cx: &mut Context) {
    set_exception_filters(cx, false);
}

/// Stops on exceptions (every filter the adapter offers), or stops not stopping on them.
pub fn dap_toggle_exceptions(cx: &mut Context) {
    let on = cx
        .editor
        .debug
        .exception_filters
        .as_ref()
        .is_some_and(|filters| !filters.is_empty());
    set_exception_filters(cx, !on);
}

/// A prompt on the command line to edit a field of the breakpoint on the cursor's line.
fn edit_breakpoint_prompt(
    cx: &mut Context,
    label: &'static str,
    get: fn(&vortex_view::editor::Breakpoint) -> Option<String>,
    set: fn(&mut vortex_view::editor::Breakpoint, Option<String>),
) {
    let Some((path, line)) = panel::cursor_line(cx.editor) else {
        return;
    };
    let Some(breakpoint) = cx
        .editor
        .breakpoints
        .get(&path)
        .and_then(|list| list.iter().find(|b| b.line == line))
    else {
        cx.editor.set_error("No breakpoint on this line");
        return;
    };
    let current = get(breakpoint).unwrap_or_default();
    let callback = Box::pin(async move {
        let call: Callback = Callback::EditorCompositor(Box::new(move |editor, compositor| {
            let prompt = Prompt::new(
                label.into(),
                None,
                ui::completers::none,
                move |cx, input: &str, event: PromptEvent| {
                    if event != PromptEvent::Validate {
                        return;
                    }
                    let value = (!input.is_empty()).then(|| input.to_owned());
                    if let Err(err) =
                        breakpoints::edit(cx.editor, &path, line, |b| set(b, value.clone()))
                    {
                        cx.editor.set_error(err.to_string());
                    }
                },
            )
            .with_line(current, editor);
            compositor.push(Box::new(prompt));
        }));
        Ok(call)
    });
    cx.jobs.callback(callback);
}

pub fn dap_edit_condition(cx: &mut Context) {
    if breakpoints::outside_debug_mode(cx.editor) {
        return;
    }
    edit_breakpoint_prompt(
        cx,
        "condition: ",
        |b| b.condition.clone(),
        |b, value| b.condition = value,
    );
}

pub fn dap_edit_log(cx: &mut Context) {
    if breakpoints::outside_debug_mode(cx.editor) {
        return;
    }
    edit_breakpoint_prompt(
        cx,
        "log message: ",
        |b| b.log_message.clone(),
        |b, value| b.log_message = value,
    );
}

pub fn dap_switch_thread(cx: &mut Context) {
    thread_picker(cx, |editor, thread| {
        if let Some(session) = editor.debug_adapters.get_active_client().map(|c| c.id()) {
            debug::threads::spectate(editor, debug::spectator_key(session, thread.id));
        }
    })
}

pub fn dap_switch_stack_frame(cx: &mut Context) {
    let debugger = debugger!(cx.editor);
    let session = debugger.id();

    let thread_id = match debugger.thread_id {
        Some(thread_id) => thread_id,
        None => {
            cx.editor.set_error("No thread is currently active");
            return;
        }
    };

    let frames = debugger
        .stack_frames
        .get(&thread_id)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .collect::<Vec<_>>();
    let thread_state = debugger
        .thread_states
        .get(&thread_id)
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    let columns = [ui::PickerColumn::new(
        "frame",
        |item: &(usize, StackFrame), thread_state: &String| {
            format!("{} ({})", item.1.name, thread_state).into()
        },
    )];
    let picker = Picker::new(
        columns,
        0,
        frames,
        thread_state,
        move |cx, (index, _frame), _action| {
            load::select_frame(cx.editor, debug::spectator_key(session, thread_id), *index);
        },
    )
    .with_preview(move |_editor, (_index, frame)| {
        frame.source.as_ref().and_then(|source| {
            source.path.as_ref().map(|path| {
                (
                    path.as_path().into(),
                    Some((
                        frame.line.saturating_sub(1),
                        frame.end_line.unwrap_or(frame.line).saturating_sub(1),
                    )),
                )
            })
        })
    });
    cx.push_layer(Box::new(picker))
}

/// Debug mode's `P`: shows or hides the debug panel.
pub fn debug_panel(cx: &mut Context) {
    debug::toggle(cx.editor);
}

/// `C-g`: focuses the debug panel, or goes back to the editor.
pub fn debug_panel_focus(cx: &mut Context) {
    debug::toggle_focus(cx.editor);
}

/// Debug mode's `w`: adds a watch expression.
pub fn dap_watch(cx: &mut Context) {
    debug::show(cx.editor);
    let callback = Box::pin(async move {
        let call: Callback = Callback::EditorCompositor(Box::new(move |editor, compositor| {
            compositor.push(Box::new(panel::watch_prompt(editor, String::new())));
        }));
        Ok(call)
    });
    cx.jobs.callback(callback);
}

/// Debug mode's `x`: evaluates an expression in the selected frame; the console shows the result.
pub fn dap_evaluate(cx: &mut Context) {
    debug::show(cx.editor);
    let callback = Box::pin(async move {
        let call: Callback = Callback::EditorCompositor(Box::new(move |_editor, compositor| {
            compositor.push(Box::new(panel::evaluate_prompt()));
        }));
        Ok(call)
    });
    cx.jobs.callback(callback);
}

#[cfg(test)]
mod tests {
    use super::map_value;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn a_list_item_that_is_a_parameter_takes_its_words() {
        let params = vec![
            "/bin/demo".to_owned(),
            r#"--port 8080 "a b" 'c d'"#.to_owned(),
        ];
        let template = json!({
            "program": "{0}",
            "args": ["first", "{1}", "last"],
            "label": "run {0} with {1}",
        });
        let filled = map_value(&template, &params, Path::new("/work"));
        assert_eq!(filled["program"], json!("/bin/demo"));
        assert_eq!(
            filled["args"],
            json!(["first", "--port", "8080", "a b", "c d", "last"])
        );
        assert_eq!(
            filled["label"],
            json!(r#"run /bin/demo with --port 8080 "a b" 'c d'"#)
        );
    }

    #[test]
    fn an_empty_parameter_gives_no_arguments() {
        let filled = map_value(
            &json!({ "args": ["{1}"] }),
            &["x".to_owned(), String::new()],
            Path::new("/work"),
        );
        assert_eq!(filled["args"], json!([]));
    }

    #[test]
    fn the_workspace_folder_and_numbers_are_filled() {
        let filled = map_value(
            &json!({ "root": "${workspaceFolder}", "out": ["${workspaceFolder}/out/**/*.js"], "port": "{0}" }),
            &["9229".to_owned()],
            Path::new("/work"),
        );
        assert_eq!(filled["root"], json!("/work"));
        assert_eq!(filled["out"], json!(["/work/out/**/*.js"]));
        assert_eq!(filled["port"], json!(9229));
    }
}
