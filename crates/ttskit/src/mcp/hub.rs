//! The editor port, held for the whole life of the server: one task reads
//! every message the game sends, keeps console output and game events in an
//! [`EventBuffer`] and forwards each message to the tool call that is waiting
//! for replies, if any.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use ttskit_editor::{EditorError, Incoming, Listener, MessageSource};

use super::buffer::{EventBody, EventBuffer, MAX_BYTES, Page, Query};

/// Wait between attempts to bind the port again after the listener failed.
pub(crate) const REBIND_INTERVAL: Duration = Duration::from_secs(2);
/// Consecutive accept failures after which the listener counts as broken.
const MAX_ACCEPT_FAILURES: u32 = 10;
/// Wait after one accept failure before accepting again.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Whether the server can receive messages from the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListenState {
    Listening {
        since: Timestamp,
    },
    /// The listener failed; binding the port again is retried.
    Failed {
        since: Timestamp,
        error: String,
    },
}

struct State {
    buffer: EventBuffer,
    listen: ListenState,
    /// The tool call waiting for replies, with the id of its [`Replies`].
    waiter: Option<(u64, mpsc::UnboundedSender<Incoming>)>,
    next_waiter: u64,
    /// The next "loaded" message (ID 1) answers our "get scripts" request
    /// and is not a load of the game.
    expect_scripts_reply: bool,
    last_message_at: Option<Timestamp>,
    /// Messages that arrived but could not be read or decoded.
    undecodable: u64,
    last_undecodable: Option<String>,
}

pub(crate) struct Hub {
    addr: SocketAddr,
    state: Mutex<State>,
    /// The newest sequence number, for calls that wait for new events.
    head: watch::Sender<u64>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Hub {
    /// A hub without a reading task, in the listening state; [`Hub::start`]
    /// binds the port and starts the task.
    pub(crate) fn new(addr: SocketAddr, max_events: usize) -> Arc<Self> {
        Arc::new(Self {
            addr,
            state: Mutex::new(State {
                buffer: EventBuffer::new(max_events, MAX_BYTES),
                listen: ListenState::Listening {
                    since: Timestamp::now(),
                },
                waiter: None,
                next_waiter: 0,
                expect_scripts_reply: false,
                last_message_at: None,
                undecodable: 0,
                last_undecodable: None,
            }),
            head: watch::Sender::new(0),
            task: Mutex::new(None),
        })
    }

    /// Bind `addr` and read from it until the hub is dropped. A port that
    /// cannot be bound is an error: the server does not start.
    pub(crate) async fn start(
        addr: SocketAddr,
        read_limit: Duration,
        max_events: usize,
    ) -> Result<Arc<Self>, EditorError> {
        let listener = Listener::bind(addr).await?;
        let hub = Self::new(addr, max_events);
        let task = tokio::spawn(read_loop(Arc::downgrade(&hub), listener, read_limit));
        *hub.task.lock().expect("task lock") = Some(task);
        Ok(hub)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves plain data behind; keep going.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record one message from the game: buffer it if it is an event, and
    /// forward it to the waiting tool call. Called by the one reading task only,
    /// so messages keep their order.
    pub(crate) fn dispatch(&self, message: Incoming) {
        let now = Timestamp::now();
        let mut st = self.lock();
        st.last_message_at = Some(now);
        let reply = matches!(message, Incoming::LoadGame { .. })
            && std::mem::take(&mut st.expect_scripts_reply);
        let (body, forward) = if st.waiter.is_some() {
            let body = (!reply).then(|| EventBody::from_incoming(message.clone()));
            (body.flatten(), Some(message))
        } else {
            let body = (!reply).then(|| EventBody::from_incoming(message));
            (body.flatten(), None)
        };
        if let Some(body) = body {
            let seq = st.buffer.push(now, body);
            self.head.send_replace(seq);
        }
        if let (Some(message), Some((_, tx))) = (forward, st.waiter.as_ref()) {
            let _ = tx.send(message);
        }
    }

    fn note_undecodable(&self, e: &EditorError) {
        let mut st = self.lock();
        st.last_message_at = Some(Timestamp::now());
        st.undecodable += 1;
        st.last_undecodable = Some(e.to_string());
    }

    fn set_failed(&self, error: String) {
        let mut st = self.lock();
        if !matches!(&st.listen, ListenState::Failed { error: e, .. } if *e == error) {
            st.listen = ListenState::Failed {
                since: Timestamp::now(),
                error,
            };
        }
        // No reply can come; a waiting call sees its channel close.
        st.waiter = None;
        st.expect_scripts_reply = false;
    }

    fn set_listening(&self) {
        self.lock().listen = ListenState::Listening {
            since: Timestamp::now(),
        };
    }

    pub(crate) fn listen_state(&self) -> ListenState {
        self.lock().listen.clone()
    }

    /// An error for a tool call that needs replies while the listener is down.
    pub(crate) fn not_listening(&self) -> Option<String> {
        match self.listen_state() {
            ListenState::Listening { .. } => None,
            ListenState::Failed { error, .. } => Some(format!(
                "not listening on {}: {error}; binding it again is retried every {} s",
                self.addr,
                REBIND_INTERVAL.as_secs()
            )),
        }
    }

    /// Start receiving every message the game sends, until the returned
    /// value is dropped. Tool calls are serialized, so there is one at a time;
    /// a new one replaces an older one.
    pub(crate) fn replies(&self) -> Replies<'_> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut st = self.lock();
        st.next_waiter += 1;
        let id = st.next_waiter;
        st.waiter = Some((id, tx));
        Replies {
            hub: self,
            id,
            rx: tokio::sync::Mutex::new(rx),
        }
    }

    pub(crate) fn query(&self, q: &Query) -> Page {
        self.lock().buffer.query(q)
    }

    pub(crate) fn last_seq(&self) -> u64 {
        self.lock().buffer.last_seq()
    }

    /// Changes whenever an event is buffered.
    pub(crate) fn watch_head(&self) -> watch::Receiver<u64> {
        self.head.subscribe()
    }

    /// The listener and buffer part of `get_status`.
    pub(crate) fn status(&self) -> Value {
        let st = self.lock();
        let listener = match &st.listen {
            ListenState::Listening { since } => json!({
                "state": "listening",
                "address": self.addr.to_string(),
                "since": since.to_string(),
            }),
            ListenState::Failed { since, error } => json!({
                "state": "failed",
                "address": self.addr.to_string(),
                "since": since.to_string(),
                "error": error,
            }),
        };
        json!({
            "listener": listener,
            "lastMessageAt": st.last_message_at.map(|t| t.to_string()),
            "buffer": {
                "events": st.buffer.len(),
                "bytes": st.buffer.bytes(),
                "lastSeq": st.buffer.last_seq(),
                "evicted": st.buffer.evicted(),
            },
            "undecodable": st.undecodable,
            "lastUndecodable": st.last_undecodable,
        })
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().ok().and_then(Option::take) {
            task.abort();
        }
    }
}

/// The messages a tool call receives: everything the game sends while it
/// exists, including what also goes into the buffer.
pub(crate) struct Replies<'a> {
    hub: &'a Hub,
    id: u64,
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Incoming>>,
}

impl Replies<'_> {
    /// The next "loaded" message answers a "get scripts" request about to be
    /// sent, so it is not buffered as a load of the game.
    pub(crate) fn expect_scripts_reply(&self) {
        self.hub.lock().expect_scripts_reply = true;
    }
}

impl Drop for Replies<'_> {
    fn drop(&mut self) {
        let mut st = self.hub.lock();
        if st.waiter.as_ref().is_some_and(|(id, _)| *id == self.id) {
            st.waiter = None;
            st.expect_scripts_reply = false;
        }
    }
}

impl MessageSource for Replies<'_> {
    async fn next_message(&self, _read_limit: Duration) -> Result<Incoming, EditorError> {
        // The read limit applies to reading one connection, which the hub's
        // task does; waiting for a message has no limit, as for a Listener.
        match self.rx.lock().await.recv().await {
            Some(message) => Ok(message),
            None => Err(EditorError::Io(std::io::Error::other(format!(
                "stopped listening on {}",
                self.hub.addr
            )))),
        }
    }
}

/// Read messages until the hub is gone. Accept failures in a row mean the
/// listener is broken: it is dropped and the port is bound again.
async fn read_loop(hub: Weak<Hub>, mut listener: Listener, read_limit: Duration) {
    let mut failures = 0;
    loop {
        let result = listener.next_message(read_limit).await;
        let Some(h) = hub.upgrade() else { return };
        match result {
            Ok(message) => {
                failures = 0;
                h.dispatch(message);
            }
            Err(EditorError::Accept(e)) => {
                failures += 1;
                if failures < MAX_ACCEPT_FAILURES {
                    drop(h);
                    tokio::time::sleep(ACCEPT_RETRY).await;
                    continue;
                }
                let addr = h.addr;
                h.set_failed(format!("cannot accept connections: {e}"));
                drop(h);
                drop(listener);
                let Some(l) = rebind(&hub, addr, REBIND_INTERVAL).await else {
                    return;
                };
                listener = l;
                failures = 0;
            }
            Err(e) => {
                failures = 0;
                h.note_undecodable(&e);
            }
        }
    }
}

/// Bind `addr` again every `interval` until it works (the hub is then
/// listening) or the hub is gone (None).
pub(crate) async fn rebind(
    hub: &Weak<Hub>,
    addr: SocketAddr,
    interval: Duration,
) -> Option<Listener> {
    loop {
        tokio::time::sleep(interval).await;
        let result = Listener::bind(addr).await;
        let h = hub.upgrade()?;
        match result {
            Ok(listener) => {
                h.set_listening();
                return Some(listener);
            }
            Err(e) => h.set_failed(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        ([127, 0, 0, 1], 1).into()
    }

    fn print(text: &str) -> Incoming {
        Incoming::Print {
            message: text.into(),
        }
    }

    fn load() -> Incoming {
        Incoming::LoadGame {
            script_states: vec![],
            save_path: None,
        }
    }

    #[tokio::test]
    async fn rebind_retries_while_the_port_is_occupied() {
        let occupier = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = occupier.local_addr().unwrap();
        let hub = Hub::new(addr, 10);
        let task = tokio::spawn({
            let weak = Arc::downgrade(&hub);
            async move { rebind(&weak, addr, Duration::from_millis(50)).await }
        });
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!task.is_finished());
        match hub.listen_state() {
            ListenState::Failed { error, .. } => {
                assert!(error.contains("already in use"), "{error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(hub.not_listening().is_some());

        drop(occupier);
        let listener = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("rebind did not finish")
            .unwrap();
        assert!(listener.is_some());
        assert!(matches!(hub.listen_state(), ListenState::Listening { .. }));
        assert!(hub.not_listening().is_none());
    }

    #[tokio::test]
    async fn rebind_returns_none_once_the_hub_is_gone() {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = free.local_addr().unwrap();
        drop(free);
        let hub = Hub::new(addr, 10);
        let weak = Arc::downgrade(&hub);
        drop(hub);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            rebind(&weak, addr, Duration::from_millis(20)),
        )
        .await
        .expect("rebind did not finish");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn a_second_replies_replaces_the_first_and_dropping_it_unregisters() {
        let hub = Hub::new(addr(), 10);
        let first = hub.replies();
        let second = hub.replies();
        assert!(first.next_message(Duration::from_secs(1)).await.is_err());

        hub.dispatch(print("one"));
        let got = second.next_message(Duration::from_secs(1)).await.unwrap();
        assert!(matches!(got, Incoming::Print { message } if message == "one"));

        drop(second);
        drop(first); // an older Replies must not unregister anything
        hub.dispatch(print("two"));
        assert_eq!(hub.last_seq(), 2);
        assert!(hub.lock().waiter.is_none());
    }

    #[tokio::test]
    async fn set_failed_ends_a_waiting_call() {
        let hub = Hub::new(addr(), 10);
        let replies = hub.replies();
        let (result, ()) = tokio::join!(
            tokio::time::timeout(
                Duration::from_secs(5),
                replies.next_message(Duration::from_secs(1))
            ),
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                hub.set_failed("broken".into());
            }
        );
        assert!(result.expect("still waiting").is_err());
        assert!(hub.not_listening().unwrap().contains("broken"));
    }

    #[tokio::test]
    async fn the_scripts_reply_is_forwarded_but_not_buffered_and_only_once() {
        let hub = Hub::new(addr(), 10);
        let replies = hub.replies();
        replies.expect_scripts_reply();
        hub.dispatch(load());
        assert!(matches!(
            replies.next_message(Duration::from_secs(1)).await.unwrap(),
            Incoming::LoadGame { .. }
        ));
        assert_eq!(hub.last_seq(), 0);

        // The flag is used up: the next load is an event.
        hub.dispatch(load());
        assert!(matches!(
            replies.next_message(Duration::from_secs(1)).await.unwrap(),
            Incoming::LoadGame { .. }
        ));
        assert_eq!(hub.last_seq(), 1);
    }

    #[tokio::test]
    async fn a_load_without_the_flag_is_buffered() {
        let hub = Hub::new(addr(), 10);
        hub.dispatch(load());
        assert_eq!(hub.last_seq(), 1);
    }

    #[tokio::test]
    async fn messages_that_are_no_events_are_forwarded_but_not_buffered() {
        let hub = Hub::new(addr(), 10);
        let replies = hub.replies();
        hub.dispatch(Incoming::Return {
            return_value: json!(1),
        });
        assert!(matches!(
            replies.next_message(Duration::from_secs(1)).await.unwrap(),
            Incoming::Return { .. }
        ));
        assert_eq!(hub.last_seq(), 0);
        assert!(hub.status()["lastMessageAt"].is_string());
    }
}
