//! A fake game server for tests, speaking the same one-document-per-connection
//! protocol as the real game. It needs no running Tabletop Simulator.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::message::{Incoming, ScriptState};

/// Raw JSON the fake game received, in order.
type Received = Arc<Mutex<Vec<Value>>>;

/// Decides what the fake game sends back for a received message that is not a
/// "get scripts" request: it returns the raw documents to push to the editor.
pub type Responder = Box<dyn Fn(&Value) -> Vec<Value> + Send + Sync>;

pub struct FakeGame {
    addr: SocketAddr,
    received: Received,
    task: JoinHandle<()>,
}

impl FakeGame {
    /// Start on an ephemeral localhost port. On a "get scripts" request it
    /// connects to `editor_addr` and replies with a `LoadGame` holding `scripts`.
    pub async fn start(
        editor_addr: SocketAddr,
        scripts: Vec<ScriptState>,
    ) -> std::io::Result<Self> {
        Self::start_with(editor_addr, scripts, Box::new(|_| Vec::new())).await
    }

    /// Like [`FakeGame::start`], and every other received message is passed to
    /// `responder`, whose documents are pushed to the editor in order.
    pub async fn start_with(
        editor_addr: SocketAddr,
        scripts: Vec<ScriptState>,
        responder: Responder,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let received: Received = Arc::default();
        let log = received.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = Vec::new();
                if stream.read_to_end(&mut buf).await.is_err() {
                    continue;
                }
                let Ok(value) = serde_json::from_slice::<Value>(&buf) else {
                    continue;
                };
                let is_get_scripts = value["messageID"] == 0;
                log.lock().expect("lock").push(value.clone());
                if !is_get_scripts {
                    for doc in responder(&value) {
                        let _ = push_raw(editor_addr, &doc).await;
                    }
                }
                if is_get_scripts {
                    let reply = serde_json::json!({
                        "scriptStates": scripts,
                        "savePath": "",
                        "messageID": 1
                    });
                    let _ = push_raw(editor_addr, &reply).await;
                }
            }
        });
        Ok(Self {
            addr,
            received,
            task,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Everything received so far.
    pub fn received(&self) -> Vec<Value> {
        self.received.lock().expect("lock").clone()
    }

    /// Push an arbitrary raw JSON document to the editor, as the real game does
    /// (pretty-printed, CRLF line endings, connection closed afterwards).
    pub async fn push_raw(&self, editor_addr: SocketAddr, doc: &Value) -> std::io::Result<()> {
        push_raw(editor_addr, doc).await
    }

    /// Push a typed message to the editor.
    pub async fn push(&self, editor_addr: SocketAddr, msg: &Incoming) -> std::io::Result<()> {
        let doc = match msg {
            Incoming::Print { message } => serde_json::json!({"messageID": 2, "message": message}),
            Incoming::Return { return_value } => {
                serde_json::json!({"messageID": 5, "returnValue": return_value})
            }
            Incoming::Custom { custom_message } => {
                serde_json::json!({"messageID": 4, "customMessage": custom_message})
            }
            Incoming::ObjectCreated { guid } => serde_json::json!({"messageID": 7, "guid": guid}),
            Incoming::GameSaved { .. } => serde_json::json!({"messageID": 6}),
            other => {
                return Err(std::io::Error::other(format!(
                    "FakeGame::push does not support {other:?}; use push_raw"
                )));
            }
        };
        push_raw(editor_addr, &doc).await
    }
}

impl Drop for FakeGame {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn push_raw(editor_addr: SocketAddr, doc: &Value) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(doc)
        .expect("value serializes")
        .replace('\n', "\r\n");
    let mut stream = TcpStream::connect(editor_addr).await?;
    stream.write_all(text.as_bytes()).await?;
    stream.shutdown().await
}
