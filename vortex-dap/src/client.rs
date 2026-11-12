use crate::{
    registry::DebugAdapterId,
    requests::{DisconnectArguments, TerminateArguments},
    transport::{Payload, Response, Transport},
    Error, ProgressMap, ProgressState, Requester, Result,
};
use vortex_core::syntax::config::{DebugAdapterConfig, DebuggerQuirks};
use vortex_dap_types::*;

use serde_json::Value;

use anyhow::anyhow;
use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::Stdio,
    sync::{atomic::AtomicU64, Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncWrite, BufReader, BufWriter},
    net::TcpStream,
    process::{Child, Command},
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    time::{self, Instant},
};

#[derive(Debug)]
pub struct Client {
    id: DebugAdapterId,
    process: Option<Child>,
    server_tx: UnboundedSender<Payload>,
    request_counter: Arc<AtomicU64>,
    connection_type: Option<ConnectionType>,
    starting_request_args: Option<Value>,
    /// The socket address of the debugger, if using TCP transport.
    pub socket: Option<SocketAddr>,
    pub caps: Option<DebuggerCapabilities>,
    // thread_id -> frames
    pub stack_frames: HashMap<ThreadId, Vec<StackFrame>>,
    pub thread_states: ThreadStates,
    pub progress: ProgressMap,
    pub thread_id: Option<ThreadId>,
    /// Currently active frame for the current thread.
    pub active_frame: Option<usize>,
    pub quirks: DebuggerQuirks,
    /// The config which was used to start this debugger.
    pub config: Option<DebugAdapterConfig>,
    /// The session which started this one with a `startDebugging` request.
    pub parent: Option<DebugAdapterId>,
    /// Set by the editor once it sent the configuration (breakpoints, `configurationDone`), so
    /// that a second `initialized` event is ignored.
    pub configured: bool,
    /// The display name of the session: the name of the configuration.
    pub name: String,
    /// When the last `stopped` event came from the adapter, before the editor got to it: what
    /// the time of a step is measured to.
    last_stop: Arc<Mutex<Option<Instant>>>,
}

/// How long to wait for a spawned adapter to accept connections.
const TCP_PROCESS_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for an adapter at a known address.
const TCP_TIMEOUT: Duration = Duration::from_secs(2);
const TCP_RETRY_DELAY: Duration = Duration::from_millis(50);

impl Client {
    // Spawn a process and communicate with it by either TCP or stdio
    // The returned stream includes the Client ID so consumers can differentiate between multiple clients
    pub async fn process(
        transport: &str,
        command: &str,
        args: Vec<&str>,
        port_arg: Option<&str>,
        id: DebugAdapterId,
    ) -> Result<(Self, UnboundedReceiver<(DebugAdapterId, Payload)>)> {
        if command.is_empty() {
            return Result::Err(Error::Other(anyhow!("Command not provided")));
        }
        match (transport, port_arg) {
            ("tcp", Some(port_arg)) => Self::tcp_process(command, args, port_arg, id).await,
            ("stdio", _) => Self::stdio(command, args, id),
            _ => Result::Err(Error::Other(anyhow!("Incorrect transport {}", transport))),
        }
    }

    pub fn streams(
        rx: Box<dyn AsyncBufRead + Unpin + Send>,
        tx: Box<dyn AsyncWrite + Unpin + Send>,
        err: Option<Box<dyn AsyncBufRead + Unpin + Send>>,
        id: DebugAdapterId,
        process: Option<Child>,
    ) -> Result<(Self, UnboundedReceiver<(DebugAdapterId, Payload)>)> {
        let (server_rx, server_tx) = Transport::start(rx, tx, err, id);
        let (client_tx, client_rx) = unbounded_channel();

        let client = Self {
            id,
            process,
            server_tx,
            request_counter: Arc::new(AtomicU64::new(0)),
            caps: None,
            connection_type: None,
            starting_request_args: None,
            socket: None,
            stack_frames: HashMap::new(),
            thread_states: HashMap::new(),
            progress: HashMap::new(),
            thread_id: None,
            active_frame: None,
            quirks: DebuggerQuirks::default(),
            config: None,
            parent: None,
            configured: false,
            name: String::new(),
            last_stop: Arc::new(Mutex::new(None)),
        };

        tokio::spawn(Self::recv(
            id,
            server_rx,
            client_tx,
            client.last_stop.clone(),
        ));

        Ok((client, client_rx))
    }

    pub async fn tcp(
        addr: std::net::SocketAddr,
        id: DebugAdapterId,
    ) -> Result<(Self, UnboundedReceiver<(DebugAdapterId, Payload)>)> {
        let deadline = Instant::now() + TCP_TIMEOUT;
        let stream = loop {
            match TcpStream::connect(addr).await {
                Ok(stream) => break stream,
                Err(err) if Instant::now() >= deadline => return Err(err.into()),
                Err(_) => time::sleep(TCP_RETRY_DELAY).await,
            }
        };
        let (rx, tx) = stream.into_split();
        let mut result = Self::streams(Box::new(BufReader::new(rx)), Box::new(tx), None, id, None);
        if let Ok((client, _)) = &mut result {
            client.socket = Some(addr);
        }
        result
    }

    pub fn stdio(
        cmd: &str,
        args: Vec<&str>,
        id: DebugAdapterId,
    ) -> Result<(Self, UnboundedReceiver<(DebugAdapterId, Payload)>)> {
        // Resolve path to the binary
        let cmd = vortex_stdx::env::which(cmd)?;

        let process = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // make sure the process is reaped on drop
            .kill_on_drop(true)
            .spawn();

        let mut process = process?;

        // TODO: do we need bufreader/writer here? or do we use async wrappers on unblock?
        let writer = BufWriter::new(process.stdin.take().expect("Failed to open stdin"));
        let reader = BufReader::new(process.stdout.take().expect("Failed to open stdout"));
        let stderr = BufReader::new(process.stderr.take().expect("Failed to open stderr"));

        Self::streams(
            Box::new(reader),
            Box::new(writer),
            Some(Box::new(stderr)),
            id,
            Some(process),
        )
    }

    async fn get_port() -> Option<u16> {
        Some(
            tokio::net::TcpListener::bind(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                0,
            ))
            .await
            .ok()?
            .local_addr()
            .ok()?
            .port(),
        )
    }

    pub fn starting_request_args(&self) -> Option<&Value> {
        self.starting_request_args.as_ref()
    }

    pub async fn tcp_process(
        cmd: &str,
        args: Vec<&str>,
        port_format: &str,
        id: DebugAdapterId,
    ) -> Result<(Self, UnboundedReceiver<(DebugAdapterId, Payload)>)> {
        let port = Self::get_port().await.unwrap();

        let mut process = Command::new(cmd)
            .args(args)
            .args(port_format.replace("{}", &port.to_string()).split(' '))
            // silence messages
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Do not kill debug adapter when leaving, it should exit automatically
            .spawn()?;

        // Wait for the adapter to become ready for connection
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
        let deadline = Instant::now() + TCP_PROCESS_TIMEOUT;
        let stream = loop {
            match TcpStream::connect(socket).await {
                Ok(stream) => break stream,
                Err(err) => {
                    if let Some(status) = process.try_wait()? {
                        return Err(Error::Other(anyhow!(
                            "the debug adapter `{cmd}` exited ({status}) before accepting connections"
                        )));
                    }
                    if Instant::now() >= deadline {
                        return Err(Error::Other(anyhow!(
                            "could not connect to the debug adapter `{cmd}` on port {port}: {err}"
                        )));
                    }
                    time::sleep(TCP_RETRY_DELAY).await;
                }
            }
        };

        let (rx, tx) = stream.into_split();
        let mut result = Self::streams(
            Box::new(BufReader::new(rx)),
            Box::new(tx),
            None,
            id,
            Some(process),
        );

        // Set the socket address for the client
        if let Ok((client, _)) = &mut result {
            client.socket = Some(socket);
        }

        result
    }

    async fn recv(
        id: DebugAdapterId,
        mut server_rx: UnboundedReceiver<Payload>,
        client_tx: UnboundedSender<(DebugAdapterId, Payload)>,
        last_stop: Arc<Mutex<Option<Instant>>>,
    ) {
        while let Some(msg) = server_rx.recv().await {
            let payload = match msg {
                Payload::Event(ev) => {
                    if ev.event == "stopped" {
                        *last_stop.lock().unwrap() = Some(Instant::now());
                    }
                    Payload::Event(ev)
                }
                Payload::Request(req) => Payload::Request(req),
                Payload::Response(_) => unreachable!(),
            };
            // The editor dropped the receiver (the client was removed): end quietly.
            if client_tx.send((id, payload)).is_err() {
                break;
            }
        }
    }

    pub fn id(&self) -> DebugAdapterId {
        self.id
    }

    /// When the last `stopped` event arrived from the adapter.
    pub fn last_stop_received(&self) -> Option<std::time::Instant> {
        self.last_stop.lock().unwrap().map(Instant::into_std)
    }

    pub fn connection_type(&self) -> Option<ConnectionType> {
        self.connection_type
    }

    /// A cheap handle to send requests without borrowing the client.
    ///
    /// Its view of the capabilities is a snapshot: make it after `initialize`.
    pub fn requester(&self) -> Requester {
        let supports_configuration_done = self
            .caps
            .as_ref()
            .and_then(|caps| caps.supports_configuration_done_request)
            .unwrap_or(false);
        Requester::new(
            self.id,
            self.server_tx.clone(),
            self.request_counter.clone(),
            supports_configuration_done,
        )
    }

    // Internal, called by specific DAP commands when resuming
    pub fn resume_application(&mut self) {
        if let Some(thread_id) = self.thread_id {
            self.thread_states.insert(thread_id, "running".to_string());
            self.stack_frames.remove(&thread_id);
        }
        self.active_frame = None;
        self.thread_id = None;
    }

    /// Execute a RPC request on the debugger.
    pub fn call<R: vortex_dap_types::Request + 'static>(
        &self,
        arguments: R::Arguments,
    ) -> impl Future<Output = Result<Value>> {
        self.requester().call::<R>(arguments)
    }

    pub fn request<R: vortex_dap_types::Request + 'static>(
        &self,
        params: R::Arguments,
    ) -> impl Future<Output = Result<R::Result>> {
        self.requester().request::<R>(params)
    }

    pub fn reply(
        &self,
        request_seq: u64,
        command: &str,
        result: core::result::Result<Value, Error>,
    ) -> impl Future<Output = Result<()>> {
        let server_tx = self.server_tx.clone();
        let command = command.to_string();

        async move {
            let response = match result {
                Ok(result) => Response {
                    request_seq,
                    command,
                    success: true,
                    message: None,
                    body: Some(result),
                },
                Err(error) => Response {
                    request_seq,
                    command,
                    success: false,
                    message: Some(error.to_string()),
                    body: None,
                },
            };

            server_tx
                .send(Payload::Response(response))
                .map_err(|e| Error::Other(e.into()))?;

            Ok(())
        }
    }

    pub fn capabilities(&self) -> &DebuggerCapabilities {
        self.caps.as_ref().expect("debugger not yet initialized!")
    }

    pub fn progress_start(&mut self, event: events::ProgressStartBody) -> String {
        let status = ProgressState::new(event.title, event.message, event.percentage);
        let status_line = status.status_line();
        self.progress.insert(event.progress_id, status);
        status_line
    }

    pub fn progress_update(&mut self, event: events::ProgressUpdateBody) -> Option<String> {
        let status = self.progress.get_mut(&event.progress_id)?;
        status.update(event.message, event.percentage);
        Some(status.status_line())
    }

    pub fn progress_end(&mut self, event: events::ProgressEndBody) -> Option<String> {
        let status = self.progress.remove(&event.progress_id)?;
        Some(status.end_status_line(event.message.as_deref()))
    }

    pub async fn initialize(&mut self, adapter_id: String) -> Result<()> {
        let args = requests::InitializeArguments {
            client_id: Some("vx".to_owned()),
            client_name: Some("vortex".to_owned()),
            adapter_id,
            locale: Some("en-us".to_owned()),
            lines_start_at_one: Some(true),
            columns_start_at_one: Some(true),
            path_format: Some("path".to_owned()),
            supports_variable_type: Some(true),
            supports_variable_paging: Some(true),
            supports_run_in_terminal_request: Some(true),
            supports_memory_references: Some(true),
            supports_progress_reporting: Some(true),
            supports_invalidated_event: Some(false),
            supports_start_debugging_request: Some(true),
            supports_memory_event: Some(true),
        };

        let mut response = self.request::<requests::Initialize>(args).await?;
        // capabilities the configuration assumes over the adapter's
        if let Some(overrides) = self.config.as_ref().and_then(|c| c.capabilities.as_ref()) {
            if let Ok(serde_json::Value::Object(mut caps)) = serde_json::to_value(&response) {
                caps.extend(overrides.clone());
                match serde_json::from_value(serde_json::Value::Object(caps)) {
                    Ok(merged) => response = merged,
                    Err(err) => log::warn!("Ignoring the configured capabilities: {err}"),
                }
            }
        }
        self.caps = Some(response);

        Ok(())
    }

    pub fn disconnect(
        &mut self,
        args: Option<DisconnectArguments>,
    ) -> impl Future<Output = Result<Value>> {
        self.connection_type = None;
        self.call::<requests::Disconnect>(args)
    }

    pub fn terminate(
        &mut self,
        args: Option<TerminateArguments>,
    ) -> impl Future<Output = Result<Value>> {
        self.connection_type = None;
        self.call::<requests::Terminate>(args)
    }

    /// Remembers how the session started, for a restart, when the request is sent through a
    /// [`Requester`].
    /// The adapter process this client started, taken out so that it can be stopped once the
    /// session is over (a TCP adapter like js-debug's server doesn't exit by itself).
    pub fn take_process(&mut self) -> Option<Child> {
        self.process.take()
    }

    /// Remembers how the session started, for a restart, when the request is sent through a
    /// [`Requester`].
    pub fn set_start(&mut self, connection_type: ConnectionType, args: serde_json::Value) {
        self.connection_type = Some(connection_type);
        self.starting_request_args = Some(args);
    }

    pub fn launch(&mut self, args: serde_json::Value) -> impl Future<Output = Result<Value>> {
        self.connection_type = Some(ConnectionType::Launch);
        self.starting_request_args = Some(args.clone());
        self.call::<requests::Launch>(args)
    }

    pub fn attach(&mut self, args: serde_json::Value) -> impl Future<Output = Result<Value>> {
        self.connection_type = Some(ConnectionType::Attach);
        self.starting_request_args = Some(args.clone());
        self.call::<requests::Attach>(args)
    }

    pub fn restart(&self) -> impl Future<Output = Result<Value>> {
        let args = if let Some(args) = &self.starting_request_args {
            args.clone()
        } else {
            Value::Null
        };
        self.call::<requests::Restart>(args)
    }

    pub async fn set_breakpoints(
        &self,
        file: PathBuf,
        breakpoints: Vec<SourceBreakpoint>,
    ) -> Result<Option<Vec<Breakpoint>>> {
        self.requester().set_breakpoints(file, breakpoints).await
    }

    pub async fn configuration_done(&self) -> Result<()> {
        self.requester().configuration_done().await
    }

    pub fn continue_thread(&self, thread_id: ThreadId) -> impl Future<Output = Result<Value>> {
        let args = requests::ContinueArguments {
            thread_id,
            single_thread: None,
        };

        self.call::<requests::Continue>(args)
    }

    pub async fn stack_trace(
        &self,
        thread_id: ThreadId,
    ) -> Result<(Vec<StackFrame>, Option<usize>)> {
        self.requester().stack_trace(thread_id).await
    }

    pub fn threads(&self) -> impl Future<Output = Result<Value>> {
        self.call::<requests::Threads>(Some(requests::ThreadsArguments {}))
    }

    pub async fn scopes(&self, frame_id: usize) -> Result<Vec<Scope>> {
        self.requester().scopes(frame_id).await
    }

    pub async fn variables(&self, variables_reference: usize) -> Result<Vec<Variable>> {
        self.requester().variables(variables_reference).await
    }

    pub fn step_in(&self, thread_id: ThreadId) -> impl Future<Output = Result<Value>> {
        let args = requests::StepInArguments {
            thread_id,
            target_id: None,
            granularity: None,
            single_thread: None,
        };

        self.call::<requests::StepIn>(args)
    }

    pub fn step_out(&self, thread_id: ThreadId) -> impl Future<Output = Result<Value>> {
        let args = requests::StepOutArguments {
            thread_id,
            granularity: None,
            single_thread: None,
        };

        self.call::<requests::StepOut>(args)
    }

    pub fn next(&self, thread_id: ThreadId) -> impl Future<Output = Result<Value>> {
        let args = requests::NextArguments {
            thread_id,
            granularity: None,
            single_thread: None,
        };

        self.call::<requests::Next>(args)
    }

    pub fn pause(&self, thread_id: ThreadId) -> impl Future<Output = Result<Value>> {
        let args = requests::PauseArguments { thread_id };

        self.call::<requests::Pause>(args)
    }

    pub async fn eval(
        &self,
        expression: String,
        frame_id: Option<usize>,
    ) -> Result<requests::EvaluateResponse> {
        let args = requests::EvaluateArguments {
            expression,
            frame_id,
            context: None,
            format: None,
        };

        self.request::<requests::Evaluate>(args).await
    }

    pub fn set_exception_breakpoints(
        &self,
        filters: Vec<String>,
    ) -> impl Future<Output = Result<Value>> {
        let args = requests::SetExceptionBreakpointsArguments { filters };

        self.call::<requests::SetExceptionBreakpoints>(args)
    }

    pub fn current_stack_frame(&self) -> Option<&StackFrame> {
        self.stack_frames
            .get(&self.thread_id?)?
            .get(self.active_frame?)
    }
}
