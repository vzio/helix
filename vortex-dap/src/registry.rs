use crate::{Client, Error, Payload, Result, StackFrame};
use anyhow::anyhow;
use futures_executor::block_on;
use futures_util::stream::SelectAll;
use slotmap::SlotMap;
use std::fmt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use vortex_core::syntax::config::DebugAdapterConfig;

/// The resgistry is a struct that manages and owns multiple debugger clients
/// This holds the responsibility of managing the lifecycle of each client
/// plus showing the heirarcihical nature betweeen them
pub struct Registry {
    inner: SlotMap<DebugAdapterId, Client>,
    /// The active debugger client
    ///
    /// TODO: You can have multiple active debuggers, so the concept of a single active debugger
    /// may need to be changed
    current_client_id: Option<DebugAdapterId>,
    /// A stream of incoming messages from all debuggers
    pub incoming: SelectAll<UnboundedReceiverStream<(DebugAdapterId, Payload)>>,
}

impl Registry {
    /// Creates a new DebuggerService instance
    pub fn new() -> Self {
        Self {
            inner: SlotMap::with_key(),
            current_client_id: None,
            incoming: SelectAll::new(),
        }
    }

    pub fn start_client(
        &mut self,
        socket: Option<std::net::SocketAddr>,
        config: &DebugAdapterConfig,
    ) -> Result<DebugAdapterId> {
        self.start(socket, config, None, config.name.clone())
    }

    /// Starts the session of a `startDebugging` request of `parent`.
    ///
    /// When the parent was reached over TCP, the child connects to the same socket. Otherwise a
    /// new adapter process is started from the parent's configuration. The child is initialized
    /// but not launched: that is up to the caller.
    pub fn start_child(
        &mut self,
        parent: DebugAdapterId,
        configuration: &serde_json::Value,
    ) -> Result<DebugAdapterId> {
        let parent_client = self
            .inner
            .get(parent)
            .ok_or_else(|| Error::Other(anyhow!("the parent debug session is gone")))?;
        let config = parent_client.config.clone().ok_or_else(|| {
            Error::Other(anyhow!("the parent debug session has no configuration"))
        })?;
        let socket = parent_client.socket;
        let name = configuration
            .get("name")
            .and_then(|name| name.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| parent_client.name.clone());

        self.start(socket, &config, Some(parent), name)
    }

    fn start(
        &mut self,
        socket: Option<std::net::SocketAddr>,
        config: &DebugAdapterConfig,
        parent: Option<DebugAdapterId>,
        name: String,
    ) -> Result<DebugAdapterId> {
        self.inner.try_insert_with_key(|id| {
            let result = match socket {
                Some(socket) => block_on(Client::tcp(socket, id)),
                None => block_on(Client::process(
                    &config.transport,
                    &config.command,
                    config.args.iter().map(|arg| arg.as_str()).collect(),
                    config.port_arg.as_deref(),
                    id,
                )),
            };

            let (mut client, receiver) = result?;
            self.incoming.push(UnboundedReceiverStream::new(receiver));

            client.config = Some(config.clone());
            client.parent = parent;
            client.name = name;
            block_on(client.initialize(config.name.clone()))?;
            client.quirks = config.quirks.clone();

            Ok(client)
        })
    }

    /// Removes a client. When it was the active one, no client is active afterwards.
    pub fn remove_client(&mut self, id: DebugAdapterId) {
        self.inner.remove(id);
        if self.current_client_id == Some(id) {
            self.current_client_id = None;
        }
    }

    /// All the clients with their ids.
    pub fn iter(&self) -> impl Iterator<Item = (DebugAdapterId, &Client)> {
        self.inner.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The sessions started by `id`.
    pub fn children(&self, id: DebugAdapterId) -> Vec<DebugAdapterId> {
        self.inner
            .iter()
            .filter(|(_, client)| client.parent == Some(id))
            .map(|(child, _)| child)
            .collect()
    }

    /// The session at the top of the parents of `id` (`id` itself if it has none).
    pub fn root(&self, id: DebugAdapterId) -> DebugAdapterId {
        let mut root = id;
        // The bound guards against a cycle, which can't be made through `start_child`.
        for _ in 0..self.inner.len() {
            match self.inner.get(root).and_then(|client| client.parent) {
                Some(parent) if self.inner.contains_key(parent) => root = parent,
                _ => break,
            }
        }
        root
    }

    pub fn get_client(&self, id: DebugAdapterId) -> Option<&Client> {
        self.inner.get(id)
    }

    pub fn get_client_mut(&mut self, id: DebugAdapterId) -> Option<&mut Client> {
        self.inner.get_mut(id)
    }

    pub fn get_active_client(&self) -> Option<&Client> {
        self.current_client_id.and_then(|id| self.get_client(id))
    }

    pub fn get_active_client_mut(&mut self) -> Option<&mut Client> {
        self.current_client_id
            .and_then(|id| self.get_client_mut(id))
    }

    pub fn set_active_client(&mut self, id: DebugAdapterId) {
        if self.get_client(id).is_some() {
            self.current_client_id = Some(id);
        } else {
            self.current_client_id = None;
        }
    }

    pub fn unset_active_client(&mut self) {
        self.current_client_id = None;
    }

    pub fn current_stack_frame(&self) -> Option<&StackFrame> {
        self.get_active_client()
            .and_then(|debugger| debugger.current_stack_frame())
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

slotmap::new_key_type! {
    pub struct DebugAdapterId;
}

impl fmt::Display for DebugAdapterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };

    fn config() -> DebugAdapterConfig {
        DebugAdapterConfig {
            name: "fake".into(),
            transport: "tcp".into(),
            command: String::new(),
            args: vec![],
            port_arg: None,
            templates: vec![],
            quirks: Default::default(),
            std_paths: None,
            lib_paths: None,
            capabilities: None,
        }
    }

    /// Answers `initialize` on every connection.
    async fn fake_adapter(listener: TcpListener) {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);
                loop {
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).await.unwrap() == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(value) = line.trim().strip_prefix("Content-Length: ") {
                            length = value.parse().unwrap();
                        }
                    }
                    let mut content = vec![0; length];
                    reader.read_exact(&mut content).await.unwrap();
                    let request: serde_json::Value = serde_json::from_slice(&content).unwrap();
                    assert_eq!(request["command"], "initialize");
                    assert_eq!(request["arguments"]["supportsStartDebuggingRequest"], true);
                    let response = json!({
                        "type": "response",
                        "request_seq": request["seq"],
                        "success": true,
                        "command": "initialize",
                        "body": {"supportsConfigurationDoneRequest": true},
                    })
                    .to_string();
                    let framed = format!("Content-Length: {}\r\n\r\n{response}", response.len());
                    writer.write_all(framed.as_bytes()).await.unwrap();
                }
            });
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_child_connects_to_the_socket_of_its_parent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket = listener.local_addr().unwrap();
        tokio::spawn(fake_adapter(listener));

        let mut registry = Registry::new();
        assert!(registry.is_empty());
        let parent = registry.start_client(Some(socket), &config()).unwrap();
        let child = registry
            .start_child(parent, &json!({"name": "child session", "type": "x"}))
            .unwrap();
        let grandchild = registry.start_child(child, &json!({})).unwrap();

        let client = registry.get_client(child).unwrap();
        assert_eq!(client.parent, Some(parent));
        assert_eq!(client.socket, Some(socket));
        assert_eq!(client.name, "child session");
        assert!(!client.configured);
        assert!(client
            .capabilities()
            .supports_configuration_done_request
            .unwrap());
        assert_eq!(registry.get_client(parent).unwrap().name, "fake");
        assert_eq!(
            registry.get_client(grandchild).unwrap().name,
            "child session"
        );

        assert_eq!(registry.children(parent), vec![child]);
        assert_eq!(registry.root(grandchild), parent);
        assert_eq!(registry.root(parent), parent);
        assert_eq!(registry.iter().count(), 3);

        registry.set_active_client(child);
        registry.remove_client(child);
        assert!(registry.get_active_client().is_none());
        assert_eq!(registry.iter().count(), 2);
    }

    #[test]
    fn a_child_needs_a_parent() {
        let mut registry = Registry::new();
        assert!(registry
            .start_child(DebugAdapterId::default(), &json!({}))
            .is_err());
    }
}
