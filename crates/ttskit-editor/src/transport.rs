//! Transport: one JSON document per TCP connection, terminated by the sender
//! closing the connection (see `docs/editor-api.md`).

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::message::{CodecError, Incoming, Outgoing};

/// Port the game listens on for messages from the editor.
pub const GAME_PORT: u16 = 39999;
/// Port the editor must listen on for messages from the game.
pub const EDITOR_PORT: u16 = 39998;

/// Default timeout for connecting, writing and reading one message.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound for one incoming message; protects against a peer that never closes.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error(
        "cannot listen on {addr}: the port is already in use (another editor integration may be running)"
    )]
    PortInUse { addr: SocketAddr },
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("cannot connect to the game at {addr}: {source} (is Tabletop Simulator running?)")]
    Connect {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("incoming message exceeds {MAX_MESSAGE_BYTES} bytes")]
    TooLarge,
    #[error("incoming message is not valid UTF-8")]
    NotUtf8,
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

async fn within<T>(
    limit: Duration,
    fut: impl Future<Output = std::io::Result<T>>,
) -> Result<T, EditorError> {
    match timeout(limit, fut).await {
        Ok(r) => Ok(r?),
        Err(_) => Err(EditorError::Timeout(limit)),
    }
}

/// Send one message to the game at `addr`.
pub async fn send(
    addr: SocketAddr,
    message: &Outgoing,
    limit: Duration,
) -> Result<(), EditorError> {
    let mut stream = match timeout(limit, TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(source)) => return Err(EditorError::Connect { addr, source }),
        Err(_) => return Err(EditorError::Timeout(limit)),
    };
    let bytes = message.to_json().into_bytes();
    within(limit, async {
        stream.write_all(&bytes).await?;
        stream.shutdown().await
    })
    .await
}

/// Listens for messages from the game.
pub struct Listener {
    inner: TcpListener,
}

impl Listener {
    /// Bind to `addr`. Reports a clear error if the port is taken.
    pub async fn bind(addr: SocketAddr) -> Result<Self, EditorError> {
        match TcpListener::bind(addr).await {
            Ok(inner) => Ok(Self { inner }),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                Err(EditorError::PortInUse { addr })
            }
            Err(source) => Err(EditorError::Bind { addr, source }),
        }
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// Wait for the next message. Waiting for a connection has no limit (wrap in
    /// `tokio::time::timeout` if needed); reading the message is bounded by
    /// `read_limit`. A message that fails to decode is returned as an error and
    /// does not stop the listener; call again for the next one.
    pub async fn next_message(&self, read_limit: Duration) -> Result<Incoming, EditorError> {
        let (mut stream, _) = self.inner.accept().await?;
        let buf = within(read_limit, async {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 64 * 1024];
            loop {
                let n = stream.read(&mut chunk).await?;
                if n == 0 {
                    return Ok(Some(buf));
                }
                if buf.len() + n > MAX_MESSAGE_BYTES {
                    return Ok(None);
                }
                buf.extend_from_slice(&chunk[..n]);
            }
        })
        .await?
        .ok_or(EditorError::TooLarge)?;
        let text = String::from_utf8(buf).map_err(|_| EditorError::NotUtf8)?;
        Ok(Incoming::parse(&text)?)
    }
}
