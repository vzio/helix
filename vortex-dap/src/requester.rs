use crate::{
    registry::DebugAdapterId,
    requests::{DisconnectArguments, TerminateArguments},
    transport::{Payload, Request as TransportRequest, Response},
    Error, Result,
};
use anyhow::anyhow;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    future::Future,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::mpsc::UnboundedSender, time::timeout};
use vortex_dap_types::*;

/// How long a request waits for its response.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Which stepping request to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// `next`
    Over,
    /// `stepIn`
    In,
    /// `stepOut`
    Out,
}

/// A cheap handle to send requests to a debug adapter.
///
/// It shares the connection and the request counter with its [`Client`](crate::Client) but
/// borrows nothing from it: the futures it returns are `'static` and `Send`, so a chain of
/// requests can run in a background task. The capabilities are a snapshot from when the
/// handle was made.
#[derive(Debug, Clone)]
pub struct Requester {
    id: DebugAdapterId,
    server_tx: UnboundedSender<Payload>,
    request_counter: Arc<AtomicU64>,
    supports_configuration_done: bool,
}

/// Deserializes the body of a response.
///
/// Adapters may omit the body, or send `{}` where the spec has none: a missing body reads as
/// `{}` when it is not `null` (a struct with only optional fields), and `{}` as `null` when it
/// is not `{}` (a unit result).
pub(crate) fn parse_body<T: DeserializeOwned>(body: Value) -> Result<T> {
    let fallback = match &body {
        Value::Null => Some(Value::Object(Default::default())),
        Value::Object(map) if map.is_empty() => Some(Value::Null),
        _ => None,
    };
    match serde_json::from_value(body) {
        Ok(value) => Ok(value),
        Err(err) => match fallback {
            Some(fallback) => serde_json::from_value(fallback).map_err(|_| err.into()),
            None => Err(err.into()),
        },
    }
}

impl Requester {
    pub(crate) fn new(
        id: DebugAdapterId,
        server_tx: UnboundedSender<Payload>,
        request_counter: Arc<AtomicU64>,
        supports_configuration_done: bool,
    ) -> Self {
        Self {
            id,
            server_tx,
            request_counter,
            supports_configuration_done,
        }
    }

    /// The id of the debug adapter this handle talks to.
    pub fn id(&self) -> DebugAdapterId {
        self.id
    }

    fn next_request_id(&self) -> u64 {
        // The `seq` of the first message is 1, then 1 greater each time.
        self.request_counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Executes a request and returns the raw body of the response (`null` if it has none).
    pub fn call<R: vortex_dap_types::Request + 'static>(
        &self,
        arguments: R::Arguments,
    ) -> impl Future<Output = Result<Value>> + Send + 'static {
        let server_tx = self.server_tx.clone();
        let id = self.next_request_id();
        let arguments = serde_json::to_value(arguments).map_err(Error::from);

        async move {
            let arguments = Some(arguments?);
            let (callback_tx, mut callback_rx) = tokio::sync::mpsc::channel(1);

            let req = TransportRequest {
                back_ch: Some(callback_tx),
                seq: id,
                command: R::COMMAND.to_string(),
                arguments,
            };

            server_tx
                .send(Payload::Request(req))
                .map_err(|_| Error::StreamClosed)?;

            let response: Response = timeout(REQUEST_TIMEOUT, callback_rx.recv())
                .await
                .map_err(|_| Error::Timeout(id))?
                .ok_or(Error::StreamClosed)??;

            if !response.success {
                let message = response
                    .message
                    .clone()
                    .unwrap_or_else(|| "DAP request failed".to_string());
                return Err(Error::Other(anyhow!(message)));
            }

            Ok(response.body.unwrap_or_default())
        }
    }

    /// Executes a request and parses the body of the response.
    pub fn request<R: vortex_dap_types::Request + 'static>(
        &self,
        arguments: R::Arguments,
    ) -> impl Future<Output = Result<R::Result>> + Send + 'static {
        let call = self.call::<R>(arguments);
        async move { parse_body(call.await?) }
    }

    pub fn threads(&self) -> impl Future<Output = Result<Vec<Thread>>> + Send + 'static {
        let request = self.request::<requests::Threads>(Some(requests::ThreadsArguments {}));
        async move { Ok(request.await?.threads) }
    }

    /// The stack frames of a thread and the total number of frames, if the adapter says.
    pub fn stack_trace(
        &self,
        thread_id: ThreadId,
    ) -> impl Future<Output = Result<(Vec<StackFrame>, Option<usize>)>> + Send + 'static {
        let request = self.request::<requests::StackTrace>(requests::StackTraceArguments {
            thread_id,
            start_frame: None,
            levels: None,
            format: None,
        });
        async move {
            let response = request.await?;
            Ok((response.stack_frames, response.total_frames))
        }
    }

    pub fn scopes(
        &self,
        frame_id: usize,
    ) -> impl Future<Output = Result<Vec<Scope>>> + Send + 'static {
        let request = self.request::<requests::Scopes>(requests::ScopesArguments { frame_id });
        async move { Ok(request.await?.scopes) }
    }

    pub fn variables(
        &self,
        variables_reference: usize,
    ) -> impl Future<Output = Result<Vec<Variable>>> + Send + 'static {
        let request = self.request::<requests::Variables>(requests::VariablesArguments {
            variables_reference,
            filter: None,
            start: None,
            count: None,
            format: None,
        });
        async move { Ok(request.await?.variables) }
    }

    /// Evaluates an expression. `context` is sent as is: "repl", "watch", "hover"...
    pub fn evaluate(
        &self,
        expression: String,
        frame_id: Option<usize>,
        context: Option<&str>,
    ) -> impl Future<Output = Result<requests::EvaluateResponse>> + Send + 'static {
        self.request::<requests::Evaluate>(requests::EvaluateArguments {
            expression,
            frame_id,
            context: context.map(str::to_owned),
            format: None,
        })
    }

    pub fn set_variable(
        &self,
        variables_reference: usize,
        name: String,
        value: String,
    ) -> impl Future<Output = Result<requests::SetVariableResponse>> + Send + 'static {
        self.request::<requests::SetVariable>(requests::SetVariableArguments {
            variables_reference,
            name,
            value,
            format: None,
        })
    }

    pub fn set_expression(
        &self,
        expression: String,
        value: String,
        frame_id: Option<usize>,
    ) -> impl Future<Output = Result<requests::SetExpressionResponse>> + Send + 'static {
        self.request::<requests::SetExpression>(requests::SetExpressionArguments {
            expression,
            value,
            frame_id,
            format: None,
        })
    }

    pub fn continue_thread(
        &self,
        thread_id: ThreadId,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::Continue>(requests::ContinueArguments {
            thread_id,
            single_thread: None,
        });
        async move { call.await.map(drop) }
    }

    pub fn next(&self, thread_id: ThreadId) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::Next>(requests::NextArguments {
            thread_id,
            granularity: None,
            single_thread: None,
        });
        async move { call.await.map(drop) }
    }

    pub fn step_in(
        &self,
        thread_id: ThreadId,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::StepIn>(requests::StepInArguments {
            thread_id,
            target_id: None,
            granularity: None,
            single_thread: None,
        });
        async move { call.await.map(drop) }
    }

    pub fn step_out(
        &self,
        thread_id: ThreadId,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::StepOut>(requests::StepOutArguments {
            thread_id,
            granularity: None,
            single_thread: None,
        });
        async move { call.await.map(drop) }
    }

    /// Sends next / stepIn / stepOut for `thread_id`. `single_thread` is sent only when true.
    pub fn step(
        &self,
        kind: StepKind,
        thread_id: ThreadId,
        granularity: Option<SteppingGranularity>,
        single_thread: bool,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let single_thread = single_thread.then_some(true);
        let call: std::pin::Pin<Box<dyn Future<Output = Result<Value>> + Send>> = match kind {
            StepKind::Over => Box::pin(self.call::<requests::Next>(requests::NextArguments {
                thread_id,
                granularity,
                single_thread,
            })),
            StepKind::In => Box::pin(self.call::<requests::StepIn>(requests::StepInArguments {
                thread_id,
                target_id: None,
                granularity,
                single_thread,
            })),
            StepKind::Out => Box::pin(self.call::<requests::StepOut>(requests::StepOutArguments {
                thread_id,
                granularity,
                single_thread,
            })),
        };
        async move { call.await.map(drop) }
    }

    /// `continue` for one thread; returns the response's allThreadsContinued (None when the
    /// adapter didn't say).
    pub fn continue_with(
        &self,
        thread_id: ThreadId,
        single_thread: bool,
    ) -> impl Future<Output = Result<Option<bool>>> + Send + 'static {
        let request = self.request::<requests::Continue>(requests::ContinueArguments {
            thread_id,
            single_thread: single_thread.then_some(true),
        });
        async move { Ok(request.await?.all_threads_continued) }
    }

    /// `variables` with paging: at most `count` children starting at `start` (both None: all).
    pub fn variables_page(
        &self,
        reference: usize,
        start: Option<usize>,
        count: Option<usize>,
    ) -> impl Future<Output = Result<Vec<Variable>>> + Send + 'static {
        let request = self.request::<requests::Variables>(requests::VariablesArguments {
            variables_reference: reference,
            filter: None,
            start,
            count,
            format: None,
        });
        async move { Ok(request.await?.variables) }
    }

    pub fn pause(&self, thread_id: ThreadId) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::Pause>(requests::PauseArguments { thread_id });
        async move { call.await.map(drop) }
    }

    /// Sets the breakpoints of a file, replacing the ones it had.
    pub fn set_breakpoints(
        &self,
        path: PathBuf,
        breakpoints: Vec<SourceBreakpoint>,
    ) -> impl Future<Output = Result<Option<Vec<Breakpoint>>>> + Send + 'static {
        let request = self.request::<requests::SetBreakpoints>(requests::SetBreakpointsArguments {
            source: Source {
                path: Some(path),
                name: None,
                source_reference: None,
                presentation_hint: None,
                origin: None,
                sources: None,
                adapter_data: None,
                checksums: None,
            },
            breakpoints: Some(breakpoints),
            source_modified: Some(false),
        });
        async move { Ok(request.await?.breakpoints) }
    }

    pub fn set_exception_breakpoints(
        &self,
        filters: Vec<String>,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::SetExceptionBreakpoints>(
            requests::SetExceptionBreakpointsArguments { filters },
        );
        async move { call.await.map(drop) }
    }

    /// Tells the adapter the configuration is complete. Does nothing when the adapter doesn't
    /// support the request.
    pub fn configuration_done(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.supports_configuration_done.then(|| {
            self.call::<requests::ConfigurationDone>(Some(requests::ConfigurationDoneArguments {}))
        });
        async move {
            if let Some(call) = call {
                call.await?;
            }
            Ok(())
        }
    }

    pub fn launch(&self, args: Value) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::Launch>(args);
        async move { call.await.map(drop) }
    }

    pub fn attach(&self, args: Value) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::Attach>(args);
        async move { call.await.map(drop) }
    }

    pub fn disconnect(
        &self,
        args: Option<DisconnectArguments>,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::Disconnect>(args);
        async move { call.await.map(drop) }
    }

    pub fn terminate(
        &self,
        args: Option<TerminateArguments>,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        let call = self.call::<requests::Terminate>(args);
        async move { call.await.map(drop) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Client;
    use serde_json::json;
    use tokio::io::{
        duplex, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream,
    };

    pub(crate) async fn read_message<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Value {
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.trim().strip_prefix("Content-Length: ") {
                length = value.parse().unwrap();
            }
        }
        let mut content = vec![0; length];
        reader.read_exact(&mut content).await.unwrap();
        serde_json::from_slice(&content).unwrap()
    }

    pub(crate) async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, message: Value) {
        let json = message.to_string();
        let framed = format!("Content-Length: {}\r\n\r\n{json}", json.len());
        writer.write_all(framed.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
    }

    #[test]
    fn a_missing_body_parses_as_nothing() {
        // A unit result and a struct with only optional fields.
        parse_body::<()>(Value::Null).unwrap();
        parse_body::<()>(json!({})).unwrap();
        let response: requests::SetExceptionBreakpointsResponse = parse_body(Value::Null).unwrap();
        assert_eq!(response.breakpoints, None);
        // A struct with required fields still fails.
        assert!(parse_body::<requests::ScopesResponse>(Value::Null).is_err());
        // A real body is not touched.
        let response: requests::SetExceptionBreakpointsResponse =
            parse_body(json!({"breakpoints": [{"verified": true}]})).unwrap();
        assert_eq!(response.breakpoints.unwrap().len(), 1);
    }

    fn tid(n: i64) -> ThreadId {
        serde_json::from_value(json!(n)).unwrap()
    }

    fn capturing_requester() -> (Requester, tokio::sync::mpsc::UnboundedReceiver<Payload>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let requester = Requester::new(
            DebugAdapterId::default(),
            tx,
            Arc::new(AtomicU64::new(0)),
            true,
        );
        (requester, rx)
    }

    /// Answers the next request with `body` and returns its command and arguments.
    async fn answer(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Payload>,
        body: Value,
    ) -> (String, Value) {
        let Some(Payload::Request(req)) = rx.recv().await else {
            panic!("no request")
        };
        let response = Response {
            request_seq: req.seq,
            success: true,
            message: None,
            command: req.command.clone(),
            body: Some(body),
        };
        req.back_ch.unwrap().send(Ok(response)).await.unwrap();
        (req.command, req.arguments.unwrap_or_default())
    }

    #[tokio::test]
    async fn step_continue_and_variables_page_send_their_arguments() {
        let (requester, mut rx) = capturing_requester();

        let fut = tokio::spawn(requester.step(
            StepKind::In,
            tid(4),
            Some(SteppingGranularity::Line),
            true,
        ));
        let (command, args) = answer(&mut rx, json!({})).await;
        fut.await.unwrap().unwrap();
        assert_eq!(command, "stepIn");
        assert_eq!(
            args,
            json!({"threadId": 4, "granularity": "line", "singleThread": true})
        );

        let fut = tokio::spawn(requester.step(StepKind::Over, tid(4), None, false));
        let (command, args) = answer(&mut rx, json!({})).await;
        fut.await.unwrap().unwrap();
        assert_eq!(command, "next");
        assert_eq!(args, json!({"threadId": 4}));

        let fut = tokio::spawn(requester.step(StepKind::Out, tid(4), None, true));
        let (command, args) = answer(&mut rx, json!({})).await;
        fut.await.unwrap().unwrap();
        assert_eq!(command, "stepOut");
        assert_eq!(args, json!({"threadId": 4, "singleThread": true}));

        let fut = tokio::spawn(requester.continue_with(tid(2), true));
        let (command, args) = answer(&mut rx, json!({"allThreadsContinued": false})).await;
        assert_eq!(fut.await.unwrap().unwrap(), Some(false));
        assert_eq!(command, "continue");
        assert_eq!(args, json!({"threadId": 2, "singleThread": true}));

        let fut = tokio::spawn(requester.continue_with(tid(2), false));
        let (_, args) = answer(&mut rx, json!({})).await;
        assert_eq!(fut.await.unwrap().unwrap(), None);
        assert_eq!(args, json!({"threadId": 2}));

        let fut = tokio::spawn(requester.variables_page(9, Some(10), Some(5)));
        let (command, args) = answer(
            &mut rx,
            json!({"variables": [{"name": "a", "value": "1", "variablesReference": 0}]}),
        )
        .await;
        assert_eq!(fut.await.unwrap().unwrap().len(), 1);
        assert_eq!(command, "variables");
        assert_eq!(
            args,
            json!({"variablesReference": 9, "start": 10, "count": 5})
        );

        let fut = tokio::spawn(requester.variables_page(9, None, None));
        let (_, args) = answer(&mut rx, json!({"variables": []})).await;
        fut.await.unwrap().unwrap();
        assert_eq!(args, json!({"variablesReference": 9}));
    }

    fn scripted_client() -> (Client, DuplexStream, DuplexStream) {
        let (client_writer, server_reader) = duplex(64 * 1024);
        let (server_writer, client_reader) = duplex(64 * 1024);
        let (client, _events) = Client::streams(
            Box::new(BufReader::new(client_reader)),
            Box::new(client_writer),
            None,
            DebugAdapterId::default(),
            None,
        )
        .unwrap();
        (client, server_reader, server_writer)
    }

    #[tokio::test]
    async fn a_requester_sends_requests_and_parses_the_responses() {
        let (client, server_reader, mut server_writer) = scripted_client();
        let requester = client.requester();
        let other = requester.clone();

        tokio::spawn(async move {
            let mut reader = BufReader::new(server_reader);
            for _ in 0..3 {
                let request = read_message(&mut reader).await;
                let seq = request["seq"].as_u64().unwrap();
                let body = match request["command"].as_str().unwrap() {
                    "evaluate" => {
                        assert_eq!(request["arguments"]["expression"], "a + b");
                        assert_eq!(request["arguments"]["frameId"], 7);
                        assert_eq!(request["arguments"]["context"], "watch");
                        // No variablesReference.
                        json!({"result": "3", "type": "int"})
                    }
                    "threads" => json!({"threads": [{"id": 1, "name": "main"}]}),
                    // No body at all.
                    "setExceptionBreakpoints" => Value::Null,
                    other => panic!("unexpected request {other}"),
                };
                let mut response = json!({
                    "type": "response",
                    "request_seq": seq,
                    "success": true,
                    "command": request["command"],
                });
                if !body.is_null() {
                    response["body"] = body;
                }
                write_message(&mut server_writer, response).await;
            }
        });

        let result = requester
            .evaluate("a + b".into(), Some(7), Some("watch"))
            .await
            .unwrap();
        assert_eq!(result.result, "3");
        assert_eq!(result.ty.as_deref(), Some("int"));
        assert_eq!(result.variables_reference, 0);

        let threads = other.threads().await.unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].name, "main");

        other.set_exception_breakpoints(vec![]).await.unwrap();
        // The adapter doesn't support it (no capabilities): nothing is sent.
        requester.configuration_done().await.unwrap();
    }

    #[tokio::test]
    async fn a_request_fails_when_the_stream_closes() {
        let (client, server_reader, server_writer) = scripted_client();
        let requester = client.requester();
        drop(server_reader);
        drop(server_writer);
        assert!(requester.threads().await.is_err());
    }
}
