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

/// How long [`Listener::bind`] waits for a connection to the port it is about
/// to bind. A local listener accepts within a millisecond; a port nobody
/// listens on is only reported as refused after about 2 s on Windows, so the
/// probe treats a timeout as "nobody listens".
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error(
        "cannot listen on {addr}: the port is already in use (another editor integration may be running)"
    )]
    PortInUse { addr: SocketAddr },
    /// Accepting a connection failed; the listener itself may be broken.
    #[error("cannot accept a connection: {0}")]
    Accept(std::io::Error),
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
    #[error("a custom message must be a JSON object; the game ignores anything else")]
    CustomNotObject,
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
    ///
    /// Before binding, it connects to `addr` (unless the port is 0) and
    /// reports the port as in use if something accepts within
    /// [`PROBE_TIMEOUT`]. On Windows, binding `127.0.0.1` succeeds while
    /// another program listens on the wildcard address (`0.0.0.0`, or `::`
    /// with dual stack, the Node.js default) and then takes all of its
    /// connections, without an error on either side. The probe is an empty
    /// connection, closed at once.
    pub async fn bind(addr: SocketAddr) -> Result<Self, EditorError> {
        if addr.port() != 0 && someone_listens(addr).await {
            return Err(EditorError::PortInUse { addr });
        }
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
    /// does not stop the listener; call again for the next one. A connection
    /// that closes without sending anything (such as the probe of
    /// [`Listener::bind`] in another process) is skipped.
    pub async fn next_message(&self, read_limit: Duration) -> Result<Incoming, EditorError> {
        loop {
            let (stream, _) = self.inner.accept().await.map_err(EditorError::Accept)?;
            let buf = read_document(stream, read_limit).await?;
            if buf.is_empty() {
                continue;
            }
            let text = String::from_utf8(buf).map_err(|_| EditorError::NotUtf8)?;
            return Ok(Incoming::parse(&text)?);
        }
    }
}

/// True if something accepts a connection to `addr` within [`PROBE_TIMEOUT`].
async fn someone_listens(addr: SocketAddr) -> bool {
    matches!(
        timeout(PROBE_TIMEOUT, TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// Read one document: everything until the peer closes the connection.
async fn read_document(
    mut stream: TcpStream,
    read_limit: Duration,
) -> Result<Vec<u8>, EditorError> {
    within(read_limit, async {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
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
    .ok_or(EditorError::TooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_probe_sees_a_listener_and_a_free_port() {
        let held = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = held.local_addr().unwrap();
        assert!(someone_listens(addr).await);
        drop(held);
        let started = tokio::time::Instant::now();
        assert!(!someone_listens(addr).await);
        // A refused connection takes about 2 s on Windows; the probe must not wait for it.
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn binding_a_port_that_someone_listens_on_is_port_in_use() {
        let held = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = held.local_addr().unwrap();
        let err = Listener::bind(addr).await.err().unwrap();
        assert!(matches!(err, EditorError::PortInUse { .. }), "{err:?}");
    }
}
