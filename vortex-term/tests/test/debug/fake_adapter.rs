//! A scripted fake Debug Adapter Protocol adapter for the integration tests.
//!
//! [`FakeAdapter::start`] binds `127.0.0.1:0` and serves any number of connections (sessions,
//! numbered from 0 in accept order) on tokio tasks of the test process. The editor's DAP client
//! connects to `addr` as if to a TCP adapter. The "program" is described by a [`Script`].
//!
//! # Rules
//!
//! * Handshake: `initialize` -> response (capabilities from the script), then an `initialized`
//!   event. `launch`/`attach` is answered only after `configurationDone` (immediately when the
//!   script says `configuration_done(false)`, or when `configurationDone` came first).
//!   `configurationDone` is answered before the pending launch.
//! * After the launch response of a program session: `process` event, `thread` started event,
//!   the script's `output` events, then the program runs: it stops at the first stop that "hits".
//! * A stop hits when its reason is not `"breakpoint"` (always), or when it is a `"breakpoint"`
//!   stop and the session has a breakpoint on the stop's line in the file of its top frame (paths
//!   compare equal, or have the same file name). A breakpoint with a `logMessage` never stops: it
//!   sends an `output` event (category `console`, text + `\n`). A breakpoint with a `condition`
//!   is skipped when the condition evaluates (as `evaluate` does, top frame) to `false` or `0`;
//!   conditions that cannot be evaluated hit. Hit counts are ignored. Stops that do not hit are
//!   passed over. Stops are consumed in order and never revisited.
//! * `continue`: respond, `continued`, then the next hitting stop or the exit sequence.
//!   `next`/`stepIn`/`stepOut` (when `stepping(true)`, the default): the very next stop, whatever
//!   its reason or breakpoints, reported with reason `"step"`; with `stepping(false)` they act as
//!   `continue`. `pause`: respond, then the next stop with reason `"pause"` (ignored if none).
//! * Past the last stop the program exits: `exited {exitCode}` then `terminated`; or never, with
//!   `run_forever()` (then only `terminate`/`disconnect` end it). `terminate` runs the exit
//!   sequence at once. `disconnect` answers and closes the connection.
//! * References: frame ids and `variablesReference`s are numbered per stop and change at every
//!   stop, even when the program is stopped at the same place again. A frame id or reference from
//!   an earlier stop is answered with an error ("unknown frameId" / "unknown variablesReference").
//!   Scope and variable references are non-zero only for scopes and for variables with children.
//! * `setVariable`/`setExpression` (answered "unsupported" unless the script enables them) change
//!   the value in the current stop; the new value is also kept for the same scope/name path in
//!   every later stop of that session.
//! * `evaluate`: `1+2` -> `3`; otherwise the expression is a name (`a.b` for children) looked up
//!   in the scopes of the frame (frame 0 without `frameId`); else error "cannot evaluate". The
//!   request (with its `context`) is in the request log like every other request.
//! * Memory: `Script::read_memory()` / `Script::write_memory()` advertise
//!   `supportsReadMemoryRequest` / `supportsWriteMemoryRequest` (and enable the requests; without
//!   them they are "unsupported"). The simulated memory is a sparse map of readable bytes: the
//!   regions of `Script::memory(address, bytes)`, everything else is unreadable. A
//!   `memoryReference` is a hex address (`0x1000`); `offset` is added to it (it may be negative).
//!   `readMemory` answers `address` (the first readable byte of the range asked for), `data`
//!   (base64: the readable bytes from there, up to the end of their region or of the range) and
//!   `unreadableBytes` (the unreadable bytes of the range after the data). A range without a
//!   readable byte is answered with `address` = the range's start, no `data`, and
//!   `unreadableBytes` = `count`; with `Script::read_errors()` it fails instead ("cannot read
//!   memory"). `writeMemory` writes the bytes, all or nothing: a byte outside the readable
//!   memory fails it ("cannot write memory"). It answers `bytesWritten` (the number of bytes
//!   sent, or `Script::bytes_written(n)` whatever it was). `Var::memory(reference)` gives a
//!   variable a `memoryReference` (in `variables` and in `evaluate` of its name).
//!   `Stop::poke(address, bytes)` changes memory when the program reaches that stop, just before
//!   `stopped`, so `continue`/`next` change what the next read shows. The memory is shared by
//!   all the connections; tests look at it with `FakeAdapter::memory_at` and change it with
//!   `FakeAdapter::set_memory`, and at the requests with `FakeAdapter::memory_reads` /
//!   `FakeAdapter::memory_writes` (absolute addresses).
//! * `setExceptionBreakpoints` is answered WITHOUT a body (like lldb-dap).
//! * Unknown requests: `success: false`, message `"unsupported"`.
//! * Delays: `Stop::delay(ms)`. When a `continue`/`next`/`stepIn`/`stepOut` reaches such a stop,
//!   the program "runs" for that long (frames, scopes and variables answer as for a running
//!   program: nothing) and only then sends `stopped`. A `pause` request arriving meanwhile
//!   stops at once at that same stop, with reason `"pause"`, and consumes it. The stops of a
//!   `pause` itself are never delayed.
//! * `allThreadsStopped` in `stopped` events is true, or false with `Script::non_stop()`.
//!   `continue`'s response and the `continued` event say `allThreadsContinued: true`, or false
//!   when the request carried `singleThread: true`.
//! * `Stop::keep_reason()`: when a step reaches this stop it reports the stop's own reason (not
//!   `"step"`), without needing a breakpoint.
//! * The `process` event carries the test process's own pid as `systemProcessId` (so reading
//!   the CPU time works).
//! * Thread events: `Stop::thread_started(id, name)` / `Stop::thread_exited(id)` send a `thread`
//!   event just before that stop's `stopped`. The `threads` response lists the threads alive at
//!   that point: `main` (and `worker` with `worker_thread()`), plus the started ones minus the
//!   exited ones.
//! * Thread mode (`Script::threads()`, `Script::thread_program(id, stops)`): every thread has
//!   a queue of stops of its own (their `thread` is forced to the thread's id) and the single
//!   queue of `Script::stop` is not used. The threads are `1 main`, `2 worker`, `N thread N`.
//!   At launch every thread runs to its first stop that hits, in id order; the lowest sends
//!   `stopped` and the others stay stopped silently, as in an all-stop session (with
//!   `non_stop()` each sends its own `stopped`, those of the threads after the first with
//!   `preserveFocusHint: true`). Every thread that is stopped answers
//!   `stackTrace`, `scopes` and `variables` for its own stop. A `continue`/`next`/`stepIn`/
//!   `stepOut` for thread T advances only T's queue, with `singleThread` or without it (the
//!   request, with its `singleThread`, is in the log): so tests assert on the log and on what
//!   each queue did (`FakeAdapter::stops_sent`). A thread whose queue runs out exits (a
//!   `thread` exited event); the program exits when the last thread has. A thread that isn't
//!   stopped (it is waiting out a delay, or it exited) answers `continue`/`next`... with
//!   "not stopped".
//! * Collections: `Var::many(n)` adds `n` leaf children `[0]`..`[n-1]`. `variables` honours
//!   `start` and `count` (the children `start..start+count`). A `Var::indexed()` variable
//!   also says `indexedVariables: <number of children>` in the `variables` response of its
//!   container, the way adapters announce the length of a collection.
//! * `child()` mode (like js-debug): connection 0 is the parent: it has no thread and no stops;
//!   after the launch response it sends the reverse request `startDebugging` (launch, name
//!   `child [1]`, `__pendingTargetId` `t1`). Every later connection is a child and the program:
//!   its `initialized` event comes twice (after `initialize`, and after the launch response), and
//!   at exit it sends no `exited` but an `output` (stderr, `Process exited with code N`), then
//!   `terminated`, and then the parent sends `terminated`.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

// ---------------------------------------------------------------------------------------------
// The script
// ---------------------------------------------------------------------------------------------

/// A variable of a scope: a tree through `children`.
#[derive(Clone, Debug)]
pub struct Var {
    pub name: String,
    pub value: String,
    pub ty: Option<String>,
    pub children: Vec<Var>,
    pub indexed: bool,
    pub memory_reference: Option<String>,
}

impl Var {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Var {
        Var {
            name: name.into(),
            value: value.into(),
            ty: None,
            children: Vec::new(),
            indexed: false,
            memory_reference: None,
        }
    }
    /// The value points to memory: its `memoryReference` (a hex address like `0x1000`).
    pub fn memory(mut self, reference: impl Into<String>) -> Var {
        self.memory_reference = Some(reference.into());
        self
    }
    /// Announces the number of children as `indexedVariables`.
    pub fn indexed(mut self) -> Var {
        self.indexed = true;
        self
    }
    /// `n` leaf children `[0]`..`[n-1]` with the values `0`..`n-1`.
    pub fn many(mut self, n: usize) -> Var {
        for i in 0..n {
            self.children
                .push(Var::new(format!("[{i}]"), i.to_string()));
        }
        self
    }
    pub fn ty(mut self, ty: impl Into<String>) -> Var {
        self.ty = Some(ty.into());
        self
    }
    pub fn child(mut self, child: Var) -> Var {
        self.children.push(child);
        self
    }
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub name: String,
    pub vars: Vec<Var>,
    pub expensive: bool,
}

impl Scope {
    pub fn new(name: impl Into<String>) -> Scope {
        Scope {
            name: name.into(),
            vars: Vec::new(),
            expensive: false,
        }
    }
    pub fn var(mut self, var: Var) -> Scope {
        self.vars.push(var);
        self
    }
    pub fn expensive(mut self) -> Scope {
        self.expensive = true;
        self
    }
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub name: String,
    pub path: String,
    pub line: u64,
    pub scopes: Vec<Scope>,
}

impl Frame {
    pub fn new(name: impl Into<String>, path: impl Into<String>, line: u64) -> Frame {
        Frame {
            name: name.into(),
            path: path.into(),
            line,
            scopes: Vec::new(),
        }
    }
    pub fn scope(mut self, scope: Scope) -> Frame {
        self.scopes.push(scope);
        self
    }
}

/// One stop of the program. `scope()` adds scopes to the top frame; frames deeper in the stack
/// get theirs with [`Frame::scope`] and [`Stop::frame_with`]. Without frames, the stop gets one
/// frame `main` at the script's source and the stop's line.
#[derive(Clone, Debug)]
pub struct Stop {
    pub line: u64,
    pub reason: String,
    pub thread: i64,
    pub frames: Vec<Frame>,
    pub scopes: Vec<Scope>,
    pub delay_ms: u64,
    pub events: Vec<ThreadEvent>,
    pub keep_reason: bool,
    /// Memory written when the program reaches the stop.
    pub pokes: Vec<(u64, Vec<u8>)>,
}

/// A `thread` event sent just before a stop's `stopped`.
#[derive(Clone, Debug)]
pub enum ThreadEvent {
    Started(i64, String),
    Exited(i64),
}

impl Stop {
    pub fn new(line: u64, reason: impl Into<String>) -> Stop {
        Stop {
            line,
            reason: reason.into(),
            thread: 1,
            frames: Vec::new(),
            scopes: Vec::new(),
            delay_ms: 0,
            events: Vec::new(),
            keep_reason: false,
            pokes: Vec::new(),
        }
    }
    /// The program changes memory (`bytes` at `address`) on its way to this stop.
    pub fn poke(mut self, address: u64, bytes: impl AsRef<[u8]>) -> Stop {
        self.pokes.push((address, bytes.as_ref().to_vec()));
        self
    }
    /// Reached by a step, this stop reports its own reason instead of `"step"` (like stepping
    /// onto a line with a breakpoint, which a real adapter reports as `"breakpoint"`).
    pub fn keep_reason(mut self) -> Stop {
        self.keep_reason = true;
        self
    }
    /// The program runs this long before this stop is reported.
    pub fn delay(mut self, ms: u64) -> Stop {
        self.delay_ms = ms;
        self
    }
    /// A `thread` started event for thread `id` just before this stop's `stopped`.
    pub fn thread_started(mut self, id: i64, name: impl Into<String>) -> Stop {
        self.events.push(ThreadEvent::Started(id, name.into()));
        self
    }
    /// A `thread` exited event for thread `id` just before this stop's `stopped`.
    pub fn thread_exited(mut self, id: i64) -> Stop {
        self.events.push(ThreadEvent::Exited(id));
        self
    }
    pub fn thread(mut self, id: i64) -> Stop {
        self.thread = id;
        self
    }
    pub fn frame(mut self, name: impl Into<String>, path: impl Into<String>, line: u64) -> Stop {
        self.frames.push(Frame::new(name, path, line));
        self
    }
    pub fn frame_with(mut self, frame: Frame) -> Stop {
        self.frames.push(frame);
        self
    }
    pub fn scope(mut self, scope: Scope) -> Stop {
        self.scopes.push(scope);
        self
    }
}

/// What the fake program does. See the module documentation for the rules.
#[derive(Clone, Debug)]
pub struct Script {
    source: String,
    stops: Vec<Stop>,
    stepping: bool,
    exit_code: i64,
    run_forever: bool,
    outputs: Vec<(String, String)>,
    configuration_done: bool,
    set_variable: bool,
    set_expression: bool,
    conditional_breakpoints: bool,
    log_points: bool,
    exception_filters: Vec<(String, String)>,
    extra_capabilities: Vec<(String, Value)>,
    worker: bool,
    child: bool,
    all_stopped: bool,
    thread_mode: bool,
    programs: Vec<(i64, Vec<Stop>)>,
    read_memory: bool,
    write_memory: bool,
    read_errors: bool,
    bytes_written: Option<usize>,
    memory: Vec<(u64, Vec<u8>)>,
}

impl Script {
    /// `source`: the path of the program's source file (used for stops without frames).
    pub fn new(source: impl Into<String>) -> Script {
        Script {
            source: source.into(),
            stops: Vec::new(),
            stepping: true,
            exit_code: 0,
            run_forever: false,
            outputs: Vec::new(),
            configuration_done: true,
            set_variable: false,
            set_expression: false,
            conditional_breakpoints: false,
            log_points: false,
            exception_filters: Vec::new(),
            extra_capabilities: Vec::new(),
            worker: false,
            child: false,
            all_stopped: true,
            thread_mode: false,
            programs: Vec::new(),
            read_memory: false,
            write_memory: false,
            read_errors: false,
            bytes_written: None,
            memory: Vec::new(),
        }
    }
    /// `supportsReadMemoryRequest`: `readMemory` works.
    pub fn read_memory(mut self) -> Script {
        self.read_memory = true;
        self
    }
    /// `supportsWriteMemoryRequest`: `writeMemory` works.
    pub fn write_memory(mut self) -> Script {
        self.write_memory = true;
        self
    }
    /// `bytes` are readable (and writable) from `address`.
    pub fn memory(mut self, address: u64, bytes: impl AsRef<[u8]>) -> Script {
        self.memory.push((address, bytes.as_ref().to_vec()));
        self
    }
    /// `len` readable bytes from `address`, the byte at `address + i` being `f(i)`.
    pub fn memory_with(self, address: u64, len: usize, f: impl Fn(usize) -> u8) -> Script {
        let bytes: Vec<u8> = (0..len).map(f).collect();
        self.memory(address, bytes)
    }
    /// `readMemory` of a range with nothing readable fails instead of saying `unreadableBytes`.
    pub fn read_errors(mut self) -> Script {
        self.read_errors = true;
        self
    }
    /// `writeMemory` always answers `bytesWritten: n`.
    pub fn bytes_written(mut self, n: usize) -> Script {
        self.bytes_written = Some(n);
        self
    }
    /// Gives a stop its default frame and moves its scopes to the top frame.
    fn normalize(&self, mut stop: Stop) -> Stop {
        if stop.frames.is_empty() {
            stop.frames
                .push(Frame::new("main", self.source.clone(), stop.line));
        }
        let scopes = std::mem::take(&mut stop.scopes);
        stop.frames[0].scopes.splice(0..0, scopes);
        stop
    }
    pub fn stop(mut self, stop: Stop) -> Script {
        let stop = self.normalize(stop);
        self.stops.push(stop);
        self
    }
    /// `stopped` events say `allThreadsStopped: false`.
    pub fn non_stop(mut self) -> Script {
        self.all_stopped = false;
        self
    }
    /// Thread mode: every thread has a queue of stops of its own (see the module documentation).
    pub fn threads(mut self) -> Script {
        self.thread_mode = true;
        self
    }
    /// Thread mode: the stops of thread `id` (their `thread` becomes `id`).
    pub fn thread_program(mut self, id: i64, stops: Vec<Stop>) -> Script {
        self.thread_mode = true;
        let stops: Vec<Stop> = stops
            .into_iter()
            .map(|mut stop| {
                stop.thread = id;
                self.normalize(stop)
            })
            .collect();
        match self.programs.iter_mut().find(|(i, _)| *i == id) {
            Some((_, list)) => list.extend(stops),
            None => {
                self.programs.push((id, stops));
                self.programs.sort_by_key(|(i, _)| *i);
            }
        }
        self
    }
    fn program(&self, id: i64) -> &[Stop] {
        self.programs
            .iter()
            .find(|(i, _)| *i == id)
            .map_or(&[], |(_, stops)| stops.as_slice())
    }
    /// The threads alive at the start.
    fn initial_threads(&self) -> Vec<(i64, String)> {
        let name = |id: i64| match id {
            1 => "main".to_string(),
            2 => "worker".to_string(),
            n => format!("thread {n}"),
        };
        if self.thread_mode {
            return self
                .programs
                .iter()
                .map(|(id, _)| (*id, name(*id)))
                .collect();
        }
        let mut threads = vec![(1, name(1))];
        if self.worker {
            threads.push((2, name(2)));
        }
        threads
    }
    /// Whether `next`/`stepIn`/`stepOut` advance to the next stop (default true).
    pub fn stepping(mut self, on: bool) -> Script {
        self.stepping = on;
        self
    }
    /// The exit code reported when the program runs past the last stop (default 0).
    pub fn exit_code(mut self, code: i64) -> Script {
        self.exit_code = code;
        self
    }
    /// The program never exits by itself after its last stop.
    pub fn run_forever(mut self) -> Script {
        self.run_forever = true;
        self
    }
    /// An `output` event sent when the program starts.
    pub fn output(mut self, category: impl Into<String>, text: impl Into<String>) -> Script {
        self.outputs.push((category.into(), text.into()));
        self
    }
    /// `supportsConfigurationDoneRequest` (default true). When false the launch is answered at
    /// once and the program starts without waiting for `configurationDone`.
    pub fn configuration_done(mut self, on: bool) -> Script {
        self.configuration_done = on;
        self
    }
    pub fn set_variable(mut self) -> Script {
        self.set_variable = true;
        self
    }
    pub fn set_expression(mut self) -> Script {
        self.set_expression = true;
        self
    }
    pub fn conditional_breakpoints(mut self) -> Script {
        self.conditional_breakpoints = true;
        self
    }
    pub fn log_points(mut self) -> Script {
        self.log_points = true;
        self
    }
    pub fn exception_filter(mut self, id: impl Into<String>, label: impl Into<String>) -> Script {
        self.exception_filters.push((id.into(), label.into()));
        self
    }
    /// Any other capability flag, advertised as is.
    pub fn capability(mut self, key: impl Into<String>, value: Value) -> Script {
        self.extra_capabilities.push((key.into(), value));
        self
    }
    /// A second thread (id 2, "worker") in `threads`.
    pub fn worker_thread(mut self) -> Script {
        self.worker = true;
        self
    }
    /// Imitate js-debug: a parent session and a child session that runs the program.
    pub fn child(mut self) -> Script {
        self.child = true;
        self
    }

    fn capabilities(&self) -> Value {
        let mut caps = serde_json::Map::new();
        caps.insert(
            "supportsConfigurationDoneRequest".into(),
            self.configuration_done.into(),
        );
        caps.insert("supportsSetVariable".into(), self.set_variable.into());
        caps.insert("supportsSetExpression".into(), self.set_expression.into());
        caps.insert(
            "supportsConditionalBreakpoints".into(),
            self.conditional_breakpoints.into(),
        );
        caps.insert("supportsLogPoints".into(), self.log_points.into());
        caps.insert("supportsReadMemoryRequest".into(), self.read_memory.into());
        caps.insert(
            "supportsWriteMemoryRequest".into(),
            self.write_memory.into(),
        );
        let filters: Vec<Value> = self
            .exception_filters
            .iter()
            .map(|(id, label)| json!({"filter": id, "label": label}))
            .collect();
        caps.insert("exceptionBreakpointFilters".into(), filters.into());
        for (k, v) in &self.extra_capabilities {
            caps.insert(k.clone(), v.clone());
        }
        Value::Object(caps)
    }
}

// ---------------------------------------------------------------------------------------------
// The public handle
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Logged {
    session: usize,
    command: String,
    arguments: Value,
}

#[derive(Clone, Debug)]
struct ClientResponse {
    session: usize,
    command: String,
    success: bool,
    body: Value,
}

struct Shared {
    script: Script,
    log: Mutex<Vec<Logged>>,
    responses: Mutex<Vec<ClientResponse>>,
    next_index: AtomicUsize,
    parent_out: Mutex<Option<Arc<Out>>>,
    /// Every `stopped` event sent: (thread, line of the top frame, reason).
    sent: Mutex<Vec<(i64, u64, String)>>,
    /// The readable bytes of the simulated memory.
    memory: Mutex<BTreeMap<u64, u8>>,
}

pub struct FakeAdapter {
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeAdapter {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeAdapter {
    /// Binds `127.0.0.1:0` and serves connections until dropped.
    pub async fn start(script: Script) -> Result<FakeAdapter> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let mut memory = BTreeMap::new();
        for (address, bytes) in &script.memory {
            for (i, byte) in bytes.iter().enumerate() {
                memory.insert(address + i as u64, *byte);
            }
        }
        let shared = Arc::new(Shared {
            script,
            log: Mutex::new(Vec::new()),
            responses: Mutex::new(Vec::new()),
            next_index: AtomicUsize::new(0),
            parent_out: Mutex::new(None),
            sent: Mutex::new(Vec::new()),
            memory: Mutex::new(memory),
        });
        let sh = shared.clone();
        let task = tokio::spawn(async move {
            let mut conns = JoinSet::new();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let index = sh.next_index.fetch_add(1, Ordering::SeqCst);
                conns.spawn(serve(stream, sh.clone(), index));
            }
        });
        Ok(FakeAdapter { addr, shared, task })
    }

    /// Every request received, in order: (session index, command, arguments).
    pub fn requests(&self) -> Vec<(usize, String, Value)> {
        self.shared
            .log
            .lock()
            .unwrap()
            .iter()
            .map(|l| (l.session, l.command.clone(), l.arguments.clone()))
            .collect()
    }

    pub fn requests_named(&self, command: &str) -> Vec<(usize, Value)> {
        self.requests()
            .into_iter()
            .filter(|(_, c, _)| c == command)
            .map(|(s, _, a)| (s, a))
            .collect()
    }

    /// The responses the client sent to reverse requests (`startDebugging`):
    /// (session index, command, success, body).
    pub fn client_responses(&self) -> Vec<(usize, String, bool, Value)> {
        self.shared
            .responses
            .lock()
            .unwrap()
            .iter()
            .map(|r| (r.session, r.command.clone(), r.success, r.body.clone()))
            .collect()
    }

    /// Every `stopped` event sent, in order: (thread, line of its top frame, reason).
    pub fn stops_sent(&self) -> Vec<(i64, u64, String)> {
        self.shared.sent.lock().unwrap().clone()
    }

    /// The `len` bytes of the simulated memory from `address` (`None`: unreadable).
    pub fn memory_at(&self, address: u64, len: usize) -> Vec<Option<u8>> {
        let memory = self.shared.memory.lock().unwrap();
        (0..len as u64)
            .map(|i| memory.get(&(address + i)).copied())
            .collect()
    }

    /// Changes the simulated memory, as the program would: the bytes become readable.
    pub fn set_memory(&self, address: u64, bytes: &[u8]) {
        let mut memory = self.shared.memory.lock().unwrap();
        for (i, byte) in bytes.iter().enumerate() {
            memory.insert(address + i as u64, *byte);
        }
    }

    /// Makes `len` bytes from `address` unreadable.
    pub fn unmap_memory(&self, address: u64, len: usize) {
        let mut memory = self.shared.memory.lock().unwrap();
        for i in 0..len as u64 {
            memory.remove(&(address + i));
        }
    }

    /// The `readMemory` requests received, in order: (absolute address, count).
    pub fn memory_reads(&self) -> Vec<(u64, usize)> {
        self.requests_named("readMemory")
            .into_iter()
            .filter_map(|(_, args)| memory_range(&args).map(|(address, count)| (address, count)))
            .collect()
    }

    /// The `writeMemory` requests received, in order: (absolute address, bytes).
    pub fn memory_writes(&self) -> Vec<(u64, Vec<u8>)> {
        self.requests_named("writeMemory")
            .into_iter()
            .filter_map(|(_, args)| {
                let (address, _) = memory_range(&args)?;
                Some((address, base64_decode(args["data"].as_str()?)?))
            })
            .collect()
    }

    /// How many stops of `thread` were reported so far (how far its queue advanced).
    pub fn advanced(&self, thread: i64) -> usize {
        self.stops_sent().iter().filter(|s| s.0 == thread).count()
    }

    /// Number of connections accepted so far.
    pub fn connections(&self) -> usize {
        self.shared.next_index.load(Ordering::SeqCst)
    }

    /// Waits until a request named `command` has been received; returns the first one.
    pub async fn wait_for_request(
        &self,
        command: &str,
        deadline: Duration,
    ) -> Result<(usize, Value)> {
        self.wait_for_request_n(command, 1, deadline).await
    }

    /// Waits until `n` requests named `command` have been received; returns the n-th.
    pub async fn wait_for_request_n(
        &self,
        command: &str,
        n: usize,
        deadline: Duration,
    ) -> Result<(usize, Value)> {
        let start = std::time::Instant::now();
        loop {
            let found = self.requests_named(command);
            if found.len() >= n {
                return Ok(found[n - 1].clone());
            }
            if start.elapsed() > deadline {
                bail!(
                    "timed out waiting for request {command} #{n}; got {:?}",
                    self.requests()
                        .iter()
                        .map(|(s, c, _)| format!("{s}:{c}"))
                        .collect::<Vec<_>>()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Memory helpers
// ---------------------------------------------------------------------------------------------

/// A `memoryReference` (a hex address, or a decimal one) as a number.
fn parse_reference(text: &str) -> Option<u64> {
    let text = text.trim();
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// The absolute address and the `count` (the length of `data`, for `writeMemory`) of a
/// `readMemory` or `writeMemory` request.
fn memory_range(args: &Value) -> Option<(u64, usize)> {
    let base = parse_reference(args["memoryReference"].as_str()?)?;
    let offset = args["offset"].as_i64().unwrap_or(0);
    let count = args["count"].as_u64().unwrap_or(0) as usize;
    Some((base.wrapping_add(offset as u64), count))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut n, mut bits) = (0u32, 0);
    for c in text.trim().trim_end_matches('=').bytes() {
        let value = B64.iter().position(|b| *b == c)? as u32;
        n = n << 6 | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
            n &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------------------------

/// Content-Length framing: feed it bytes as they arrive, get the complete messages.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Value>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some(end) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = std::str::from_utf8(&self.buf[..end])?;
            let len: usize = header
                .lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.trim().parse().ok())
                })
                .ok_or_else(|| anyhow!("no Content-Length in header {header:?}"))?;
            let start = end + 4;
            if self.buf.len() < start + len {
                break;
            }
            out.push(serde_json::from_slice(&self.buf[start..start + len])?);
            self.buf.drain(..start + len);
        }
        Ok(out)
    }
}

pub fn encode(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).unwrap();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

// ---------------------------------------------------------------------------------------------
// Output side of a connection: seq numbers and ordering
// ---------------------------------------------------------------------------------------------

enum Msg {
    Json(Value),
    Close,
}

struct Out {
    tx: mpsc::UnboundedSender<Msg>,
    seq: Mutex<i64>,
}

impl Out {
    fn spawn(mut wr: OwnedWriteHalf) -> Arc<Out> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                match m {
                    Msg::Json(v) => {
                        if wr.write_all(&encode(&v)).await.is_err() {
                            break;
                        }
                    }
                    Msg::Close => {
                        let _ = wr.shutdown().await;
                        break;
                    }
                }
            }
        });
        Arc::new(Out {
            tx,
            seq: Mutex::new(0),
        })
    }

    /// Assigns the next `seq` and queues the message, atomically (so seqs are in wire order).
    fn send(&self, build: impl FnOnce(i64) -> Value) {
        let mut seq = self.seq.lock().unwrap();
        *seq += 1;
        let _ = self.tx.send(Msg::Json(build(*seq)));
    }

    fn event(&self, name: &str, body: Value) {
        self.send(|seq| {
            let mut m = json!({"seq": seq, "type": "event", "event": name});
            if !body.is_null() {
                m["body"] = body;
            }
            m
        });
    }

    fn close(&self) {
        let _ = self.tx.send(Msg::Close);
    }
}

// ---------------------------------------------------------------------------------------------
// Program state of a session
// ---------------------------------------------------------------------------------------------

const FRAME_BASE: i64 = 100_000;

/// (frame, scope, path of variable indexes): the container of a `variablesReference`.
type Target = (usize, usize, Vec<usize>);

#[derive(Clone)]
struct Bp {
    line: u64,
    condition: Option<String>,
    log: Option<String>,
}

struct Current {
    stop: Stop,
    epoch: i64,
    refs: HashMap<i64, Target>,
    ref_of: HashMap<Target, i64>,
}

struct Allocator<'a> {
    n: i64,
    refs: HashMap<i64, Target>,
    ref_of: HashMap<Target, i64>,
    overrides: &'a HashMap<String, String>,
}

impl Allocator<'_> {
    fn alloc(&mut self, target: Target) {
        self.n += 1;
        self.refs.insert(self.n, target.clone());
        self.ref_of.insert(target, self.n);
    }

    fn walk(
        &mut self,
        vars: &mut [Var],
        prefix: &str,
        path: &mut Vec<usize>,
        fi: usize,
        si: usize,
    ) {
        for (i, v) in vars.iter_mut().enumerate() {
            let key = format!("{prefix}/{}", v.name);
            if let Some(o) = self.overrides.get(&key) {
                v.value = o.clone();
            }
            if !v.children.is_empty() {
                path.push(i);
                self.alloc((fi, si, path.clone()));
                self.walk(&mut v.children, &key, path, fi, si);
                path.pop();
            }
        }
    }
}

impl Current {
    fn build(stop: &Stop, epoch: i64, overrides: &HashMap<String, String>) -> Current {
        let mut stop = stop.clone();
        let mut a = Allocator {
            n: epoch * 10_000,
            refs: HashMap::new(),
            ref_of: HashMap::new(),
            overrides,
        };
        for (fi, f) in stop.frames.iter_mut().enumerate() {
            for (si, sc) in f.scopes.iter_mut().enumerate() {
                a.alloc((fi, si, Vec::new()));
                let name = sc.name.clone();
                a.walk(&mut sc.vars, &name, &mut Vec::new(), fi, si);
            }
        }
        Current {
            stop,
            epoch,
            refs: a.refs,
            ref_of: a.ref_of,
        }
    }

    fn frame_id(&self, index: usize) -> i64 {
        FRAME_BASE + self.epoch * 100 + index as i64
    }

    fn frame_index(&self, id: Option<i64>) -> Result<usize, String> {
        let Some(id) = id else { return Ok(0) };
        let base = FRAME_BASE + self.epoch * 100;
        if id >= base && ((id - base) as usize) < self.stop.frames.len() {
            Ok((id - base) as usize)
        } else {
            Err(format!("unknown frameId {id}"))
        }
    }

    fn vars_at(&self, t: &Target) -> Option<&Vec<Var>> {
        let mut vars = &self.stop.frames.get(t.0)?.scopes.get(t.1)?.vars;
        for &i in &t.2 {
            vars = &vars.get(i)?.children;
        }
        Some(vars)
    }

    fn vars_at_mut(&mut self, t: &Target) -> Option<&mut Vec<Var>> {
        let mut vars = &mut self.stop.frames.get_mut(t.0)?.scopes.get_mut(t.1)?.vars;
        for &i in &t.2 {
            let tmp = vars;
            vars = &mut tmp.get_mut(i)?.children;
        }
        Some(vars)
    }

    /// The (scope, path) of `a.b` in the scopes of frame `fi`.
    fn find(&self, fi: usize, expr: &str) -> Option<(usize, Vec<usize>)> {
        let parts: Vec<&str> = expr.split('.').collect();
        let scopes = &self.stop.frames.get(fi)?.scopes;
        for (si, sc) in scopes.iter().enumerate() {
            if let Some(i) = sc.vars.iter().position(|v| v.name == parts[0]) {
                let mut path = vec![i];
                let mut cur = &sc.vars[i];
                for p in &parts[1..] {
                    let j = cur.children.iter().position(|c| c.name == *p)?;
                    path.push(j);
                    cur = &cur.children[j];
                }
                return Some((si, path));
            }
        }
        None
    }

    fn var(&self, fi: usize, si: usize, full: &[usize]) -> Option<&Var> {
        let (last, container) = full.split_last()?;
        self.vars_at(&(fi, si, container.to_vec()))?.get(*last)
    }

    /// (value, type, variablesReference, memoryReference)
    fn evaluate(
        &self,
        expr: &str,
        frame_id: Option<i64>,
    ) -> Result<(String, Option<String>, i64, Option<String>), String> {
        let fi = self.frame_index(frame_id)?;
        let compact: String = expr.chars().filter(|c| !c.is_whitespace()).collect();
        if compact == "1+2" {
            return Ok(("3".into(), None, 0, None));
        }
        let (si, path) = self
            .find(fi, expr.trim())
            .ok_or_else(|| "cannot evaluate".to_string())?;
        let v = self.var(fi, si, &path).ok_or("cannot evaluate")?;
        let r = self.ref_of.get(&(fi, si, path)).copied().unwrap_or(0);
        Ok((v.value.clone(), v.ty.clone(), r, v.memory_reference.clone()))
    }

    /// Sets the value at `full`; returns (override key, value, type, reference).
    fn set_at(
        &mut self,
        fi: usize,
        si: usize,
        full: &[usize],
        value: &str,
    ) -> Option<(String, String, Option<String>, i64)> {
        let (last, container) = full.split_last()?;
        let ctarget = (fi, si, container.to_vec());
        let var = self.vars_at_mut(&ctarget)?.get_mut(*last)?;
        var.value = value.to_string();
        let (name, ty) = (var.name.clone(), var.ty.clone());
        // The key walks the names of the containers, like `Allocator::walk` does.
        let sc = &self.stop.frames[fi].scopes[si];
        let mut key = sc.name.clone();
        let mut vars = &sc.vars;
        for &i in container {
            key.push('/');
            key.push_str(&vars[i].name);
            vars = &vars[i].children;
        }
        key.push('/');
        key.push_str(&name);
        let r = self
            .ref_of
            .get(&(fi, si, full.to_vec()))
            .copied()
            .unwrap_or(0);
        Some((key, value.to_string(), ty, r))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Continue,
    Step,
    Pause,
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Single,
    Parent,
    Child,
}

/// A stop waiting out its delay.
struct Pending {
    token: u64,
    /// The queue it is in: `None` for the single queue, else the thread's.
    queue: Option<i64>,
    index: usize,
    reason: String,
}

#[derive(Default)]
struct Program {
    /// The next stop of the single queue.
    next: usize,
    /// The stop that was reported last, while its thread is stopped.
    current: Option<Current>,
    /// Thread mode: the other threads that are stopped.
    others: HashMap<i64, Current>,
    /// Thread mode: the next stop of each thread's queue.
    pos: HashMap<i64, usize>,
    /// The threads alive: (id, name).
    alive: Vec<(i64, String)>,
    /// Stops waiting out their delay, by thread.
    pending: HashMap<i64, Pending>,
    next_token: u64,
    epoch: i64,
    exited: bool,
    bps: HashMap<String, Vec<Bp>>,
    bp_ids: HashMap<(String, u64), i64>,
    overrides: HashMap<String, String>,
}

impl Program {
    fn currents(&self) -> impl Iterator<Item = &Current> {
        self.current.iter().chain(self.others.values())
    }

    /// The stop to answer for when a request names no frame: the last reported, else the lowest
    /// thread's.
    fn default_current(&self) -> Option<&Current> {
        self.current
            .as_ref()
            .or_else(|| self.others.values().min_by_key(|c| c.stop.thread))
    }

    fn current_of(&self, thread: i64) -> Option<&Current> {
        self.currents().find(|c| c.stop.thread == thread)
    }

    /// The stop that owns frame `frame` / reference `reference` (or the default one).
    fn find(&self, frame: Option<i64>, reference: Option<i64>) -> Option<&Current> {
        let pick = |c: &&Current| match (frame, reference) {
            (_, Some(r)) => c.refs.contains_key(&r),
            (Some(f), None) => c.frame_index(Some(f)).is_ok(),
            (None, None) => true,
        };
        if reference.is_none() && frame.is_none() {
            return self.default_current();
        }
        self.currents().find(pick)
    }

    fn find_mut(&mut self, frame: Option<i64>, reference: Option<i64>) -> Option<&mut Current> {
        let pick = |c: &Current| match (frame, reference) {
            (_, Some(r)) => c.refs.contains_key(&r),
            (Some(f), None) => c.frame_index(Some(f)).is_ok(),
            (None, None) => true,
        };
        if self.current.as_ref().is_some_and(pick) {
            return self.current.as_mut();
        }
        self.others.values_mut().find(|c| pick(c))
    }

    /// The thread resumes: its stop is gone.
    fn take_current(&mut self, thread: Option<i64>) {
        match thread {
            None => self.current = None,
            Some(t) => {
                if self.current.as_ref().is_some_and(|c| c.stop.thread == t) {
                    self.current = None;
                }
                self.others.remove(&t);
            }
        }
    }

    /// `current` becomes the stop of its thread's session, the one before it joins `others`.
    fn install(&mut self, cur: Current, thread_mode: bool) {
        if thread_mode {
            let thread = cur.stop.thread;
            self.others.remove(&thread);
            if let Some(old) = self.current.take() {
                if old.stop.thread != thread {
                    self.others.insert(old.stop.thread, old);
                }
            }
        }
        self.current = Some(cur);
    }
}

fn same_file(a: &str, b: &str) -> bool {
    a == b || std::path::Path::new(a).file_name() == std::path::Path::new(b).file_name()
}

// ---------------------------------------------------------------------------------------------
// A session
// ---------------------------------------------------------------------------------------------

struct Req {
    seq: i64,
    command: String,
    args: Value,
}

/// A delayed stop is due.
struct Fire {
    thread: i64,
    token: u64,
}

struct Session {
    index: usize,
    shared: Arc<Shared>,
    out: Arc<Out>,
    role: Role,
    configured: bool,
    pending_launch: Option<Req>,
    prog: Program,
    fire: mpsc::UnboundedSender<Fire>,
    /// The `stopped` event of the stop being installed is not sent (a silent stop of thread mode).
    quiet: bool,
    /// The `stopped` event carries `preserveFocusHint: true`.
    hint: bool,
}

async fn serve(stream: TcpStream, shared: Arc<Shared>, index: usize) {
    let (mut rd, wr) = stream.into_split();
    let out = Out::spawn(wr);
    let role = match (shared.script.child, index) {
        (false, _) => Role::Single,
        (true, 0) => Role::Parent,
        (true, _) => Role::Child,
    };
    if role == Role::Parent {
        *shared.parent_out.lock().unwrap() = Some(out.clone());
    }
    let (fire, mut due) = mpsc::unbounded_channel();
    let mut prog = Program::default();
    if role != Role::Parent {
        prog.alive = shared.script.initial_threads();
    }
    let mut s = Session {
        index,
        shared,
        out,
        role,
        configured: false,
        pending_launch: None,
        prog,
        fire,
        quiet: false,
        hint: false,
    };
    let mut framer = Framer::default();
    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            read = rd.read(&mut buf) => {
                let n = match read {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let Ok(msgs) = framer.push(&buf[..n]) else {
                    break;
                };
                for m in msgs {
                    if s.handle(m) {
                        return;
                    }
                }
            }
            Some(Fire { thread, token }) = due.recv() => s.fire_due(thread, token),
        }
    }
}

impl Session {
    fn respond(&self, req: &Req, body: Option<Value>) {
        self.out.send(|seq| {
            let mut m = json!({"seq": seq, "type": "response", "request_seq": req.seq,
                "success": true, "command": req.command});
            if let Some(b) = body {
                m["body"] = b;
            }
            m
        });
    }

    fn fail(&self, req: &Req, message: &str) {
        self.out.send(|seq| {
            json!({"seq": seq, "type": "response", "request_seq": req.seq,
                "success": false, "command": req.command, "message": message})
        });
    }

    /// Returns true when the connection is to be closed.
    fn handle(&mut self, msg: Value) -> bool {
        match msg["type"].as_str() {
            Some("response") => {
                self.shared.responses.lock().unwrap().push(ClientResponse {
                    session: self.index,
                    command: msg["command"].as_str().unwrap_or("").to_string(),
                    success: msg["success"].as_bool().unwrap_or(false),
                    body: msg.get("body").cloned().unwrap_or(Value::Null),
                });
                return false;
            }
            Some("request") => {}
            _ => return false,
        }
        let req = Req {
            seq: msg["seq"].as_i64().unwrap_or(0),
            command: msg["command"].as_str().unwrap_or("").to_string(),
            args: msg.get("arguments").cloned().unwrap_or(Value::Null),
        };
        self.shared.log.lock().unwrap().push(Logged {
            session: self.index,
            command: req.command.clone(),
            arguments: req.args.clone(),
        });
        let script = self.shared.script.clone();
        match req.command.as_str() {
            "initialize" => {
                self.respond(&req, Some(script.capabilities()));
                self.out.event("initialized", Value::Null);
            }
            "launch" | "attach" => {
                if !script.configuration_done || self.configured {
                    self.respond(&req, None);
                    self.after_launch();
                } else {
                    self.pending_launch = Some(req);
                }
            }
            "configurationDone" => {
                self.respond(&req, None);
                self.configured = true;
                if let Some(launch) = self.pending_launch.take() {
                    self.respond(&launch, None);
                    self.after_launch();
                }
            }
            "setBreakpoints" => self.set_breakpoints(&req),
            "setExceptionBreakpoints" => self.respond(&req, None),
            "threads" => {
                let threads: Vec<Value> = self
                    .prog
                    .alive
                    .iter()
                    .map(|(id, name)| json!({"id": id, "name": name}))
                    .collect();
                self.respond(&req, Some(json!({ "threads": threads })));
            }
            "stackTrace" => self.stack_trace(&req),
            "scopes" => self.scopes(&req),
            "variables" => self.variables(&req),
            "evaluate" => {
                let frame = req.args["frameId"].as_i64();
                let r = match self.prog.find(frame, None).or(self.prog.default_current()) {
                    None => Err("cannot evaluate".to_string()),
                    Some(c) => c.evaluate(req.args["expression"].as_str().unwrap_or(""), frame),
                };
                match r {
                    Ok((value, ty, r, memory)) => {
                        let mut b = json!({"result": value, "variablesReference": r});
                        if let Some(t) = ty {
                            b["type"] = t.into();
                        }
                        if let Some(m) = memory {
                            b["memoryReference"] = m.into();
                        }
                        self.respond(&req, Some(b));
                    }
                    Err(m) => self.fail(&req, &m),
                }
            }
            "readMemory" if script.read_memory => self.read_memory(&req),
            "writeMemory" if script.write_memory => self.write_memory(&req),
            "setVariable" if script.set_variable => self.set_variable(&req),
            "setExpression" if script.set_expression => self.set_expression(&req),
            "continue" | "next" | "stepIn" | "stepOut" => {
                let thread = self.request_thread(&req);
                if !self.is_stopped(thread) {
                    return self.not_stopped(&req);
                }
                let single = req.args["singleThread"].as_bool() == Some(true);
                if req.command == "continue" {
                    self.respond(&req, Some(json!({"allThreadsContinued": !single})));
                } else {
                    self.respond(&req, None);
                }
                self.out.event(
                    "continued",
                    json!({"threadId": thread, "allThreadsContinued": !single}),
                );
                let mode = if req.command == "continue" {
                    Mode::Continue
                } else {
                    Mode::Step
                };
                self.resume(thread, mode);
            }
            "pause" => {
                self.respond(&req, None);
                let thread = req.args["threadId"].as_i64().unwrap_or(1);
                let key = if self.prog.pending.contains_key(&thread) {
                    Some(thread)
                } else if self.prog.pending.len() == 1 {
                    self.prog.pending.keys().next().copied()
                } else {
                    None
                };
                match key.and_then(|k| self.prog.pending.remove(&k)) {
                    // the program was running towards a stop: it stops there, paused
                    Some(p) => self.emit_stop(p.queue, p.index, "pause".into()),
                    None => {
                        let thread = self.request_thread(&req);
                        self.resume(thread, Mode::Pause)
                    }
                }
            }
            "terminate" => {
                self.respond(&req, None);
                self.exit_sequence();
            }
            "disconnect" => {
                self.respond(&req, None);
                self.out.close();
                return true;
            }
            _ => self.fail(&req, "unsupported"),
        }
        false
    }

    /// The thread a request is for: `threadId`, else the current stop's.
    fn request_thread(&self, req: &Req) -> i64 {
        req.args["threadId"]
            .as_i64()
            .or_else(|| self.prog.default_current().map(|c| c.stop.thread))
            .unwrap_or(1)
    }

    /// Whether `thread` is stopped (the single queue has one stop at a time, whatever its thread).
    fn is_stopped(&self, thread: i64) -> bool {
        if self.shared.script.thread_mode {
            self.prog.current_of(thread).is_some()
        } else {
            self.prog.current.is_some()
        }
    }

    fn not_stopped(&self, req: &Req) -> bool {
        self.fail(req, "not stopped");
        false
    }

    /// After the launch response.
    fn after_launch(&mut self) {
        match self.role {
            Role::Parent => {
                self.out.send(|seq| {
                    json!({"seq": seq, "type": "request", "command": "startDebugging",
                        "arguments": {"request": "launch", "configuration":
                            {"type": "fake", "name": "child [1]", "__pendingTargetId": "t1"}}})
                });
                return;
            }
            Role::Child => self.out.event("initialized", Value::Null),
            Role::Single => {}
        }
        self.out.event(
            "process",
            json!({"name": self.shared.script.source, "systemProcessId": std::process::id(),
                "isLocalProcess": true, "startMethod": "launch"}),
        );
        self.out
            .event("thread", json!({"reason": "started", "threadId": 1}));
        for (category, text) in &self.shared.script.outputs {
            self.out
                .event("output", json!({"category": category, "output": text}));
        }
        let script = self.shared.script.clone();
        if script.thread_mode {
            // every thread runs to its first stop; one `stopped` in an all-stop session
            for (n, (id, _)) in script.programs.iter().enumerate() {
                self.quiet = script.all_stopped && n > 0;
                // in a non-stop session the other threads' stops didn't cause the focus
                self.hint = !script.all_stopped && n > 0;
                self.resume(*id, Mode::Continue);
            }
            self.quiet = false;
            self.hint = false;
        } else {
            self.resume(1, Mode::Continue);
        }
    }

    fn set_breakpoints(&mut self, req: &Req) {
        let path = req.args["source"]["path"]
            .as_str()
            .or_else(|| req.args["source"]["name"].as_str())
            .unwrap_or("")
            .to_string();
        let wanted: Vec<Bp> = match req.args["breakpoints"].as_array() {
            Some(list) => list
                .iter()
                .map(|b| Bp {
                    line: b["line"].as_u64().unwrap_or(0),
                    condition: b["condition"].as_str().map(String::from),
                    log: b["logMessage"].as_str().map(String::from),
                })
                .collect(),
            None => req.args["lines"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|l| Bp {
                            line: l.as_u64().unwrap_or(0),
                            condition: None,
                            log: None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let mut answer = Vec::new();
        for bp in &wanted {
            let next_id = self.prog.bp_ids.len() as i64 + 1;
            let id = *self
                .prog
                .bp_ids
                .entry((path.clone(), bp.line))
                .or_insert(next_id);
            answer.push(json!({"id": id, "verified": true, "line": bp.line}));
        }
        self.prog.bps.insert(path, wanted);
        self.respond(req, Some(json!({ "breakpoints": answer })));
    }

    fn stack_trace(&self, req: &Req) {
        let thread = req.args["threadId"].as_i64().unwrap_or(1);
        let Some(c) = self.prog.current_of(thread) else {
            return self.respond(req, Some(json!({"stackFrames": [], "totalFrames": 0})));
        };
        let start = req.args["startFrame"].as_u64().unwrap_or(0) as usize;
        let levels = match req.args["levels"].as_u64() {
            Some(0) | None => usize::MAX,
            Some(n) => n as usize,
        };
        let frames: Vec<Value> = c
            .stop
            .frames
            .iter()
            .enumerate()
            .skip(start)
            .take(levels)
            .map(|(i, f)| {
                let name = std::path::Path::new(&f.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                json!({"id": c.frame_id(i), "name": f.name, "line": f.line, "column": 1,
                    "source": {"name": name, "path": f.path}})
            })
            .collect();
        self.respond(
            req,
            Some(json!({"stackFrames": frames, "totalFrames": c.stop.frames.len()})),
        );
    }

    fn scopes(&self, req: &Req) {
        if self.prog.currents().next().is_none() {
            return self.fail(req, "not stopped");
        }
        let id = req.args["frameId"].as_i64().unwrap_or(-1);
        let Some(c) = self.prog.find(Some(id), None) else {
            return self.fail(req, &format!("unknown frameId {id}"));
        };
        let fi = c.frame_index(Some(id)).unwrap_or(0);
        let scopes: Vec<Value> = c.stop.frames[fi]
            .scopes
            .iter()
            .enumerate()
            .map(|(si, sc)| {
                json!({"name": sc.name, "expensive": sc.expensive,
                    "variablesReference": c.ref_of[&(fi, si, Vec::new())]})
            })
            .collect();
        self.respond(req, Some(json!({ "scopes": scopes })));
    }

    fn variables(&self, req: &Req) {
        let r = req.args["variablesReference"].as_i64().unwrap_or(0);
        let Some((c, t)) = self
            .prog
            .find(None, Some(r))
            .and_then(|c| c.refs.get(&r).map(|t| (c, t)))
        else {
            return self.fail(req, &format!("unknown variablesReference {r}"));
        };
        let vars = c.vars_at(t).cloned().unwrap_or_default();
        let start = req.args["start"].as_u64().unwrap_or(0) as usize;
        let count = req.args["count"]
            .as_u64()
            .filter(|n| *n > 0)
            .map_or(usize::MAX, |n| n as usize);
        let list: Vec<Value> = vars
            .iter()
            .enumerate()
            .skip(start)
            .take(count)
            .map(|(i, v)| {
                let mut path = t.2.clone();
                path.push(i);
                let vr = c.ref_of.get(&(t.0, t.1, path)).copied().unwrap_or(0);
                let mut j = json!({"name": v.name, "value": v.value, "variablesReference": vr});
                if let Some(ty) = &v.ty {
                    j["type"] = ty.clone().into();
                }
                if let Some(m) = &v.memory_reference {
                    j["memoryReference"] = m.clone().into();
                }
                if v.indexed && !v.children.is_empty() {
                    j["indexedVariables"] = v.children.len().into();
                }
                j
            })
            .collect();
        self.respond(req, Some(json!({ "variables": list })));
    }

    fn read_memory(&self, req: &Req) {
        let Some((address, count)) = memory_range(&req.args) else {
            return self.fail(req, "bad memoryReference");
        };
        let memory = self.shared.memory.lock().unwrap();
        let first = (0..count as u64).find(|i| memory.contains_key(&(address + i)));
        let Some(first) = first else {
            drop(memory);
            if self.shared.script.read_errors {
                return self.fail(req, "cannot read memory");
            }
            return self.respond(
                req,
                Some(json!({"address": format!("0x{address:x}"), "unreadableBytes": count})),
            );
        };
        let start = address + first;
        let data: Vec<u8> = (first..count as u64)
            .map_while(|i| memory.get(&(address + i)).copied())
            .collect();
        let unreadable = count - first as usize - data.len();
        drop(memory);
        self.respond(
            req,
            Some(
                json!({"address": format!("0x{start:x}"), "data": base64_encode(&data),
                "unreadableBytes": unreadable}),
            ),
        );
    }

    fn write_memory(&self, req: &Req) {
        let Some((address, _)) = memory_range(&req.args) else {
            return self.fail(req, "bad memoryReference");
        };
        let Some(data) = req.args["data"].as_str().and_then(base64_decode) else {
            return self.fail(req, "bad data");
        };
        let mut memory = self.shared.memory.lock().unwrap();
        if (0..data.len() as u64).any(|i| !memory.contains_key(&(address + i))) {
            drop(memory);
            return self.fail(req, "cannot write memory");
        }
        for (i, byte) in data.iter().enumerate() {
            memory.insert(address + i as u64, *byte);
        }
        drop(memory);
        let written = self.shared.script.bytes_written.unwrap_or(data.len());
        self.respond(req, Some(json!({ "bytesWritten": written })));
    }

    fn answer_set(&mut self, req: &Req, done: Option<(String, String, Option<String>, i64)>) {
        match done {
            Some((key, value, ty, r)) => {
                self.prog.overrides.insert(key, value.clone());
                let mut b = json!({"value": value, "variablesReference": r});
                if let Some(t) = ty {
                    b["type"] = t.into();
                }
                self.respond(req, Some(b));
            }
            None => self.fail(req, "cannot set"),
        }
    }

    fn set_variable(&mut self, req: &Req) {
        let r = req.args["variablesReference"].as_i64().unwrap_or(0);
        let name = req.args["name"].as_str().unwrap_or("");
        let value = req.args["value"].as_str().unwrap_or("");
        let done = self.prog.find_mut(None, Some(r)).and_then(|c| {
            let t = c.refs.get(&r)?.clone();
            let idx = c.vars_at(&t)?.iter().position(|v| v.name == name)?;
            let mut full = t.2.clone();
            full.push(idx);
            c.set_at(t.0, t.1, &full, value)
        });
        self.answer_set(req, done);
    }

    fn set_expression(&mut self, req: &Req) {
        let expr = req.args["expression"].as_str().unwrap_or("").trim();
        let value = req.args["value"].as_str().unwrap_or("");
        let frame = req.args["frameId"].as_i64();
        let done = self.prog.find_mut(frame, None).and_then(|c| {
            let fi = c.frame_index(frame).ok()?;
            let (si, path) = c.find(fi, expr)?;
            c.set_at(fi, si, &path, value)
        });
        self.answer_set(req, done);
    }

    /// The stops of a queue.
    fn queue(&self, queue: Option<i64>) -> Vec<Stop> {
        match queue {
            None => self.shared.script.stops.clone(),
            Some(t) => self.shared.script.program(t).to_vec(),
        }
    }

    /// Thread `thread` runs (the single queue in a plain script, the thread's in thread mode)
    /// to its next stop, or to its end.
    fn resume(&mut self, thread: i64, mode: Mode) {
        let shared = self.shared.clone();
        let queue = shared.script.thread_mode.then_some(thread);
        self.prog.take_current(queue);
        let stops = self.queue(queue);
        let step = mode == Mode::Pause || (mode == Mode::Step && shared.script.stepping);
        let mut i = match queue {
            None => self.prog.next,
            Some(t) => self.prog.pos.get(&t).copied().unwrap_or(0),
        };
        let mut found: Option<(usize, String)> = None;
        while i < stops.len() {
            let stop = &stops[i];
            if step {
                let reason = if mode == Mode::Pause {
                    "pause"
                } else if stop.keep_reason {
                    stop.reason.as_str()
                } else {
                    "step"
                };
                found = Some((i, reason.to_string()));
                break;
            }
            if stop.reason != "breakpoint" {
                found = Some((i, stop.reason.clone()));
                break;
            }
            let cur = Current::build(stop, self.prog.epoch + 1, &self.prog.overrides);
            let path = &stop.frames[0].path;
            let candidates: Vec<Bp> = self
                .prog
                .bps
                .iter()
                .filter(|(p, _)| same_file(p, path))
                .flat_map(|(_, v)| v.iter().cloned())
                .filter(|bp| bp.line == stop.line)
                .collect();
            let mut hit = false;
            for bp in candidates {
                if let Some(cond) = &bp.condition {
                    if let Ok((v, ..)) = cur.evaluate(cond, None) {
                        if v == "false" || v == "0" {
                            continue;
                        }
                    }
                }
                if let Some(m) = &bp.log {
                    self.out.event(
                        "output",
                        json!({"category": "console", "output": format!("{m}\n")}),
                    );
                    continue;
                }
                hit = true;
                break;
            }
            if hit {
                found = Some((i, stop.reason.clone()));
                break;
            }
            i += 1;
        }
        match found {
            Some((i, reason)) => {
                match queue {
                    None => self.prog.next = i + 1,
                    Some(t) => {
                        self.prog.pos.insert(t, i + 1);
                    }
                }
                let delay = stops[i].delay_ms;
                if delay > 0 && mode != Mode::Pause {
                    self.prog.next_token += 1;
                    let token = self.prog.next_token;
                    let thread = stops[i].thread;
                    self.prog.pending.insert(
                        thread,
                        Pending {
                            token,
                            queue,
                            index: i,
                            reason,
                        },
                    );
                    let fire = self.fire.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        let _ = fire.send(Fire { thread, token });
                    });
                } else {
                    self.emit_stop(queue, i, reason);
                }
            }
            None => match queue {
                None => {
                    self.prog.next = stops.len();
                    if mode == Mode::Pause || shared.script.run_forever {
                        return;
                    }
                    self.exit_sequence();
                }
                Some(t) => {
                    self.prog.pos.insert(t, stops.len());
                    if mode == Mode::Pause {
                        return;
                    }
                    // the thread ends
                    self.prog.alive.retain(|(id, _)| *id != t);
                    self.out
                        .event("thread", json!({"reason": "exited", "threadId": t}));
                    if self.prog.alive.is_empty() && !shared.script.run_forever {
                        self.exit_sequence();
                    }
                }
            },
        }
    }

    /// A delayed stop is due.
    fn fire_due(&mut self, thread: i64, token: u64) {
        if self.prog.pending.get(&thread).map(|p| p.token) != Some(token) {
            return;
        }
        let p = self.prog.pending.remove(&thread).unwrap();
        self.emit_stop(p.queue, p.index, p.reason);
    }

    /// The program stops at stop `index` of `queue`: its thread events, then `stopped`.
    fn emit_stop(&mut self, queue: Option<i64>, index: usize, reason: String) {
        let shared = self.shared.clone();
        let stops = self.queue(queue);
        let stop = &stops[index];
        self.prog.epoch += 1;
        for event in &stop.events {
            match event {
                ThreadEvent::Started(id, name) => {
                    if !self.prog.alive.iter().any(|(i, _)| i == id) {
                        self.prog.alive.push((*id, name.clone()));
                    }
                    self.out
                        .event("thread", json!({"reason": "started", "threadId": id}));
                }
                ThreadEvent::Exited(id) => {
                    self.prog.alive.retain(|(i, _)| i != id);
                    self.out
                        .event("thread", json!({"reason": "exited", "threadId": id}));
                }
            }
        }
        {
            let mut memory = shared.memory.lock().unwrap();
            for (address, bytes) in &stop.pokes {
                for (i, byte) in bytes.iter().enumerate() {
                    memory.insert(address + i as u64, *byte);
                }
            }
        }
        let cur = Current::build(stop, self.prog.epoch, &self.prog.overrides);
        self.prog.install(cur, shared.script.thread_mode);
        shared
            .sent
            .lock()
            .unwrap()
            .push((stop.thread, stop.line, reason.clone()));
        if !self.quiet {
            let mut body = json!({"reason": reason, "threadId": stop.thread,
                "allThreadsStopped": shared.script.all_stopped});
            if self.hint {
                body["preserveFocusHint"] = true.into();
            }
            self.out.event("stopped", body);
        }
    }

    fn exit_sequence(&mut self) {
        self.prog.exited = true;
        self.prog.current = None;
        self.prog.others.clear();
        self.prog.pending.clear();
        let code = self.shared.script.exit_code;
        match self.role {
            Role::Single => {
                self.out.event("exited", json!({"exitCode": code}));
                self.out.event("terminated", Value::Null);
            }
            Role::Child => {
                self.out.event(
                    "output",
                    json!({"category": "stderr", "output": format!("Process exited with code {code}\n")}),
                );
                self.out.event("terminated", Value::Null);
                if let Some(parent) = self.shared.parent_out.lock().unwrap().clone() {
                    parent.event("terminated", Value::Null);
                }
            }
            Role::Parent => self.out.event("terminated", Value::Null),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A tiny raw client, for the tests of the fake itself (also usable by other tests)
// ---------------------------------------------------------------------------------------------

pub struct RawClient {
    rd: OwnedReadHalf,
    wr: OwnedWriteHalf,
    framer: Framer,
    queue: VecDeque<Value>,
    seq: i64,
}

impl RawClient {
    pub async fn connect(addr: SocketAddr) -> Result<RawClient> {
        let (rd, wr) = TcpStream::connect(addr).await?.into_split();
        Ok(RawClient {
            rd,
            wr,
            framer: Framer::default(),
            queue: VecDeque::new(),
            seq: 0,
        })
    }

    /// Sends a request, returns its seq.
    pub async fn request(&mut self, command: &str, arguments: Value) -> Result<i64> {
        self.seq += 1;
        let msg = json!({"seq": self.seq, "type": "request", "command": command,
            "arguments": arguments});
        self.send_raw(&encode(&msg)).await?;
        Ok(self.seq)
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.wr.write_all(bytes).await?;
        Ok(())
    }

    /// Answers a reverse request.
    pub async fn respond(&mut self, request: &Value, success: bool) -> Result<()> {
        self.seq += 1;
        let msg = json!({"seq": self.seq, "type": "response", "request_seq": request["seq"],
            "success": success, "command": request["command"]});
        self.send_raw(&encode(&msg)).await
    }

    /// The next message from the adapter (5 s timeout; an error at end of stream).
    pub async fn next(&mut self) -> Result<Value> {
        loop {
            if let Some(m) = self.queue.pop_front() {
                return Ok(m);
            }
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout(Duration::from_secs(5), self.rd.read(&mut buf))
                .await
                .map_err(|_| anyhow!("timed out waiting for a message"))??;
            if n == 0 {
                bail!("connection closed");
            }
            self.queue.extend(self.framer.push(&buf[..n])?);
        }
    }

    /// Reads messages until `pred` holds; returns it. The messages before it are dropped.
    pub async fn until(&mut self, pred: impl Fn(&Value) -> bool) -> Result<Value> {
        loop {
            let m = self.next().await?;
            if pred(&m) {
                return Ok(m);
            }
        }
    }

    /// The response to request `seq` (skipping whatever comes before it).
    pub async fn response(&mut self, seq: i64) -> Result<Value> {
        self.until(|m| m["type"] == "response" && m["request_seq"] == seq)
            .await
    }

    /// Sends a request and waits for its response.
    pub async fn call(&mut self, command: &str, arguments: Value) -> Result<Value> {
        let seq = self.request(command, arguments).await?;
        self.response(seq).await
    }

    /// The next message, which must be the event `name`.
    pub async fn expect_event(&mut self, name: &str) -> Result<Value> {
        let m = self.next().await?;
        if m["type"] != "event" || m["event"] != name {
            bail!("expected event {name}, got {m}");
        }
        Ok(m)
    }

    /// Waits for the end of the connection.
    pub async fn expect_eof(&mut self) -> Result<()> {
        loop {
            match self.next().await {
                Ok(_) => {}
                Err(e) if e.to_string() == "connection closed" => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Tests of the fake itself
// ---------------------------------------------------------------------------------------------

fn demo_scope(x: &str) -> Scope {
    Scope::new("Locals").var(Var::new("x", x).ty("int")).var(
        Var::new("p", "Point")
            .ty("Point")
            .child(Var::new("a", "5"))
            .child(Var::new("inner", "Inner").child(Var::new("z", "9"))),
    )
}

#[tokio::test]
async fn fake_adapter_single_session_to_a_stop_and_to_exit() -> Result<()> {
    let script = Script::new("/tmp/prog.js")
        .stop(
            Stop::new(3, "breakpoint")
                .frame("main", "/tmp/prog.js", 3)
                .frame("helper", "/tmp/prog.js", 10)
                .scope(demo_scope("1")),
        )
        .stop(Stop::new(5, "breakpoint").scope(demo_scope("1")))
        .exit_code(7)
        .output("stdout", "hello\n")
        .set_variable()
        .exception_filter("raised", "Raised");
    let fake = FakeAdapter::start(script).await?;
    let mut c = RawClient::connect(fake.addr).await?;

    // Messages split across writes and several in one write.
    let init = encode(
        &json!({"seq": 1, "type": "request", "command": "initialize",
        "arguments": {"adapterID": "fake"}}),
    );
    c.seq = 1;
    c.send_raw(&init[..7]).await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    c.send_raw(&init[7..]).await?;
    let r = c.next().await?;
    assert_eq!(r["type"], "response");
    assert_eq!(r["seq"], 1);
    assert_eq!(r["body"]["supportsSetVariable"], true);
    assert_eq!(r["body"]["supportsSetExpression"], false);
    assert_eq!(
        r["body"]["exceptionBreakpointFilters"][0]["filter"],
        "raised"
    );
    let e = c.expect_event("initialized").await?;
    assert_eq!(e["seq"], 2);

    // launch, setBreakpoints, setExceptionBreakpoints, configurationDone in one write.
    let mut batch = Vec::new();
    for (i, (cmd, args)) in [
        ("launch", json!({"program": "/tmp/prog.js"})),
        (
            "setBreakpoints",
            json!({"source": {"path": "/tmp/prog.js"},
                "breakpoints": [{"line": 3}, {"line": 5}]}),
        ),
        ("setExceptionBreakpoints", json!({"filters": ["raised"]})),
        ("configurationDone", json!({})),
    ]
    .into_iter()
    .enumerate()
    {
        batch.extend(encode(
            &json!({"seq": 10 + i, "type": "request", "command": cmd,
            "arguments": args}),
        ));
    }
    c.send_raw(&batch).await?;
    let r = c.next().await?;
    assert_eq!(r["command"], "setBreakpoints");
    assert_eq!(r["body"]["breakpoints"][1]["line"], 5);
    assert_eq!(r["body"]["breakpoints"][0]["verified"], true);
    let r = c.next().await?;
    assert_eq!(r["command"], "setExceptionBreakpoints");
    assert!(r.get("body").is_none(), "{r}");
    assert_eq!(c.next().await?["command"], "configurationDone");
    let r = c.next().await?;
    assert_eq!(
        (r["command"].as_str(), r["request_seq"].as_i64()),
        (Some("launch"), Some(10))
    );
    c.expect_event("process").await?;
    assert_eq!(c.expect_event("thread").await?["body"]["reason"], "started");
    assert_eq!(c.expect_event("output").await?["body"]["output"], "hello\n");
    let stopped = c.until(|m| m["event"] == "stopped").await?;
    assert_eq!(stopped["body"]["reason"], "breakpoint");
    assert_eq!(stopped["body"]["threadId"], 1);

    let r = c.call("threads", json!({})).await?;
    assert_eq!(r["body"]["threads"][0]["name"], "main");
    let r = c.call("stackTrace", json!({"threadId": 1})).await?;
    let frames = r["body"]["stackFrames"].as_array().unwrap().clone();
    assert_eq!(frames.len(), 2);
    assert_eq!(
        (frames[0]["name"].as_str(), frames[0]["line"].as_i64()),
        (Some("main"), Some(3))
    );
    let frame0 = frames[0]["id"].as_i64().unwrap();
    let r = c.call("scopes", json!({"frameId": frame0})).await?;
    let locals = r["body"]["scopes"][0]["variablesReference"]
        .as_i64()
        .unwrap();
    let r = c
        .call("variables", json!({"variablesReference": locals}))
        .await?;
    let vars = r["body"]["variables"].as_array().unwrap().clone();
    assert_eq!(vars[0]["name"], "x");
    assert_eq!(vars[0]["variablesReference"], 0);
    let p_ref = vars[1]["variablesReference"].as_i64().unwrap();
    assert!(p_ref > 0);
    let r = c
        .call("variables", json!({"variablesReference": p_ref}))
        .await?;
    assert_eq!(r["body"]["variables"][0]["name"], "a");
    let inner_ref = r["body"]["variables"][1]["variablesReference"]
        .as_i64()
        .unwrap();
    assert!(inner_ref > 0);

    let r = c
        .call("evaluate", json!({"expression": "1+2", "context": "repl"}))
        .await?;
    assert_eq!(r["body"]["result"], "3");
    let r = c
        .call(
            "evaluate",
            json!({"expression": "p.inner.z", "context": "watch"}),
        )
        .await?;
    assert_eq!(r["body"]["result"], "9");
    let r = c.call("evaluate", json!({"expression": "nope"})).await?;
    assert_eq!(
        (r["success"].as_bool(), r["message"].as_str()),
        (Some(false), Some("cannot evaluate"))
    );
    let r = c
        .call(
            "setVariable",
            json!({"variablesReference": locals, "name": "x", "value": "42"}),
        )
        .await?;
    assert_eq!(r["body"]["value"], "42");
    let r = c
        .call("setExpression", json!({"expression": "x", "value": "1"}))
        .await?;
    assert_eq!(r["message"], "unsupported");
    let r = c.call("evaluate", json!({"expression": "x"})).await?;
    assert_eq!(r["body"]["result"], "42");

    // continue: continued, then the second stop; references change, old ones are stale.
    let seq = c.request("continue", json!({"threadId": 1})).await?;
    assert_eq!(c.response(seq).await?["body"]["allThreadsContinued"], true);
    c.expect_event("continued").await?;
    c.expect_event("stopped").await?;
    let r = c.call("stackTrace", json!({"threadId": 1})).await?;
    let new_frame = r["body"]["stackFrames"][0]["id"].as_i64().unwrap();
    assert_ne!(new_frame, frame0);
    assert_eq!(r["body"]["stackFrames"][0]["line"], 5);
    let r = c.call("scopes", json!({"frameId": frame0})).await?;
    assert_eq!(r["success"], false);
    let r = c.call("scopes", json!({"frameId": new_frame})).await?;
    let new_locals = r["body"]["scopes"][0]["variablesReference"]
        .as_i64()
        .unwrap();
    assert_ne!(new_locals, locals);
    assert_eq!(
        c.call("variables", json!({"variablesReference": locals}))
            .await?["success"],
        false
    );
    let r = c
        .call("variables", json!({"variablesReference": new_locals}))
        .await?;
    assert_eq!(
        r["body"]["variables"][0]["value"], "42",
        "setVariable is kept"
    );

    // next with no stop left: the program exits.
    let seq = c.request("next", json!({"threadId": 1})).await?;
    c.response(seq).await?;
    c.expect_event("continued").await?;
    assert_eq!(c.expect_event("exited").await?["body"]["exitCode"], 7);
    c.expect_event("terminated").await?;
    let r = c.call("bogus", json!({})).await?;
    assert_eq!(r["message"], "unsupported");
    let seq = c.request("disconnect", json!({})).await?;
    c.response(seq).await?;
    c.expect_eof().await?;

    let names: Vec<String> = fake.requests().into_iter().map(|(_, c, _)| c).collect();
    assert_eq!(
        &names[..5],
        [
            "initialize",
            "launch",
            "setBreakpoints",
            "setExceptionBreakpoints",
            "configurationDone"
        ]
    );
    assert_eq!(fake.requests_named("evaluate").len(), 4);
    assert_eq!(fake.requests_named("evaluate")[0].1["context"], "repl");
    fake.wait_for_request("disconnect", Duration::from_secs(1))
        .await?;
    Ok(())
}

#[tokio::test]
async fn fake_adapter_skips_stops_without_breakpoints() -> Result<()> {
    let script = Script::new("/tmp/a/prog.py")
        .stop(Stop::new(2, "breakpoint"))
        .stop(Stop::new(4, "breakpoint"))
        .stop(Stop::new(6, "exception"))
        .stepping(false)
        .exit_code(0);
    let fake = FakeAdapter::start(script).await?;
    let mut c = RawClient::connect(fake.addr).await?;
    c.call("initialize", json!({})).await?;
    c.expect_event("initialized").await?;
    // A different directory, same file name: still matches.
    c.call(
        "setBreakpoints",
        json!({"source": {"path": "/x/prog.py"}, "breakpoints": [{"line": 4}]}),
    )
    .await?;
    let launch = c.request("launch", json!({})).await?;
    c.call("configurationDone", json!({})).await?;
    c.response(launch).await?;
    c.expect_event("process").await?;
    c.expect_event("thread").await?;
    let s = c.expect_event("stopped").await?;
    assert_eq!(s["body"]["reason"], "breakpoint");
    let r = c.call("stackTrace", json!({"threadId": 1})).await?;
    assert_eq!(r["body"]["stackFrames"][0]["line"], 4);
    // Not stepping: next acts as continue; the exception stop is always hit.
    let seq = c.request("next", json!({"threadId": 1})).await?;
    c.response(seq).await?;
    c.expect_event("continued").await?;
    assert_eq!(
        c.expect_event("stopped").await?["body"]["reason"],
        "exception"
    );
    let seq = c.request("terminate", json!({})).await?;
    c.response(seq).await?;
    assert_eq!(c.expect_event("exited").await?["body"]["exitCode"], 0);
    c.expect_event("terminated").await?;
    Ok(())
}

#[tokio::test]
async fn fake_adapter_child_session_flow() -> Result<()> {
    let script = Script::new("/tmp/app.js")
        .child()
        .stop(Stop::new(2, "breakpoint").scope(demo_scope("1")))
        .output("stdout", "from the child\n")
        .exit_code(3);
    let fake = FakeAdapter::start(script).await?;

    // The parent.
    let mut parent = RawClient::connect(fake.addr).await?;
    parent.call("initialize", json!({})).await?;
    parent.expect_event("initialized").await?;
    let launch = parent
        .request("launch", json!({"type": "fake", "program": "/tmp/app.js"}))
        .await?;
    parent
        .call(
            "setBreakpoints",
            json!({"source": {"path": "/tmp/app.js"}, "breakpoints": [{"line": 2}]}),
        )
        .await?;
    let r = parent
        .call("setExceptionBreakpoints", json!({"filters": []}))
        .await?;
    assert!(r.get("body").is_none());
    // The launch is not answered before configurationDone.
    let cfg = parent.request("configurationDone", json!({})).await?;
    let r = parent.next().await?;
    assert_eq!(
        (r["command"].as_str(), r["request_seq"].as_i64()),
        (Some("configurationDone"), Some(cfg))
    );
    let r = parent.next().await?;
    assert_eq!(
        (r["command"].as_str(), r["request_seq"].as_i64()),
        (Some("launch"), Some(launch))
    );
    let rev = parent.next().await?;
    assert_eq!(rev["type"], "request");
    assert_eq!(rev["command"], "startDebugging");
    assert_eq!(rev["arguments"]["request"], "launch");
    let child_config = rev["arguments"]["configuration"].clone();
    assert_eq!(child_config["__pendingTargetId"], "t1");
    assert_eq!(child_config["name"], "child [1]");
    parent.respond(&rev, true).await?;
    assert_eq!(
        parent.call("threads", json!({})).await?["body"]["threads"],
        json!([])
    );

    // The child, on a second connection.
    let mut child = RawClient::connect(fake.addr).await?;
    child.call("initialize", json!({})).await?;
    child.expect_event("initialized").await?;
    let child_launch = child.request("launch", child_config.clone()).await?;
    child
        .call(
            "setBreakpoints",
            json!({"source": {"path": "/tmp/app.js"}, "breakpoints": [{"line": 2}]}),
        )
        .await?;
    let cfg = child.request("configurationDone", json!({})).await?;
    assert_eq!(child.next().await?["request_seq"], cfg);
    let r = child.next().await?;
    assert_eq!(
        (r["command"].as_str(), r["request_seq"].as_i64()),
        (Some("launch"), Some(child_launch))
    );
    child.expect_event("initialized").await?; // the second one
    child.expect_event("process").await?;
    child.expect_event("thread").await?;
    assert_eq!(
        child.expect_event("output").await?["body"]["output"],
        "from the child\n"
    );
    assert_eq!(
        child.expect_event("stopped").await?["body"]["reason"],
        "breakpoint"
    );

    // The parent has no stop of its own.
    let r = parent.call("stackTrace", json!({"threadId": 1})).await?;
    assert_eq!(r["body"]["totalFrames"], 0);

    // Continue to the end: the child terminates first, then the parent.
    let seq = child.request("continue", json!({"threadId": 1})).await?;
    child.response(seq).await?;
    child.expect_event("continued").await?;
    let out = child.expect_event("output").await?;
    assert_eq!(out["body"]["category"], "stderr");
    assert_eq!(out["body"]["output"], "Process exited with code 3\n");
    child.expect_event("terminated").await?;
    parent.expect_event("terminated").await?;
    for c in [&mut child, &mut parent] {
        let seq = c.request("disconnect", json!({})).await?;
        c.response(seq).await?;
        c.expect_eof().await?;
    }

    let launches = fake.requests_named("launch");
    assert_eq!(launches.len(), 2);
    assert_eq!((launches[0].0, launches[1].0), (0, 1));
    assert_eq!(launches[1].1, child_config);
    assert_eq!(fake.connections(), 2);
    let cr = fake.client_responses();
    assert_eq!(
        (cr[0].0, cr[0].1.as_str(), cr[0].2),
        (0, "startDebugging", true)
    );
    Ok(())
}

#[tokio::test]
async fn fake_adapter_memory_reads_writes_and_pokes() -> Result<()> {
    let script = Script::new("/tmp/prog.js")
        .read_memory()
        .write_memory()
        .memory(0x1000, [1, 2, 3, 4])
        .memory(0x1008, [9, 9])
        .stop(Stop::new(2, "entry").poke(0x1001, [7]))
        .stop(
            Stop::new(3, "entry")
                .scope(Scope::new("Locals").var(Var::new("p", "0x1000").memory("0x1000"))),
        );
    let fake = FakeAdapter::start(script).await?;
    let mut c = RawClient::connect(fake.addr).await?;
    let r = c.call("initialize", json!({})).await?;
    assert_eq!(r["body"]["supportsReadMemoryRequest"], true);
    assert_eq!(r["body"]["supportsWriteMemoryRequest"], true);
    c.expect_event("initialized").await?;
    c.call("configurationDone", json!({})).await?;
    let seq = c.request("launch", json!({})).await?;
    c.response(seq).await?;
    c.expect_event("stopped").await?;

    // the poke is in, a read stops where the region ends, then counts the unreadable bytes
    let r = c
        .call(
            "readMemory",
            json!({"memoryReference": "0x1000", "count": 8}),
        )
        .await?;
    assert_eq!(r["body"]["address"], "0x1000");
    assert_eq!(
        base64_decode(r["body"]["data"].as_str().unwrap()),
        Some(vec![1, 7, 3, 4])
    );
    assert_eq!(r["body"]["unreadableBytes"], 4);
    // an offset is added, the leading unreadable bytes are skipped
    let r = c
        .call(
            "readMemory",
            json!({"memoryReference": "0x1000", "offset": 2, "count": 8}),
        )
        .await?;
    assert_eq!(r["body"]["address"], "0x1002");
    assert_eq!(
        base64_decode(r["body"]["data"].as_str().unwrap()),
        Some(vec![3, 4])
    );
    let r = c
        .call(
            "readMemory",
            json!({"memoryReference": "0x2000", "count": 4}),
        )
        .await?;
    assert_eq!(r["body"]["unreadableBytes"], 4);
    assert!(r["body"]["data"].is_null());
    // writes are all or nothing
    let data = base64_encode(&[0xaa, 0xbb]);
    let r = c
        .call(
            "writeMemory",
            json!({"memoryReference": "0x1002", "data": data}),
        )
        .await?;
    assert_eq!(r["body"]["bytesWritten"], 2);
    assert_eq!(
        fake.memory_at(0x1000, 4),
        vec![Some(1), Some(7), Some(0xaa), Some(0xbb)]
    );
    let r = c
        .call(
            "writeMemory",
            json!({"memoryReference": "0x1003", "data": data}),
        )
        .await?;
    assert_eq!(r["success"], false);
    assert_eq!(fake.memory_at(0x1003, 2), vec![Some(0xbb), None]);
    assert_eq!(
        fake.memory_reads(),
        vec![(0x1000, 8), (0x1002, 8), (0x2000, 4)]
    );
    assert_eq!(
        fake.memory_writes(),
        vec![(0x1002, vec![0xaa, 0xbb]), (0x1003, vec![0xaa, 0xbb])]
    );
    for n in 0..5usize {
        let bytes: Vec<u8> = (0..n as u8).collect();
        assert_eq!(base64_decode(&base64_encode(&bytes)), Some(bytes));
    }
    assert_eq!(base64_encode(b"Man"), "TWFu");
    assert_eq!(base64_encode(b"Ma"), "TWE=");
    Ok(())
}
