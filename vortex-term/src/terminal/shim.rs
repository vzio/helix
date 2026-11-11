//! `vx` inside the panel: instead of starting a nested Vortex, it asks this editor to open its
//! files (or just to take focus) over a Unix socket, and as `$EDITOR` (`vx --wait`) waits until
//! the buffer is closed.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Set in the shell's environment: the path of the socket. Its presence puts the `vx` binary
/// in client mode.
pub const SOCKET_ENV: &str = "VORTEX_TERMINAL_SOCKET";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Focus,
    Open { files: Vec<OpenFile>, wait: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    /// Absolute path.
    pub path: PathBuf,
    /// 1-based.
    pub line: Option<usize>,
    /// 1-based.
    pub column: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Done,
    Error(String),
}

/// How the editor answers a request, possibly much later (when the awaited buffer is closed).
/// Dropping it without sending answers [`Response::Done`].
pub struct Reply(tokio::sync::oneshot::Sender<Response>);

impl Reply {
    pub fn send(self, response: Response) {
        let _ = self.0.send(response);
    }
}

/// Build the request for the command line arguments (without the program name).
/// `--wait` (the panel's `$EDITOR`) makes the client wait until the buffer is closed.
pub fn request_from_args(args: impl IntoIterator<Item = String>, cwd: &Path) -> Request {
    let mut files = Vec::new();
    let mut wait = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            files.extend(args.by_ref().map(|a| open_file(&a, cwd)));
            break;
        }
        match arg.as_str() {
            // Flags that take a value.
            "-c" | "--config" | "--log" | "-w" | "--working-dir" => {
                args.next();
            }
            "--wait" => wait = true,
            a if a.starts_with('-') && a.len() > 1 => {}
            a => files.push(open_file(a, cwd)),
        }
    }
    if files.is_empty() {
        Request::Focus
    } else {
        Request::Open { files, wait }
    }
}

fn open_file(arg: &str, cwd: &Path) -> OpenFile {
    let (path, pos) = crate::args::parse_file(arg);
    // `parse_file` yields (0, 0) both for "no position" and for `:1:1`: tell them apart by
    // whether the argument was split at all.
    let has_pos = Path::new(arg) != path;
    OpenFile {
        path: if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        },
        line: has_pos.then(|| pos.row + 1),
        column: (has_pos && arg_has_column(arg)).then(|| pos.col + 1),
    }
}

/// Whether the position part of `arg` has a column (`file:10:2`, not `file:10`).
fn arg_has_column(arg: &str) -> bool {
    let mut parts = arg.trim_end_matches(':').rsplitn(3, ':');
    let col = parts.next().and_then(|s| s.parse::<usize>().ok());
    let row = parts.next().and_then(|s| s.parse::<usize>().ok());
    col.is_some() && row.is_some() && parts.next().is_some()
}

/// Single-quote `s` for `/bin/sh`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(unix)]
pub use unix::{client_main, send, Server};

#[cfg(not(unix))]
pub use other::{client_main, Server};

#[cfg(not(unix))]
mod other {
    use super::*;

    pub struct Server;

    impl Server {
        pub fn start(
            _handler: impl Fn(Request, Reply) + Send + Sync + 'static,
        ) -> std::io::Result<Server> {
            Err(std::io::ErrorKind::Unsupported.into())
        }
        pub fn bin_dir(&self) -> &Path {
            Path::new("")
        }
        pub fn env(&self) -> Vec<(String, String)> {
            Vec::new()
        }
    }

    pub fn client_main() -> Option<i32> {
        None
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{self, BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    /// Requests are small; refuse anything larger.
    const MAX_REQUEST: u64 = 4 * 1024 * 1024;

    /// The socket and the shim scripts, in a private temp directory. Dropping it stops
    /// accepting and removes the directory.
    #[allow(dead_code)] // `dir` is held only for its Drop
    pub struct Server {
        /// Removes the directory on drop.
        pub(super) dir: tempfile::TempDir,
        socket: PathBuf,
        bin_dir: PathBuf,
        task: tokio::task::JoinHandle<()>,
    }

    impl Server {
        /// Must be called inside a tokio runtime: spawns the accept loop.
        pub fn start(
            handler: impl Fn(Request, Reply) + Send + Sync + 'static,
        ) -> io::Result<Server> {
            let dir = tempfile::Builder::new().prefix("vx-terminal-").tempdir()?;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
            let socket = dir.path().join("sock");
            let bin_dir = dir.path().join("bin");
            std::fs::create_dir(&bin_dir)?;
            let exe = std::env::current_exe()?;
            write_scripts(&bin_dir, &exe)?;

            let listener = UnixListener::bind(&socket)?;
            let handler = Arc::new(handler);
            let task = tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            let handler = handler.clone();
                            tokio::spawn(async move {
                                if let Err(err) = serve(stream, &*handler).await {
                                    log::warn!("terminal shim connection: {err}");
                                }
                            });
                        }
                        Err(err) => {
                            log::error!("terminal shim accept failed: {err}");
                            break;
                        }
                    }
                }
            });
            Ok(Server {
                dir,
                socket,
                bin_dir,
                task,
            })
        }

        /// Holds `vx`; put it first on the shell's PATH.
        pub fn bin_dir(&self) -> &Path {
            &self.bin_dir
        }

        /// The variables for the shell.
        pub fn env(&self) -> Vec<(String, String)> {
            // run by a shell (`sh -c "$EDITOR file"`), so quoted
            let editor = format!(
                "{} --wait",
                sh_quote(&self.bin_dir.join("vx").to_string_lossy())
            );
            vec![
                (
                    SOCKET_ENV.into(),
                    self.socket.to_string_lossy().into_owned(),
                ),
                ("EDITOR".into(), editor.clone()),
                ("VISUAL".into(), editor.clone()),
                ("GIT_EDITOR".into(), editor),
            ]
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
            // The directory itself is removed when `dir` drops.
        }
    }

    pub(super) fn write_scripts(bin_dir: &Path, exe: &Path) -> io::Result<()> {
        let exe = sh_quote(&exe.to_string_lossy());
        let path = bin_dir.join("vx");
        std::fs::write(&path, format!("#!/bin/sh\nexec {exe} \"$@\"\n"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
    }

    async fn serve(
        stream: tokio::net::UnixStream,
        handler: &(dyn Fn(Request, Reply) + Send + Sync),
    ) -> io::Result<()> {
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        tokio::io::BufReader::new(read.take(MAX_REQUEST))
            .read_line(&mut line)
            .await?;
        let request: Request = serde_json::from_str(line.trim())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        handler(request, Reply(tx));
        let response = rx.await.unwrap_or(Response::Done);
        let mut out = serde_json::to_vec(&response)?;
        out.push(b'\n');
        write.write_all(&out).await?;
        write.shutdown().await
    }

    /// Blocking client: send one request and wait for the response.
    pub fn send(socket: &Path, request: &Request) -> io::Result<Response> {
        let mut stream = UnixStream::connect(socket)?;
        let mut out = serde_json::to_vec(request)?;
        out.push(b'\n');
        stream.write_all(&out)?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        if line.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the editor closed the connection without answering",
            ));
        }
        serde_json::from_str(line.trim())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    pub fn client_main() -> Option<i32> {
        let socket = std::env::var_os(SOCKET_ENV).filter(|s| !s.is_empty())?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        let passthrough = args.iter().take_while(|a| *a != "--").any(|a| {
            matches!(
                a.as_str(),
                "-h" | "--help" | "-V" | "--version" | "--health" | "-g" | "--grammar"
            )
        });
        if passthrough {
            return None;
        }
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(err) => {
                eprintln!("vx: cannot determine the current directory ({err})");
                return Some(1);
            }
        };
        let request = request_from_args(args, &cwd);
        Some(match send(Path::new(&socket), &request) {
            Ok(Response::Done) => 0,
            Ok(Response::Error(msg)) => {
                eprintln!("vx: {msg}");
                1
            }
            Err(err) => {
                eprintln!("vx: cannot reach the Vortex that runs this terminal ({err})");
                1
            }
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn strs(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn recording_server(
        respond: impl Fn(Reply) + Send + Sync + 'static,
    ) -> (Server, Arc<Mutex<Vec<Request>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let server = Server::start(move |req, reply| {
            seen2.lock().unwrap().push(req);
            respond(reply);
        })
        .unwrap();
        (server, seen)
    }

    async fn send_async(socket: PathBuf, request: Request) -> io::Result<Response> {
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(move || send(&socket, &request)),
        )
        .await
        .expect("client timed out")
        .unwrap()
    }

    use std::io;

    fn socket_of(server: &Server) -> PathBuf {
        server.dir.path().join("sock")
    }

    #[test]
    fn args_none_is_focus() {
        assert_eq!(
            request_from_args(strs(&[]), Path::new("/w")),
            Request::Focus
        );
        assert_eq!(
            request_from_args(
                strs(&["--vsplit", "-c", "x.toml", "--wait"]),
                Path::new("/w")
            ),
            Request::Focus
        );
    }

    #[test]
    fn args_files_and_flags() {
        let req = request_from_args(
            strs(&[
                "-v",
                "a.rs:10:2",
                "-c",
                "x",
                "--vsplit",
                "b.rs",
                "/abs/c.rs:3",
                "--wait",
            ]),
            Path::new("/w"),
        );
        assert_eq!(
            req,
            Request::Open {
                files: vec![
                    OpenFile {
                        path: "/w/a.rs".into(),
                        line: Some(10),
                        column: Some(2)
                    },
                    OpenFile {
                        path: "/w/b.rs".into(),
                        line: None,
                        column: None
                    },
                    OpenFile {
                        path: "/abs/c.rs".into(),
                        line: Some(3),
                        column: None
                    },
                ],
                wait: true
            }
        );
        let req = request_from_args(strs(&["a"]), Path::new("/w"));
        assert!(matches!(req, Request::Open { wait: false, .. }));
    }

    #[tokio::test]
    async fn round_trip() {
        let (server, seen) = recording_server(|r| r.send(Response::Done));
        let req = Request::Open {
            files: vec![OpenFile {
                path: "/x".into(),
                line: Some(1),
                column: None,
            }],
            wait: false,
        };
        let resp = send_async(socket_of(&server), req.clone()).await.unwrap();
        assert_eq!(resp, Response::Done);
        assert_eq!(*seen.lock().unwrap(), vec![req]);
    }

    #[tokio::test]
    async fn waiting_blocks_until_reply() {
        let (server, _) = recording_server(|r| {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                r.send(Response::Done);
            });
        });
        let start = Instant::now();
        let resp = send_async(socket_of(&server), Request::Focus)
            .await
            .unwrap();
        assert_eq!(resp, Response::Done);
        assert!(start.elapsed() >= Duration::from_millis(290));
    }

    #[tokio::test]
    async fn dropped_reply_is_done_and_error_passes() {
        let (server, _) = recording_server(drop);
        let resp = send_async(socket_of(&server), Request::Focus)
            .await
            .unwrap();
        assert_eq!(resp, Response::Done);
        let (server, _) = recording_server(|r| r.send(Response::Error("nope".into())));
        let resp = send_async(socket_of(&server), Request::Focus)
            .await
            .unwrap();
        assert_eq!(resp, Response::Error("nope".into()));
    }

    #[tokio::test]
    async fn malformed_input_does_not_kill_server() {
        use std::io::Write;
        let (server, _) = recording_server(|r| r.send(Response::Done));
        let socket = socket_of(&server);
        let s2 = socket.clone();
        tokio::task::spawn_blocking(move || {
            let mut c = std::os::unix::net::UnixStream::connect(&s2).unwrap();
            c.write_all(b"not json\n").unwrap();
            let mut buf = String::new();
            let _ = io::Read::read_to_string(&mut c, &mut buf);
            assert!(buf.is_empty());
        })
        .await
        .unwrap();
        let resp = send_async(socket, Request::Focus).await.unwrap();
        assert_eq!(resp, Response::Done);
    }

    #[tokio::test]
    async fn env_and_scripts() {
        use std::os::unix::fs::PermissionsExt;
        let (server, _) = recording_server(drop);
        let env: std::collections::HashMap<_, _> = server.env().into_iter().collect();
        let vx = server.bin_dir().join("vx");
        assert_eq!(env[SOCKET_ENV], socket_of(&server).to_string_lossy());
        for k in ["EDITOR", "VISUAL", "GIT_EDITOR"] {
            assert_eq!(
                env[k],
                format!("{} --wait", sh_quote(&vx.to_string_lossy()))
            );
        }
        // `vx` is the only program the panel puts on PATH
        let names: Vec<_> = std::fs::read_dir(server.bin_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["vx"]);
        let exe = sh_quote(&std::env::current_exe().unwrap().to_string_lossy());
        assert_eq!(
            std::fs::metadata(&vx).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let body = std::fs::read_to_string(&vx).unwrap();
        assert!(body.contains(&format!("exec {exe} \"$@\"")), "{body}");
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/a b/vx"), "'/a b/vx'");
        assert_eq!(sh_quote("/it's/vx"), r"'/it'\''s/vx'");
        let dir = tempfile::tempdir().unwrap();
        let weird = dir.path().join("it's here");
        std::fs::create_dir(&weird).unwrap();
        unix::write_scripts(&weird, Path::new("/bin/echo")).unwrap();
        let out = std::process::Command::new("sh")
            .arg(weird.join("vx"))
            .arg("ok")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
    }

    #[tokio::test]
    async fn drop_removes_directory() {
        let (server, _) = recording_server(drop);
        let dir = server.dir.path().to_path_buf();
        assert!(dir.exists());
        drop(server);
        assert!(!dir.exists());
    }
}
