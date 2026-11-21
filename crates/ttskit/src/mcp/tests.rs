//! The server is driven over an in-memory pipe by a scripted client that
//! writes raw newline-delimited JSON-RPC lines, against a fake game.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use ttskit_editor::fake::{FakeGame, Responder};
use ttskit_editor::{Listener, ScriptState};

use super::*;

struct Client {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    next_id: u64,
    /// Responses read while waiting for another id.
    stash: HashMap<u64, Value>,
}

impl Client {
    async fn write(&mut self, doc: &Value) {
        let mut line = doc.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    /// Send a request and return its id without waiting for the response.
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    async fn response(&mut self, id: u64) -> Value {
        if let Some(r) = self.stash.remove(&id) {
            return r;
        }
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(Duration::from_secs(15), self.reader.read_line(&mut line))
                .await
                .expect("no response in time")
                .unwrap();
            assert!(n > 0, "the server closed the stream");
            let doc: Value = serde_json::from_str(&line).expect("a JSON-RPC line");
            assert_eq!(doc["jsonrpc"], "2.0");
            match doc["id"].as_u64() {
                Some(got) if got == id => return doc,
                Some(got) => {
                    self.stash.insert(got, doc);
                }
                None => {} // a notification from the server
            }
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.response(id).await
    }

    async fn notify(&mut self, method: &str) {
        self.write(&json!({"jsonrpc": "2.0", "method": method}))
            .await;
    }

    async fn initialize(&mut self) -> Value {
        let r = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "0"}
                }),
            )
            .await;
        self.notify("notifications/initialized").await;
        r
    }

    async fn send_call(&mut self, tool: &str, arguments: Value) -> u64 {
        self.send("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
    }

    /// The `result` of a tool call; panics on a protocol error.
    async fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let id = self.send_call(tool, arguments).await;
        let r = self.response(id).await;
        assert!(r.get("error").is_none(), "protocol error: {r}");
        r["result"].clone()
    }
}

fn is_error(result: &Value) -> bool {
    result["isError"] == true
}

/// The pretty JSON document in the first text content.
fn body(result: &Value) -> Value {
    let text = result["content"][0]["text"].as_str().expect("text content");
    serde_json::from_str(text).expect("JSON text")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn state(guid: &str, script: Option<&str>, ui: Option<&str>) -> ScriptState {
    ScriptState {
        name: format!("obj {guid}"),
        guid: guid.into(),
        script: script.map(Into::into),
        ui: ui.map(Into::into),
    }
}

struct Harness {
    client: Client,
    game: Arc<FakeGame>,
    ep: Endpoints,
    _root: tempfile::TempDir,
}

fn connect(ep: Endpoints, root: &std::path::Path) -> Client {
    let (server_side, client_side) = tokio::io::duplex(1 << 20);
    let server = EditorServer::new(ep, resolve_root(Some(root)).unwrap());
    tokio::spawn(serve_on(server, server_side));
    let (reader, writer) = tokio::io::split(client_side);
    Client {
        reader: BufReader::new(reader),
        writer,
        next_id: 0,
        stash: HashMap::new(),
    }
}

async fn harness(
    scripts: Vec<ScriptState>,
    save_path: &str,
    responder: Option<Responder>,
) -> Harness {
    let editor = ([127, 0, 0, 1], free_port()).into();
    let game = match responder {
        Some(r) => FakeGame::start_with(editor, scripts, r).await.unwrap(),
        None => FakeGame::start_with_save_path(editor, scripts, save_path)
            .await
            .unwrap(),
    };
    let ep = Endpoints {
        game: game.addr(),
        editor,
        timeout: Duration::from_secs(5),
    };
    let root = tempfile::tempdir().unwrap();
    let mut client = connect(ep, root.path());
    client.initialize().await;
    Harness {
        client,
        game: Arc::new(game),
        ep,
        _root: root,
    }
}

async fn plain() -> Harness {
    harness(vec![], "", None).await
}

#[tokio::test]
async fn initialize_reports_name_and_tools_capability() {
    let root = tempfile::tempdir().unwrap();
    let ep = Endpoints::new(1, free_port());
    let mut client = connect(ep, root.path());
    let r = client.initialize().await;
    let result = &r["result"];
    assert_eq!(result["serverInfo"]["name"], "ttskit");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(result["capabilities"]["tools"].is_object(), "{result}");
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("not affiliated")
    );
}

#[tokio::test]
async fn tools_list_has_the_five_editor_tools() {
    let mut h = plain().await;
    let r = h.client.request("tools/list", json!({})).await;
    let tools = r["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "execute_lua",
            "get_console",
            "get_scripts",
            "get_status",
            "send_custom_message"
        ]
    );
    for t in tools {
        assert!(
            !t["description"].as_str().unwrap_or_default().is_empty(),
            "{t}"
        );
        assert!(t["inputSchema"].is_object(), "{t}");
    }
    let schema = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()
            .get("inputSchema")
            .unwrap()
            .clone()
    };
    assert!(
        schema("execute_lua")["required"]
            .as_array()
            .unwrap()
            .contains(&json!("code"))
    );
    assert_eq!(
        schema("send_custom_message")["properties"]["message"]["type"],
        "object"
    );
}

#[tokio::test]
async fn get_status_reports_the_game_save_ports_and_root() {
    let mut h = harness(
        vec![state("a", Some("s"), None), state("-1", Some("g"), None)],
        "C:/saves/TS_Save_1.json",
        None,
    )
    .await;
    let r = h.client.call("get_status", json!({})).await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["gameReachable"], true);
    assert_eq!(v["savePath"], "C:/saves/TS_Save_1.json");
    assert_eq!(v["objectsWithScripts"], 2);
    assert_eq!(v["gamePort"], h.ep.game.port());
    assert_eq!(v["editorPort"], h.ep.editor.port());
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(!v["root"].as_str().unwrap().is_empty());
    assert!(v.get("error").is_none());
}

#[tokio::test]
async fn get_status_with_an_empty_save_path_reports_null() {
    let mut h = plain().await;
    let v = body(&h.client.call("get_status", json!({})).await);
    assert_eq!(v["gameReachable"], true);
    assert!(v["savePath"].is_null());
}

#[tokio::test]
async fn get_status_for_an_unreachable_game_is_not_an_error_result() {
    // A port that was just released refuses connections.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let game = closed.local_addr().unwrap();
    drop(closed);
    let ep = Endpoints {
        game,
        editor: ([127, 0, 0, 1], free_port()).into(),
        timeout: Duration::from_secs(3),
    };
    let root = tempfile::tempdir().unwrap();
    let mut client = connect(ep, root.path());
    client.initialize().await;
    let r = client.call("get_status", json!({})).await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["gameReachable"], false);
    assert!(v["savePath"].is_null());
    assert!(v["objectsWithScripts"].is_null());
    assert!(!v["error"].as_str().unwrap().is_empty());
}

fn two_objects() -> Vec<ScriptState> {
    vec![
        state("a1", Some("print(1)"), Some("<ui/>")),
        state("b2", Some("ä"), None),
    ]
}

#[tokio::test]
async fn get_scripts_without_guids_is_a_summary() {
    let mut h = harness(two_objects(), "x.json", None).await;
    let r = h.client.call("get_scripts", json!({})).await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["savePath"], "x.json");
    assert_eq!(
        v["objects"],
        json!([
            {"guid": "a1", "name": "obj a1", "scriptBytes": 8, "uiBytes": 5},
            // UTF-8 length, not characters; a missing UI is 0 bytes.
            {"guid": "b2", "name": "obj b2", "scriptBytes": 2, "uiBytes": 0},
        ])
    );
    assert!(v.get("missing").is_none());
}

#[tokio::test]
async fn get_scripts_with_guids_returns_full_text_and_the_missing_ones() {
    let mut h = harness(two_objects(), "", None).await;
    let r = h
        .client
        .call("get_scripts", json!({"guids": ["b2", "zz", "a1"]}))
        .await;
    let v = body(&r);
    // In the order the game lists them.
    assert_eq!(
        v["objects"],
        json!([
            {"guid": "a1", "name": "obj a1", "script": "print(1)", "ui": "<ui/>"},
            {"guid": "b2", "name": "obj b2", "script": "ä", "ui": ""},
        ])
    );
    assert_eq!(v["missing"], json!(["zz"]));
    assert!(v["savePath"].is_null());
}

#[tokio::test]
async fn get_scripts_for_an_unreachable_game_is_an_error_result() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let game = closed.local_addr().unwrap();
    drop(closed);
    let ep = Endpoints {
        game,
        editor: ([127, 0, 0, 1], free_port()).into(),
        timeout: Duration::from_secs(3),
    };
    let root = tempfile::tempdir().unwrap();
    let mut client = connect(ep, root.path());
    client.initialize().await;
    let r = client.call("get_scripts", json!({})).await;
    assert!(is_error(&r), "{r}");
    assert!(body(&r)["error"].is_string());
}

/// A responder that answers an execute (messageID 3) with `replies`.
fn on_execute(replies: Vec<Value>) -> Responder {
    Box::new(move |m| {
        if m["messageID"] == 3 {
            replies.clone()
        } else {
            vec![]
        }
    })
}

#[tokio::test]
async fn execute_lua_returns_the_prints() {
    let mut h = harness(
        vec![],
        "",
        Some(on_execute(vec![json!({"messageID": 2, "message": "hi"})])),
    )
    .await;
    let r = h
        .client
        .call(
            "execute_lua",
            json!({"code": "print('hi')", "quiet_ms": 150}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["ok"], true);
    assert_eq!(v["guid"], "-1");
    assert_eq!(v["prints"], json!(["hi"]));
    let sent: Vec<Value> = h
        .game
        .received()
        .into_iter()
        .filter(|m| m["messageID"] == 3)
        .collect();
    assert_eq!(sent[0]["script"], "print('hi')");
    assert_eq!(sent[0]["guid"], "-1");
}

#[tokio::test]
async fn execute_lua_uses_the_given_guid() {
    let mut h = plain().await;
    let r = h
        .client
        .call(
            "execute_lua",
            json!({"code": "x", "guid": "a53de1", "quiet_ms": 100}),
        )
        .await;
    assert_eq!(body(&r)["guid"], "a53de1");
    assert!(
        h.game
            .received()
            .iter()
            .any(|m| m["messageID"] == 3 && m["guid"] == "a53de1")
    );
}

#[tokio::test]
async fn execute_lua_error_is_an_error_result_with_the_hint() {
    let mut h = harness(
        vec![],
        "",
        Some(on_execute(vec![json!({
            "messageID": 3, "guid": "-1", "error": "zzz",
            "errorMessagePrefix": "Error finding guid for execute script: "
        })])),
    )
    .await;
    let r = h
        .client
        .call("execute_lua", json!({"code": "x", "guid": "zzz"}))
        .await;
    assert!(is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["ok"], false);
    let message = v["errors"][0]["message"].as_str().unwrap();
    assert!(message.contains("already have a script"), "{message}");
}

#[tokio::test]
async fn execute_lua_without_a_reply_is_ok() {
    let mut h = plain().await;
    let r = h
        .client
        .call("execute_lua", json!({"code": "x", "quiet_ms": 100}))
        .await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["ok"], true);
    assert_eq!(v["prints"], json!([]));
}

#[tokio::test]
async fn execute_lua_requires_code() {
    let mut h = plain().await;
    let id = h.client.send_call("execute_lua", json!({})).await;
    let r = h.client.response(id).await;
    // rmcp reports invalid arguments as an error result.
    assert!(r.get("error").is_some() || is_error(&r["result"]), "{r}");
}

#[tokio::test]
async fn send_custom_message_delivers_the_object() {
    let mut h = plain().await;
    let r = h
        .client
        .call(
            "send_custom_message",
            json!({"message": {"action": "ping", "n": 1}}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(body(&r), json!({"sent": true}));
    for _ in 0..40 {
        if !h.game.received().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        h.game.received()[0]["customMessage"],
        json!({"action": "ping", "n": 1})
    );
}

#[tokio::test]
async fn send_custom_message_rejects_non_objects_and_sends_nothing() {
    let mut h = plain().await;
    for message in [json!([1]), json!("x")] {
        let id = h
            .client
            .send_call("send_custom_message", json!({ "message": message }))
            .await;
        let r = h.client.response(id).await;
        assert!(
            r.get("error").is_some() || is_error(&r["result"]),
            "accepted {message}: {r}"
        );
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(h.game.received().is_empty());
}

#[tokio::test]
async fn get_console_collects_what_arrives_during_the_call() {
    let mut h = plain().await;
    let game = h.game.clone();
    let editor = h.ep.editor;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        game.push_raw(editor, &json!({"messageID": 2, "message": "one"}))
            .await
            .unwrap();
        game.push_raw(
            editor,
            &json!({"messageID": 3, "guid": "a1", "error": "boom",
                    "errorMessagePrefix": "[Obj] Lua Error: boom"}),
        )
        .await
        .unwrap();
        // Not console output.
        game.push_raw(editor, &json!({"messageID": 4, "customMessage": {}}))
            .await
            .unwrap();
    });
    let r = h
        .client
        .call("get_console", json!({"quiet_ms": 400, "max_ms": 5000}))
        .await;
    assert!(!is_error(&r), "{r}");
    let events = body(&r)["events"].as_array().unwrap().clone();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["type"], "print");
    assert_eq!(events[0]["message"], "one");
    assert!(events[0]["time"].as_str().unwrap().ends_with('Z'));
    assert_eq!(events[1]["type"], "error");
    assert_eq!(events[1]["guid"], "a1");
    assert_eq!(events[1]["message"], "[Obj] Lua Error: boom");
}

#[tokio::test]
async fn get_console_returns_no_events_when_the_game_is_silent() {
    let mut h = plain().await;
    let r = h.client.call("get_console", json!({"quiet_ms": 150})).await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(body(&r), json!({"events": []}));
}

#[tokio::test]
async fn get_console_stops_at_max_ms_even_while_messages_keep_coming() {
    let mut h = plain().await;
    let game = h.game.clone();
    let editor = h.ep.editor;
    let pusher = tokio::spawn(async move {
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = game
                .push_raw(editor, &json!({"messageID": 2, "message": "tick"}))
                .await;
        }
    });
    let started = std::time::Instant::now();
    let r = h
        .client
        .call("get_console", json!({"quiet_ms": 1000, "max_ms": 500}))
        .await;
    assert!(started.elapsed() < Duration::from_millis(1800));
    assert!(!is_error(&r), "{r}");
    assert!(!body(&r)["events"].as_array().unwrap().is_empty());
    pusher.abort();
}

#[tokio::test]
async fn a_busy_editor_port_is_an_error_result() {
    let mut h = plain().await;
    let _held = Listener::bind(h.ep.editor).await.unwrap();
    for (tool, args) in [
        ("get_console", json!({"quiet_ms": 100})),
        ("execute_lua", json!({"code": "x"})),
        ("get_scripts", json!({})),
    ] {
        let r = h.client.call(tool, args).await;
        assert!(is_error(&r), "{tool}: {r}");
        let v = body(&r);
        assert!(
            v["error"].as_str().unwrap().contains("already in use"),
            "{tool}: {v}"
        );
        assert_eq!(v["gameUnreachable"], false);
    }
    // get_status still succeeds and says why the round trip failed.
    let v = body(&h.client.call("get_status", json!({})).await);
    assert_eq!(v["gameReachable"], false);
    assert!(v["error"].as_str().unwrap().contains("already in use"));
}

#[tokio::test]
async fn the_port_is_released_after_each_call() {
    let mut h = plain().await;
    h.client.call("get_console", json!({"quiet_ms": 100})).await;
    // Binding would fail if the server still held the port.
    drop(Listener::bind(h.ep.editor).await.unwrap());
}

#[tokio::test]
async fn concurrent_calls_are_serialized_and_both_succeed() {
    let mut h = harness(
        vec![],
        "",
        Some(on_execute(vec![json!({"messageID": 2, "message": "p"})])),
    )
    .await;
    let args = json!({"code": "print('p')", "quiet_ms": 150});
    let a = h.client.send_call("execute_lua", args.clone()).await;
    let b = h.client.send_call("execute_lua", args).await;
    for id in [a, b] {
        let r = h.client.response(id).await;
        let result = &r["result"];
        assert!(!is_error(result), "{r}");
        assert_eq!(body(result)["prints"], json!(["p"]));
    }
}

#[test]
fn root_must_be_an_existing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = resolve_root(Some(dir.path())).unwrap();
    assert!(root.is_absolute());
    assert!(!root.to_string_lossy().starts_with(r"\\?\"));
    assert!(resolve_root(Some(&dir.path().join("missing"))).is_err());
    let file = dir.path().join("f.txt");
    std::fs::write(&file, "x").unwrap();
    assert!(
        resolve_root(Some(&file))
            .unwrap_err()
            .message
            .contains("not a directory")
    );
    assert!(resolve_root(None).unwrap().is_absolute());
}
