//! A bounded ring buffer of game events, for the MCP server's console tool.

use std::collections::VecDeque;

use serde_json::{Value, json};
use ttskit_editor::{ConsoleEvent, Incoming, LuaError};

use crate::editor::event_json;

/// Kinds of buffered events; also the values of get_console's `kinds` filter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[schemars(crate = "rmcp::schemars")]
pub(crate) enum EventKind {
    Print,
    Error,
    Custom,
    Saved,
    Loaded,
}

impl EventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::Error => "error",
            Self::Custom => "custom",
            Self::Saved => "saved",
            Self::Loaded => "loaded",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EventBody {
    Print {
        message: String,
    },
    Error(LuaError),
    Custom {
        value: Value,
    },
    /// ID 6.
    Saved {
        save_path: Option<String>,
    },
    /// ID 1, without the scripts: only the path and how many objects the game listed.
    Loaded {
        save_path: Option<String>,
        objects: usize,
    },
}

fn non_empty(path: Option<String>) -> Option<String> {
    path.filter(|p| !p.is_empty())
}

impl EventBody {
    /// The buffered form of `message`, or None for kinds that are not buffered.
    /// An empty save path becomes None.
    pub(crate) fn from_incoming(message: Incoming) -> Option<Self> {
        match message {
            Incoming::Print { message } => Some(Self::Print { message }),
            Incoming::Error {
                error,
                guid,
                error_message_prefix,
            } => Some(Self::Error(LuaError {
                guid,
                error,
                prefix: error_message_prefix,
            })),
            Incoming::Custom { custom_message } => Some(Self::Custom {
                value: custom_message,
            }),
            Incoming::GameSaved { save_path } => Some(Self::Saved {
                save_path: non_empty(save_path),
            }),
            Incoming::LoadGame {
                script_states,
                save_path,
            } => Some(Self::Loaded {
                save_path: non_empty(save_path),
                objects: script_states.len(),
            }),
            Incoming::PushObject { .. }
            | Incoming::Return { .. }
            | Incoming::ObjectCreated { .. }
            | Incoming::Unknown { .. } => None,
        }
    }

    pub(crate) fn kind(&self) -> EventKind {
        match self {
            Self::Print { .. } => EventKind::Print,
            Self::Error(_) => EventKind::Error,
            Self::Custom { .. } => EventKind::Custom,
            Self::Saved { .. } => EventKind::Saved,
            Self::Loaded { .. } => EventKind::Loaded,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event {
    pub seq: u64,
    pub time: jiff::Timestamp,
    pub body: EventBody,
}

impl Event {
    /// Size counted against the byte bound: the text it holds.
    pub(crate) fn bytes(&self) -> usize {
        match &self.body {
            EventBody::Print { message } => message.len(),
            EventBody::Error(e) => e.guid.len() + e.error.len() + e.prefix.len(),
            EventBody::Custom { value } => value.to_string().len(),
            EventBody::Saved { save_path } | EventBody::Loaded { save_path, .. } => {
                save_path.as_ref().map_or(0, String::len)
            }
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut value = match &self.body {
            EventBody::Print { message } => event_json(
                &self.time,
                &ConsoleEvent::Print {
                    message: message.clone(),
                },
            ),
            EventBody::Error(e) => event_json(&self.time, &ConsoleEvent::Error(e.clone())),
            EventBody::Custom { value } => json!({
                "time": self.time.to_string(),
                "type": self.body.kind().as_str(),
                "customMessage": value,
            }),
            EventBody::Saved { save_path } => json!({
                "time": self.time.to_string(),
                "type": self.body.kind().as_str(),
                "savePath": save_path,
            }),
            EventBody::Loaded { save_path, objects } => json!({
                "time": self.time.to_string(),
                "type": self.body.kind().as_str(),
                "savePath": save_path,
                "objectsWithScripts": objects,
            }),
        };
        value["seq"] = json!(self.seq);
        value
    }
}

/// Default of `ttskit mcp --console-buffer`.
#[cfg(test)]
pub(crate) const DEFAULT_MAX_EVENTS: usize = 1000;
pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;

pub(crate) struct EventBuffer {
    events: VecDeque<Event>,
    total_bytes: usize,
    max_events: usize,
    max_bytes: usize,
    last_seq: u64,
    evicted: u64,
}

pub(crate) struct Query {
    pub since: Option<u64>,
    pub kinds: Option<Vec<EventKind>>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Page {
    /// Matching events, in seq order.
    pub events: Vec<Event>,
    /// Pass as `since` next time: every event up to it was returned or filtered out.
    pub next_since: u64,
    /// With `since`: more matching events follow `next_since` (the limit was reached).
    pub more: bool,
    /// Without `since`: matching events older than the returned ones, left out by the limit.
    pub omitted: usize,
    /// Events after `since` (0 without it) that were evicted before this query.
    pub dropped: u64,
}

impl EventBuffer {
    /// Bounds are clamped: `max_events` below 1 is treated as 1.
    pub(crate) fn new(max_events: usize, max_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            total_bytes: 0,
            max_events: max_events.max(1),
            max_bytes,
            last_seq: 0,
            evicted: 0,
        }
    }

    /// Append with the next sequence number (the first event is 1) and return it.
    pub(crate) fn push(&mut self, time: jiff::Timestamp, body: EventBody) -> u64 {
        self.last_seq += 1;
        let event = Event {
            seq: self.last_seq,
            time,
            body,
        };
        self.total_bytes += event.bytes();
        self.events.push_back(event);
        while self.events.len() > self.max_events
            || (self.total_bytes > self.max_bytes && self.events.len() > 1)
        {
            if let Some(old) = self.events.pop_front() {
                self.total_bytes -= old.bytes();
                self.evicted += 1;
            }
        }
        self.last_seq
    }

    /// Seq of the newest event ever pushed; 0 before the first.
    pub(crate) fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.total_bytes
    }

    /// Events evicted since the buffer was created.
    pub(crate) fn evicted(&self) -> u64 {
        self.evicted
    }

    pub(crate) fn query(&self, q: &Query) -> Page {
        let wanted = |e: &&Event| {
            q.kinds
                .as_ref()
                .is_none_or(|kinds| kinds.contains(&e.body.kind()))
        };
        let s = q.since.unwrap_or(0);
        let oldest = self.events.front().map_or(self.last_seq + 1, |e| e.seq);
        let dropped = (oldest - 1).saturating_sub(s);

        match q.since {
            Some(since) => {
                let mut matching = self.events.iter().filter(|e| e.seq > since).filter(wanted);
                let events: Vec<Event> = matching.by_ref().take(q.limit).cloned().collect();
                let more = matching.next().is_some();
                let next_since = if more {
                    events.last().map_or(since, |e| e.seq)
                } else {
                    since.max(self.last_seq)
                };
                Page {
                    events,
                    next_since,
                    more,
                    omitted: 0,
                    dropped,
                }
            }
            None => {
                let matching: Vec<&Event> = self.events.iter().filter(wanted).collect();
                let skip = matching.len().saturating_sub(q.limit);
                Page {
                    events: matching[skip..].iter().map(|e| (*e).clone()).collect(),
                    next_since: self.last_seq,
                    more: false,
                    omitted: skip,
                    dropped,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ttskit_editor::ScriptState;

    use super::*;

    fn ts(n: i64) -> jiff::Timestamp {
        jiff::Timestamp::from_second(1_700_000_000 + n).unwrap()
    }

    fn print(m: &str) -> EventBody {
        EventBody::Print {
            message: m.to_string(),
        }
    }

    fn custom(n: u64) -> EventBody {
        EventBody::Custom { value: json!(n) }
    }

    fn big() -> EventBuffer {
        EventBuffer::new(1000, MAX_BYTES)
    }

    fn q(since: Option<u64>, limit: usize) -> Query {
        Query {
            since,
            kinds: None,
            limit,
        }
    }

    fn seqs(page: &Page) -> Vec<u64> {
        page.events.iter().map(|e| e.seq).collect()
    }

    #[test]
    fn seq_starts_at_one_and_order_is_kept() {
        let mut b = big();
        assert_eq!(b.last_seq(), 0);
        assert_eq!(b.push(ts(0), print("a")), 1);
        assert_eq!(b.push(ts(1), print("b")), 2);
        assert_eq!(b.push(ts(2), print("c")), 3);
        assert_eq!(b.last_seq(), 3);
        let page = b.query(&q(Some(0), 10));
        assert_eq!(seqs(&page), [1, 2, 3]);
        assert_eq!(page.events[1].body, print("b"));
        assert_eq!(page.events[1].time, ts(1));
    }

    #[test]
    fn count_bound_evicts_oldest() {
        let mut b = EventBuffer::new(3, MAX_BYTES);
        for i in 0..5 {
            b.push(ts(i), print("xy"));
        }
        assert_eq!(b.len(), 3);
        assert_eq!(b.evicted(), 2);
        assert_eq!(b.bytes(), 6);
        assert_eq!(seqs(&b.query(&q(Some(0), 10))), [3, 4, 5]);
    }

    #[test]
    fn zero_max_events_is_clamped_to_one() {
        let mut b = EventBuffer::new(0, MAX_BYTES);
        b.push(ts(0), print("a"));
        b.push(ts(1), print("b"));
        assert_eq!(b.len(), 1);
        assert_eq!(seqs(&b.query(&q(Some(0), 10))), [2]);
    }

    #[test]
    fn byte_bound_evicts_oldest() {
        let mut b = EventBuffer::new(100, 10);
        b.push(ts(0), print("aaaa"));
        b.push(ts(1), print("bbbb"));
        assert_eq!((b.len(), b.bytes()), (2, 8));
        b.push(ts(2), print("cccc"));
        assert_eq!((b.len(), b.bytes(), b.evicted()), (2, 8, 1));
        assert_eq!(seqs(&b.query(&q(Some(0), 10))), [2, 3]);
    }

    #[test]
    fn oversize_event_is_kept_alone() {
        let mut b = EventBuffer::new(100, 10);
        b.push(ts(0), print("aa"));
        b.push(ts(1), print("bb"));
        b.push(ts(2), print(&"x".repeat(50)));
        assert_eq!((b.len(), b.bytes(), b.evicted()), (1, 50, 2));
        assert_eq!(seqs(&b.query(&q(Some(0), 10))), [3]);
        b.push(ts(3), print("y"));
        assert_eq!((b.len(), b.bytes(), b.evicted()), (1, 1, 3));
    }

    #[test]
    fn event_bytes_per_kind() {
        let at = ts(0);
        let ev = |body| Event {
            seq: 1,
            time: at,
            body,
        };
        assert_eq!(ev(print("abc")).bytes(), 3);
        let err = EventBody::Error(LuaError {
            guid: "-1".into(),
            error: "boom".into(),
            prefix: "pre".into(),
        });
        assert_eq!(ev(err).bytes(), 9);
        let c = EventBody::Custom {
            value: json!({"a": 1}),
        };
        assert_eq!(ev(c).bytes(), r#"{"a":1}"#.len());
        let saved = |p: Option<&str>| EventBody::Saved {
            save_path: p.map(str::to_string),
        };
        assert_eq!(ev(saved(Some("ab"))).bytes(), 2);
        assert_eq!(ev(saved(None)).bytes(), 0);
        let loaded = EventBody::Loaded {
            save_path: Some("abcd".into()),
            objects: 9,
        };
        assert_eq!(ev(loaded).bytes(), 4);
    }

    #[test]
    fn since_is_exclusive_and_beyond_newest_is_empty() {
        let mut b = big();
        for i in 0..3 {
            b.push(ts(i), custom(i as u64));
        }
        assert_eq!(seqs(&b.query(&q(Some(1), 10))), [2, 3]);
        assert_eq!(seqs(&b.query(&q(Some(3), 10))), Vec::<u64>::new());
        let page = b.query(&q(Some(10), 10));
        assert!(page.events.is_empty());
        assert_eq!((page.next_since, page.more, page.dropped), (10, false, 0));
    }

    #[test]
    fn paging_with_small_limit_has_no_gaps_or_duplicates() {
        let mut b = big();
        for i in 0..10 {
            b.push(ts(i), custom(i as u64));
        }
        let mut since = 0;
        let mut got = Vec::new();
        let mut pages = 0;
        loop {
            let page = b.query(&q(Some(since), 3));
            got.extend(seqs(&page));
            since = page.next_since;
            pages += 1;
            if !page.more {
                break;
            }
        }
        assert_eq!(got, (1..=10).collect::<Vec<u64>>());
        assert_eq!(pages, 4);
        assert_eq!(since, 10);
    }

    #[test]
    fn exact_limit_ends_without_more() {
        let mut b = big();
        for i in 0..3 {
            b.push(ts(i), custom(i as u64));
        }
        let page = b.query(&q(Some(0), 3));
        assert_eq!(
            (seqs(&page), page.more, page.next_since),
            (vec![1, 2, 3], false, 3)
        );
    }

    #[test]
    fn dropped_after_eviction() {
        let mut b = EventBuffer::new(3, MAX_BYTES);
        for i in 0..8 {
            b.push(ts(i), custom(i as u64));
        }
        // Buffered: 6, 7, 8. Evicted: 1..=5.
        assert_eq!(b.query(&q(None, 10)).dropped, 5);
        assert_eq!(b.query(&q(Some(0), 10)).dropped, 5);
        assert_eq!(b.query(&q(Some(2), 10)).dropped, 3);
        assert_eq!(b.query(&q(Some(5), 10)).dropped, 0);
        assert_eq!(b.query(&q(Some(7), 10)).dropped, 0);
        assert_eq!(b.query(&q(Some(100), 10)).dropped, 0);
    }

    #[test]
    fn empty_buffer() {
        let b = big();
        let page = b.query(&q(None, 10));
        assert_eq!(
            page,
            Page {
                events: vec![],
                next_since: 0,
                more: false,
                omitted: 0,
                dropped: 0
            }
        );
        let page = b.query(&q(Some(0), 10));
        assert_eq!((page.next_since, page.more, page.dropped), (0, false, 0));
        let page = b.query(&q(Some(4), 10));
        assert_eq!((page.next_since, page.dropped), (4, 0));
    }

    fn mixed() -> EventBuffer {
        let mut b = big();
        b.push(ts(0), print("p1")); // 1
        b.push(ts(1), custom(1)); // 2
        b.push(ts(2), custom(2)); // 3
        b.push(ts(3), print("p2")); // 4
        b.push(ts(4), custom(3)); // 5
        b.push(ts(5), custom(4)); // 6
        b
    }

    fn kq(since: Option<u64>, kinds: &[EventKind], limit: usize) -> Query {
        Query {
            since,
            kinds: Some(kinds.to_vec()),
            limit,
        }
    }

    #[test]
    fn kinds_filter_and_next_since_skips_filtered_events() {
        let b = mixed();
        let page = b.query(&kq(Some(0), &[EventKind::Print], 10));
        assert_eq!(seqs(&page), [1, 4]);
        assert_eq!((page.next_since, page.more), (6, false));
        let page = b.query(&kq(Some(4), &[EventKind::Print], 10));
        assert!(page.events.is_empty());
        assert_eq!(page.next_since, 6);
        let page = b.query(&kq(Some(0), &[], 10));
        assert!(page.events.is_empty());
        let page = b.query(&kq(Some(0), &[EventKind::Print, EventKind::Custom], 10));
        assert_eq!(page.events.len(), 6);
    }

    #[test]
    fn paging_with_filter() {
        let b = mixed();
        let k = [EventKind::Custom];
        let page = b.query(&kq(Some(0), &k, 3));
        assert_eq!(seqs(&page), [2, 3, 5]);
        assert!(page.more);
        assert_eq!(page.next_since, 5);
        let page = b.query(&kq(Some(page.next_since), &k, 3));
        assert_eq!(seqs(&page), [6]);
        assert!(!page.more);
        assert_eq!(page.next_since, 6);
    }

    #[test]
    fn without_since_returns_newest_with_omitted() {
        let b = mixed();
        let page = b.query(&q(None, 4));
        assert_eq!(seqs(&page), [3, 4, 5, 6]);
        assert_eq!((page.omitted, page.more, page.next_since), (2, false, 6));
        let page = b.query(&kq(None, &[EventKind::Custom], 3));
        assert_eq!(seqs(&page), [3, 5, 6]);
        assert_eq!(page.omitted, 1);
        let page = b.query(&q(None, 100));
        assert_eq!((page.events.len(), page.omitted), (6, 0));
    }

    #[test]
    fn limit_zero() {
        let b = mixed();
        let page = b.query(&q(Some(2), 0));
        assert!(page.events.is_empty());
        assert_eq!((page.more, page.next_since), (true, 2));
        let page = b.query(&q(Some(6), 0));
        assert_eq!((page.more, page.next_since), (false, 6));
        let page = b.query(&q(None, 0));
        assert!(page.events.is_empty());
        assert_eq!(page.omitted, 6);
    }

    fn script_state(guid: &str) -> ScriptState {
        ScriptState {
            guid: guid.into(),
            ..ScriptState::default()
        }
    }

    #[test]
    fn from_incoming_buffered_kinds() {
        assert_eq!(
            EventBody::from_incoming(Incoming::Print {
                message: "hi".into()
            }),
            Some(print("hi"))
        );
        assert_eq!(
            EventBody::from_incoming(Incoming::Error {
                error: "e".into(),
                guid: "g".into(),
                error_message_prefix: "p".into(),
            }),
            Some(EventBody::Error(LuaError {
                guid: "g".into(),
                error: "e".into(),
                prefix: "p".into(),
            }))
        );
        assert_eq!(
            EventBody::from_incoming(Incoming::Custom {
                custom_message: json!({"k": 1})
            }),
            Some(EventBody::Custom {
                value: json!({"k": 1})
            })
        );
        assert_eq!(
            EventBody::from_incoming(Incoming::GameSaved {
                save_path: Some("a.json".into())
            }),
            Some(EventBody::Saved {
                save_path: Some("a.json".into())
            })
        );
        assert_eq!(
            EventBody::from_incoming(Incoming::LoadGame {
                script_states: vec![script_state("1"), script_state("2")],
                save_path: Some("b.json".into()),
            }),
            Some(EventBody::Loaded {
                save_path: Some("b.json".into()),
                objects: 2
            })
        );
    }

    #[test]
    fn from_incoming_empty_save_path_is_none() {
        assert_eq!(
            EventBody::from_incoming(Incoming::GameSaved {
                save_path: Some(String::new())
            }),
            Some(EventBody::Saved { save_path: None })
        );
        assert_eq!(
            EventBody::from_incoming(Incoming::LoadGame {
                script_states: vec![],
                save_path: Some(String::new()),
            }),
            Some(EventBody::Loaded {
                save_path: None,
                objects: 0
            })
        );
    }

    #[test]
    fn from_incoming_skips_unbuffered_kinds() {
        assert_eq!(
            EventBody::from_incoming(Incoming::PushObject {
                script_states: vec![]
            }),
            None
        );
        assert_eq!(
            EventBody::from_incoming(Incoming::Return {
                return_value: json!(1)
            }),
            None
        );
        assert_eq!(
            EventBody::from_incoming(Incoming::ObjectCreated { guid: "g".into() }),
            None
        );
        assert_eq!(
            EventBody::from_incoming(Incoming::Unknown {
                message_id: 99,
                raw: serde_json::Map::new()
            }),
            None
        );
    }

    #[test]
    fn kinds_of_bodies() {
        assert_eq!(print("a").kind(), EventKind::Print);
        assert_eq!(custom(1).kind(), EventKind::Custom);
        assert_eq!(
            EventBody::Saved { save_path: None }.kind(),
            EventKind::Saved
        );
        assert_eq!(
            EventBody::Loaded {
                save_path: None,
                objects: 0
            }
            .kind(),
            EventKind::Loaded
        );
    }

    fn json_of(seq: u64, body: EventBody) -> Value {
        Event {
            seq,
            time: ts(0),
            body,
        }
        .to_json()
    }

    #[test]
    fn to_json_shapes() {
        let time = ts(0).to_string();
        assert!(time.ends_with('Z'));

        assert_eq!(
            json_of(1, print("hi")),
            json!({"seq": 1, "time": time, "type": "print", "message": "hi"})
        );

        let err = LuaError {
            guid: "g".into(),
            error: "boom".into(),
            prefix: "pre".into(),
        };
        let mut expected = event_json(&ts(0), &ConsoleEvent::Error(err.clone()));
        expected["seq"] = json!(2);
        let got = json_of(2, EventBody::Error(err));
        assert_eq!(got, expected);
        assert_eq!(got["type"], "error");
        assert_eq!(got["guid"], "g");
        assert_eq!(got["error"], "boom");
        assert!(got["message"].is_string());

        assert_eq!(
            json_of(3, EventBody::Custom { value: json!([1]) }),
            json!({"seq": 3, "time": time, "type": "custom", "customMessage": [1]})
        );
        assert_eq!(
            json_of(
                4,
                EventBody::Saved {
                    save_path: Some("s.json".into())
                }
            ),
            json!({"seq": 4, "time": time, "type": "saved", "savePath": "s.json"})
        );
        assert_eq!(
            json_of(5, EventBody::Saved { save_path: None }),
            json!({"seq": 5, "time": time, "type": "saved", "savePath": null})
        );
        assert_eq!(
            json_of(
                6,
                EventBody::Loaded {
                    save_path: None,
                    objects: 3
                }
            ),
            json!({
                "seq": 6, "time": time, "type": "loaded",
                "savePath": null, "objectsWithScripts": 3
            })
        );
    }
}
