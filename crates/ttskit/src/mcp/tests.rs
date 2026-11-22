//! The server is driven over an in-memory pipe by a scripted client that
//! writes raw newline-delimited JSON-RPC lines, against a fake game.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    root: tempfile::TempDir,
}

async fn connect(ep: Endpoints, root: &std::path::Path) -> Client {
    connect_with(ep, root, buffer::DEFAULT_MAX_EVENTS).await
}

async fn connect_with(ep: Endpoints, root: &std::path::Path, max_events: usize) -> Client {
    attach(EditorServer::start(ep, resolve_root(Some(root)).unwrap(), max_events).await).await
}

async fn attach(server: EditorServer) -> Client {
    let (server_side, client_side) = tokio::io::duplex(1 << 20);
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
    let mut client = connect(ep, root.path()).await;
    client.initialize().await;
    Harness {
        client,
        game: Arc::new(game),
        ep,
        root,
    }
}

async fn plain() -> Harness {
    harness(vec![], "", None).await
}

#[tokio::test]
async fn initialize_reports_name_and_tools_capability() {
    let root = tempfile::tempdir().unwrap();
    let ep = Endpoints::new(1, free_port());
    let mut client = connect(ep, root.path()).await;
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
async fn tools_list_has_the_editor_and_save_tools() {
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
            "manifest_show",
            "save_and_play",
            "save_check_urls",
            "save_diff",
            "save_lint",
            "save_scan",
            "send_custom_message"
        ]
    );
    for t in tools {
        assert!(
            !t["description"].as_str().unwrap_or_default().is_empty(),
            "{t}"
        );
        assert!(t["inputSchema"].is_object(), "{t}");
        // Descriptions must be single lines.
        let props = t["inputSchema"]["properties"].as_object().unwrap();
        for (name, prop) in props {
            if let Some(d) = prop["description"].as_str() {
                assert!(!d.contains('\n'), "{}.{name}: {d:?}", t["name"]);
            }
        }
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
    assert!(
        schema("save_and_play")["required"]
            .as_array()
            .unwrap()
            .contains(&json!("dir"))
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
    let mut client = connect(ep, root.path()).await;
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
    let mut client = connect(ep, root.path()).await;
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

// ---- resolve_in_root and fingerprint ----

#[test]
fn resolve_in_root_accepts_paths_inside_and_refuses_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let root = resolve_root(Some(dir.path())).unwrap();
    std::fs::create_dir_all(root.join("a").join("b")).unwrap();
    let outside = tempfile::tempdir().unwrap();

    // relative inside
    let got = resolve_in_root(&root, Path::new("a/b")).unwrap();
    assert_eq!(got, root.join("a").join("b"));
    // absolute inside
    assert_eq!(
        resolve_in_root(&root, &root.join("a")).unwrap(),
        root.join("a")
    );
    // `..` is refused even when the result is inside
    let e = resolve_in_root(&root, Path::new("a/../a/b")).unwrap_err();
    assert!(e.contains(".."), "{e}");
    assert!(resolve_in_root(&root, &root.join("a").join("..").join("a")).is_err());
    // absolute outside
    let e = resolve_in_root(&root, outside.path()).unwrap_err();
    assert!(e.contains("outside the project root"), "{e}");
    // missing
    let e = resolve_in_root(&root, Path::new("nope")).unwrap_err();
    assert!(e.contains("nope"), "{e}");
}

fn plan_of(script: FieldChange) -> ScriptPlan {
    ScriptPlan {
        changes: vec![ttskit_editor::ObjectChange {
            guid: "a".into(),
            name: "n".into(),
            script,
            ui: FieldChange::Unchanged,
            listed: true,
            state: ScriptState::default(),
        }],
        untouched: 0,
    }
}

#[test]
fn fingerprint_depends_on_the_changes() {
    let modify = |old: &str, new: &str| FieldChange::Modify {
        old: old.into(),
        new: new.into(),
    };
    let base = fingerprint(&plan_of(modify("old", "new")));
    assert_eq!(base.len(), 16);
    assert!(
        base.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(base, fingerprint(&plan_of(modify("old", "new"))));
    assert_ne!(base, fingerprint(&plan_of(modify("old", "newer"))));
    assert_ne!(base, fingerprint(&plan_of(modify("older", "new"))));
    assert_ne!(
        base,
        fingerprint(&plan_of(FieldChange::Add { new: "new".into() }))
    );
}

// ---- save_and_play ----

const SAVE: &str = r#"{
  "SaveName": "t",
  "LuaScript": "g",
  "ObjectStates": [
    {"GUID": "a1", "Name": "Block", "Nickname": "", "LuaScript": "new a1"},
    {"GUID": "b2", "Name": "Card", "Nickname": "", "LuaScript": "same"}
  ]
}"#;

/// Unpack `save` into `root/name` and return the tree.
fn make_tree(root: &Path, name: &str, save: &str) -> PathBuf {
    let tree = root.join(name);
    let unpacked = ttskit_core::unpack(save.as_bytes(), ttskit_core::Eol::Lf).unwrap();
    let plan = unpacked
        .plan(&tree, ttskit_core::WriteOptions::default())
        .unwrap();
    unpacked.apply(&tree, &plan).unwrap();
    tree
}

/// The game as it is before the push: only `a1` differs from [`SAVE`].
fn live_state() -> Vec<ScriptState> {
    vec![
        state("-1", Some("g"), None),
        state("a1", Some("old a1"), None),
        state("b2", Some("same"), None),
    ]
}

/// Answers a Save & Play (messageID 1) as the real game does: "saved" (6),
/// "loaded" (1) once the reload is done, then the output of `onLoad`.
fn reload_output() -> Responder {
    Box::new(|m| {
        if m["messageID"] == 1 {
            vec![
                json!({"messageID": 6, "savePath": "s.json"}),
                json!({"messageID": 1, "scriptStates": m["scriptStates"], "savePath": "s.json"}),
                json!({"messageID": 2, "message": "reloaded"}),
                json!({"messageID": 3, "guid": "a1", "error": "boom",
                       "errorMessagePrefix": "[Block] Lua Error: boom"}),
            ]
        } else {
            vec![]
        }
    })
}

async fn sp_harness(live: Vec<ScriptState>) -> Harness {
    let h = harness(live, "", Some(reload_output())).await;
    make_tree(h.root.path(), "tree", SAVE);
    h
}

fn save_and_play_messages(h: &Harness) -> Vec<Value> {
    h.game
        .received()
        .into_iter()
        .filter(|m| m["messageID"] == 1)
        .collect()
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn save_and_play_without_confirm_shows_the_plan_and_sends_nothing() {
    let mut h = sp_harness(live_state()).await;
    let r = h
        .client
        .call("save_and_play", json!({"dir": "tree", "quiet_ms": 100}))
        .await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["sent"], false);
    assert_eq!(v["fingerprint"].as_str().unwrap().len(), 16);
    assert_eq!(v["deletes"], false);
    assert_eq!(v["untouched"], 0);
    assert_eq!(v["changes"].as_array().unwrap().len(), 1);
    assert_eq!(v["changes"][0]["guid"], "a1");
    let diff = v["diff"].as_str().unwrap();
    assert!(
        diff.contains("-old a1") && diff.contains("+new a1"),
        "{diff}"
    );
    let next = v["next"].as_str().unwrap();
    assert!(next.contains("confirm") && next.contains(v["fingerprint"].as_str().unwrap()));
    assert!(!next.contains("allow_delete"));
    settle().await;
    assert!(save_and_play_messages(&h).is_empty());
}

#[tokio::test]
async fn save_and_play_with_the_fingerprint_sends_only_changed_objects() {
    let mut h = sp_harness(live_state()).await;
    let plan = body(&h.client.call("save_and_play", json!({"dir": "tree"})).await);
    let r = h
        .client
        .call(
            "save_and_play",
            json!({"dir": "tree", "confirm": plan["fingerprint"], "quiet_ms": 200}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["sent"], true);
    assert_eq!(v["reloaded"], true);
    assert!(v.get("note").is_none());
    assert!(v.get("next").is_none());
    let console = v["console"].as_array().unwrap();
    assert_eq!(console.len(), 2, "{console:?}");
    assert_eq!(console[0]["type"], "print");
    assert_eq!(console[0]["message"], "reloaded");
    assert_eq!(console[1]["type"], "error");
    assert_eq!(console[1]["guid"], "a1");
    let sent = save_and_play_messages(&h);
    assert_eq!(sent.len(), 1);
    let states = sent[0]["scriptStates"].as_array().unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0]["guid"], "a1");
    assert_eq!(states[0]["script"], "new a1");
    assert_eq!(states[0]["ui"], "");
}

#[tokio::test]
async fn save_and_play_refuses_a_wrong_or_stale_confirm() {
    let mut a = sp_harness(live_state()).await;
    let old = body(&a.client.call("save_and_play", json!({"dir": "tree"})).await);

    // Another game state: the live script of a1 differs now.
    let mut live = live_state();
    live[1] = state("a1", Some("changed meanwhile"), None);
    let mut b = sp_harness(live).await;
    let r = b
        .client
        .call(
            "save_and_play",
            json!({"dir": "tree", "confirm": old["fingerprint"]}),
        )
        .await;
    assert!(is_error(&r), "{r}");
    let v = body(&r);
    assert!(v["error"].as_str().unwrap().contains("plan changed"));
    assert_eq!(v["gameUnreachable"], false);
    assert_eq!(v["sent"], false);
    assert_ne!(v["fingerprint"], old["fingerprint"]);
    assert!(v["diff"].as_str().unwrap().contains("-changed meanwhile"));

    let r = b
        .client
        .call(
            "save_and_play",
            json!({"dir": "tree", "confirm": "nonsense"}),
        )
        .await;
    assert!(is_error(&r), "{r}");
    settle().await;
    assert!(save_and_play_messages(&b).is_empty());
    assert!(save_and_play_messages(&a).is_empty());
}

/// A game whose a1 has a UI that the tree does not have.
async fn deleting_harness() -> Harness {
    let h = harness(
        vec![state("a1", Some("new a1"), Some("<ui/>"))],
        "",
        Some(reload_output()),
    )
    .await;
    let save = r#"{"SaveName": "t", "ObjectStates": [
        {"GUID": "a1", "Name": "Block", "Nickname": "", "LuaScript": "new a1"}]}"#;
    make_tree(h.root.path(), "tree", save);
    h
}

#[tokio::test]
async fn save_and_play_needs_allow_delete_for_deletions() {
    let mut h = deleting_harness().await;
    let plan = body(&h.client.call("save_and_play", json!({"dir": "tree"})).await);
    assert_eq!(plan["deletes"], true);
    assert!(
        plan["next"]
            .as_str()
            .unwrap()
            .contains("allow_delete: true")
    );
    let fp = plan["fingerprint"].clone();

    let r = h
        .client
        .call("save_and_play", json!({"dir": "tree", "confirm": fp}))
        .await;
    assert!(is_error(&r), "{r}");
    let v = body(&r);
    assert!(v["error"].as_str().unwrap().contains("allow_delete"));
    assert_eq!(v["sent"], false);
    settle().await;
    assert!(save_and_play_messages(&h).is_empty());

    let r = h
        .client
        .call(
            "save_and_play",
            json!({"dir": "tree", "confirm": fp, "allow_delete": true, "quiet_ms": 100}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(body(&r)["sent"], true);
    let sent = save_and_play_messages(&h);
    assert_eq!(sent[0]["scriptStates"][0]["ui"], "");
    assert_eq!(sent[0]["scriptStates"][0]["script"], "new a1");
}

#[tokio::test]
async fn save_and_play_with_nothing_to_do_sends_nothing() {
    let mut live = live_state();
    live[1] = state("a1", Some("new a1"), None);
    let mut h = sp_harness(live).await;
    for args in [
        json!({"dir": "tree"}),
        json!({"dir": "tree", "confirm": "0123456789abcdef"}),
    ] {
        let r = h.client.call("save_and_play", args).await;
        assert!(!is_error(&r), "{r}");
        let v = body(&r);
        assert_eq!(v["sent"], false);
        assert_eq!(v["changes"], json!([]));
        assert!(v["message"].as_str().unwrap().contains("not reloaded"));
    }
    settle().await;
    assert!(save_and_play_messages(&h).is_empty());
}

#[tokio::test]
async fn save_and_play_refuses_paths_outside_the_root_without_touching_the_game() {
    let mut h = sp_harness(live_state()).await;
    let outside = tempfile::tempdir().unwrap();
    let outside_tree = make_tree(outside.path(), "tree", SAVE);
    let cases = [
        json!({"dir": "../tree"}),
        json!({"dir": "tree/../tree"}),
        json!({"dir": outside_tree.to_str().unwrap()}),
        json!({"dir": "tree", "include": [outside.path().to_str().unwrap()]}),
        json!({"dir": "missing"}),
    ];
    for args in cases {
        let r = h.client.call("save_and_play", args.clone()).await;
        assert!(is_error(&r), "{args}: {r}");
        let v = body(&r);
        assert_eq!(v["gameUnreachable"], false);
        assert!(!v["error"].as_str().unwrap().is_empty());
    }
    let r = h
        .client
        .call(
            "save_and_play",
            json!({"dir": outside_tree.to_str().unwrap()}),
        )
        .await;
    let message = body(&r)["error"].as_str().unwrap().to_owned();
    assert!(message.contains("outside the project root"), "{message}");
    settle().await;
    // Not even a get-scripts request.
    assert!(h.game.received().is_empty());
}

fn script_file(tree: &Path, guid: &str) -> PathBuf {
    let objects = tree.join("objects");
    let suffix = format!(".{guid}");
    for entry in std::fs::read_dir(&objects).unwrap() {
        let path = entry.unwrap().path();
        if path.to_string_lossy().ends_with(&suffix) {
            return path.join("script.lua");
        }
    }
    panic!("no object {guid} in {}", objects.display());
}

#[tokio::test]
async fn save_and_play_bundles_includes_from_inside_the_root() {
    let mut h = sp_harness(live_state()).await;
    let tree = h.root.path().join("tree");
    std::fs::write(script_file(&tree, "a1"), "local lib = require(\"Lib\")\n").unwrap();
    let lib = h.root.path().join("lib");
    std::fs::create_dir(&lib).unwrap();
    std::fs::write(lib.join("Lib.lua"), "return { marker = \"LIB_MARKER\" }\n").unwrap();
    let r = h
        .client
        .call("save_and_play", json!({"dir": "tree", "include": ["lib"]}))
        .await;
    assert!(!is_error(&r), "{r}");
    let diff = body(&r)["diff"].as_str().unwrap().to_owned();
    assert!(diff.contains("LIB_MARKER"), "{diff}");

    // A missing module is a pack error, reported as an error result.
    std::fs::write(script_file(&tree, "a1"), "require(\"Nope\")\n").unwrap();
    let r = h
        .client
        .call("save_and_play", json!({"dir": "tree", "include": ["lib"]}))
        .await;
    assert!(is_error(&r), "{r}");
    assert!(body(&r)["error"].as_str().unwrap().contains("Nope"));
    settle().await;
    assert!(save_and_play_messages(&h).is_empty());
}

#[tokio::test]
async fn save_and_play_with_an_unreachable_game_is_an_error_result() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let game = closed.local_addr().unwrap();
    drop(closed);
    let ep = Endpoints {
        game,
        editor: ([127, 0, 0, 1], free_port()).into(),
        timeout: Duration::from_secs(3),
    };
    let root = tempfile::tempdir().unwrap();
    make_tree(root.path(), "tree", SAVE);
    let mut client = connect(ep, root.path()).await;
    client.initialize().await;
    let r = client.call("save_and_play", json!({"dir": "tree"})).await;
    assert!(is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["gameUnreachable"], true, "{v}");
}

#[tokio::test]
async fn save_and_play_without_a_reload_message_says_so_and_keeps_the_output() {
    let editor = ([127, 0, 0, 1], free_port()).into();
    let responder: Responder = Box::new(|m| {
        if m["messageID"] == 1 {
            vec![json!({"messageID": 2, "message": "no reload report"})]
        } else {
            vec![]
        }
    });
    let game = FakeGame::start_with(editor, live_state(), responder)
        .await
        .unwrap();
    let ep = Endpoints {
        game: game.addr(),
        editor,
        timeout: Duration::from_millis(600),
    };
    let root = tempfile::tempdir().unwrap();
    make_tree(root.path(), "tree", SAVE);
    let mut client = connect(ep, root.path()).await;
    client.initialize().await;
    let plan = body(&client.call("save_and_play", json!({"dir": "tree"})).await);
    let r = client
        .call(
            "save_and_play",
            json!({"dir": "tree", "confirm": plan["fingerprint"], "quiet_ms": 100}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    assert_eq!(v["sent"], true);
    assert_eq!(v["reloaded"], false);
    assert!(v["note"].as_str().unwrap().contains("get_console"));
    assert_eq!(v["console"][0]["message"], "no reload report");
}

// ---- the held editor port and the event buffer ----

/// Push each document as its own connection, in order, then let the server
/// read them.
async fn push_all(h: &Harness, docs: &[Value]) {
    for doc in docs {
        h.game.push_raw(h.ep.editor, doc).await.unwrap();
    }
    settle().await;
}

fn print_doc(text: &str) -> Value {
    json!({"messageID": 2, "message": text})
}

async fn push_prints(h: &Harness, n: usize) {
    let docs: Vec<Value> = (1..=n).map(|i| print_doc(&format!("p{i}"))).collect();
    push_all(h, &docs).await;
}

/// The body of a successful `get_console` result.
async fn console(h: &mut Harness, args: Value) -> Value {
    let r = h.client.call("get_console", args).await;
    assert!(!is_error(&r), "{r}");
    body(&r)
}

fn seqs(console: &Value) -> Vec<u64> {
    console["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn the_server_starts_without_the_hub_when_the_editor_port_is_busy() {
    let ep = Endpoints::new(1, free_port());
    let _held = Listener::bind(ep.editor).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let server = EditorServer::start(ep, resolve_root(Some(root.path())).unwrap(), 10).await;
    let Err(why) = &server.hub else {
        panic!("the hub started on a busy port");
    };
    assert!(why.contains("already in use"), "{why}");
}

#[tokio::test]
async fn the_port_is_held_while_the_server_runs_and_freed_when_it_ends() {
    let h = plain().await;
    let editor = h.ep.editor;
    let err = Listener::bind(editor).await.err().expect("port is held");
    assert!(matches!(err, EditorError::PortInUse { .. }), "{err:?}");
    drop(h.client);
    let mut freed = false;
    for _ in 0..30 {
        if Listener::bind(editor).await.is_ok() {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(freed, "the port was not released after the client left");
}

#[tokio::test]
async fn messages_without_a_running_call_are_buffered() {
    let mut h = plain().await;
    push_all(
        &h,
        &[
            print_doc("hello"),
            json!({"messageID": 3, "guid": "a1", "error": "boom",
                   "errorMessagePrefix": "[Obj] Lua Error: boom"}),
            json!({"messageID": 4, "customMessage": {"a": 1}}),
            json!({"messageID": 6, "savePath": "s.json"}),
        ],
    )
    .await;
    let v = console(&mut h, json!({})).await;
    assert_eq!(seqs(&v), [1, 2, 3, 4]);
    let events = v["events"].as_array().unwrap();
    assert_eq!(events[0]["type"], "print");
    assert_eq!(events[0]["message"], "hello");
    assert!(events[0]["time"].as_str().unwrap().ends_with('Z'));
    assert_eq!(events[1]["type"], "error");
    assert_eq!(events[1]["guid"], "a1");
    assert_eq!(events[1]["message"], "[Obj] Lua Error: boom");
    assert_eq!(events[2]["type"], "custom");
    assert_eq!(events[2]["customMessage"], json!({"a": 1}));
    assert_eq!(events[3]["type"], "saved");
    assert_eq!(events[3]["savePath"], "s.json");
    assert_eq!(v["nextSince"], 4);
    assert_eq!(v["more"], false);
    assert_eq!(v["dropped"], 0);
}

#[tokio::test]
async fn since_returns_only_newer_events() {
    let mut h = plain().await;
    push_prints(&h, 4).await;
    let v = console(&mut h, json!({"since": 2})).await;
    assert_eq!(seqs(&v), [3, 4]);
    let next = v["nextSince"].clone();
    assert_eq!(next, 4);
    let v = console(&mut h, json!({"since": next})).await;
    assert!(seqs(&v).is_empty());
    assert_eq!(v["nextSince"], next);

    let r = h.client.call("get_console", json!({"since": 99})).await;
    assert!(is_error(&r), "{r}");
    let error = body(&r)["error"].as_str().unwrap().to_owned();
    assert!(error.contains("ahead"), "{error}");
}

#[tokio::test]
async fn kinds_filters_events_and_rejects_bad_values() {
    let mut h = plain().await;
    push_all(
        &h,
        &[
            print_doc("a"),
            json!({"messageID": 3, "guid": "g", "error": "e", "errorMessagePrefix": "p"}),
            print_doc("b"),
        ],
    )
    .await;
    let v = console(&mut h, json!({"kinds": ["error"]})).await;
    assert_eq!(seqs(&v), [2]);
    assert_eq!(v["events"][0]["type"], "error");
    assert_eq!(v["nextSince"], 3);

    let r = h.client.call("get_console", json!({"kinds": []})).await;
    assert!(is_error(&r), "{r}");

    let id = h
        .client
        .send_call("get_console", json!({"kinds": ["foo"]}))
        .await;
    let r = h.client.response(id).await;
    // rmcp reports unparsable arguments as an error result, not a JSON-RPC error.
    assert!(r.get("error").is_none(), "{r}");
    assert!(is_error(&r["result"]), "{r}");
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("unknown variant `foo`"), "{text}");
}

#[tokio::test]
async fn limit_pages_through_the_buffer() {
    let mut h = plain().await;
    push_prints(&h, 5).await;
    let mut since = 0;
    let mut all = Vec::new();
    let mut more = Vec::new();
    for _ in 0..3 {
        let v = console(&mut h, json!({"since": since, "limit": 2})).await;
        all.push(seqs(&v));
        more.push(v["more"].clone());
        since = v["nextSince"].as_u64().unwrap();
    }
    assert_eq!(all, [vec![1, 2], vec![3, 4], vec![5]]);
    assert_eq!(more, [json!(true), json!(true), json!(false)]);

    // Without since: the newest ones.
    let v = console(&mut h, json!({"limit": 2})).await;
    assert_eq!(seqs(&v), [4, 5]);
    assert_eq!(v["omitted"], 3);

    for limit in [0, 1001] {
        let r = h.client.call("get_console", json!({"limit": limit})).await;
        assert!(is_error(&r), "limit {limit}: {r}");
    }
}

#[tokio::test]
async fn wait_ms_waits_for_events_and_then_for_quiet() {
    let mut h = plain().await;
    let game = h.game.clone();
    let editor = h.ep.editor;
    let pusher = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        game.push_raw(editor, &print_doc("one")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        game.push_raw(editor, &print_doc("two")).await.unwrap();
    });
    let started = std::time::Instant::now();
    let v = console(&mut h, json!({"wait_ms": 3000, "quiet_ms": 300})).await;
    let elapsed = started.elapsed();
    pusher.await.unwrap();
    assert_eq!(seqs(&v), [1, 2], "{v}");
    assert!(elapsed < Duration::from_millis(2500), "{elapsed:?}");

    // Nothing new arrives: the wait ends at wait_ms.
    let started = std::time::Instant::now();
    let v = console(&mut h, json!({"since": 2, "wait_ms": 300})).await;
    let elapsed = started.elapsed();
    assert!(seqs(&v).is_empty());
    assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");

    // Without wait_ms it returns at once.
    let started = std::time::Instant::now();
    let v = console(&mut h, json!({"since": 2})).await;
    assert!(seqs(&v).is_empty());
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
async fn get_console_does_not_wait_for_a_running_call() {
    let mut h = plain().await;
    let started = std::time::Instant::now();
    let slow = h
        .client
        .send_call("execute_lua", json!({"code": "x", "quiet_ms": 1500}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let id = h.client.send_call("get_console", json!({})).await;
    let r = h.client.response(id).await;
    assert!(!is_error(&r["result"]), "{r}");
    let answered = started.elapsed();
    assert!(answered < Duration::from_millis(1200), "{answered:?}");
    let r = h.client.response(slow).await;
    assert!(!is_error(&r["result"]), "{r}");
}

#[tokio::test]
async fn the_reply_to_get_scripts_is_not_a_loaded_event() {
    let mut h = harness(two_objects(), "x.json", None).await;
    h.client.call("get_status", json!({})).await;
    h.client.call("get_scripts", json!({})).await;
    settle().await;
    let v = console(&mut h, json!({"kinds": ["loaded"]})).await;
    assert!(seqs(&v).is_empty(), "{v}");
    assert_eq!(v["nextSince"], 0);

    // A load the game announces on its own is one.
    push_all(
        &h,
        &[json!({
            "messageID": 1,
            "scriptStates": [
                {"name": "a", "guid": "a1", "script": "s"},
                {"name": "b", "guid": "b2", "script": "t"}
            ],
            "savePath": "x.json"
        })],
    )
    .await;
    let v = console(&mut h, json!({})).await;
    assert_eq!(seqs(&v), [1]);
    assert_eq!(v["events"][0]["type"], "loaded");
    assert_eq!(v["events"][0]["savePath"], "x.json");
    assert_eq!(v["events"][0]["objectsWithScripts"], 2);
}

#[tokio::test]
async fn execute_lua_reports_the_seq_of_its_output() {
    let mut h = harness(vec![], "", Some(on_execute(vec![print_doc("from lua")]))).await;
    let r = h
        .client
        .call("execute_lua", json!({"code": "x", "quiet_ms": 150}))
        .await;
    assert!(!is_error(&r), "{r}");
    let last = body(&r)["lastSeq"].clone();
    let v = console(&mut h, json!({})).await;
    assert_eq!(seqs(&v), [1]);
    assert_eq!(v["events"][0]["message"], "from lua");
    assert_eq!(last, v["events"][0]["seq"]);
}

#[tokio::test]
async fn save_and_play_console_carries_seqs_and_the_events_stay_readable() {
    let mut h = sp_harness(live_state()).await;
    let plan = body(&h.client.call("save_and_play", json!({"dir": "tree"})).await);
    let r = h
        .client
        .call(
            "save_and_play",
            json!({"dir": "tree", "confirm": plan["fingerprint"], "quiet_ms": 200}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    let v = body(&r);
    let console_seqs: Vec<u64> = v["console"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(console_seqs, [3, 4], "{v}");
    assert_eq!(v["lastSeq"], 4);
    assert!(v.get("dropped").is_none());

    let all = console(&mut h, json!({})).await;
    let types: Vec<&str> = all["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["saved", "loaded", "print", "error"]);
    assert_eq!(seqs(&all), [1, 2, 3, 4]);
    assert_eq!(all["events"][1]["objectsWithScripts"], 1);
}

#[tokio::test]
async fn get_status_reports_the_listener_and_the_buffer() {
    let mut h = plain().await;
    push_prints(&h, 2).await;
    let v = body(&h.client.call("get_status", json!({})).await);
    assert_eq!(v["listener"]["state"], "listening");
    assert_eq!(v["listener"]["address"], h.ep.editor.to_string());
    assert!(v["listener"]["since"].is_string());
    assert!(v["listener"].get("error").is_none());
    assert_eq!(v["buffer"]["lastSeq"], 2);
    assert_eq!(v["buffer"]["events"], 2);
    assert_eq!(v["buffer"]["evicted"], 0);
    // The reply to the status call itself ("get scripts") counts as a message.
    assert!(v["lastMessageAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(v["undecodable"], 0);
    assert!(v["lastUndecodable"].is_null());
}

#[tokio::test]
async fn last_message_at_is_null_when_nothing_ever_arrived() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let game = closed.local_addr().unwrap();
    drop(closed);
    let ep = Endpoints {
        game,
        editor: ([127, 0, 0, 1], free_port()).into(),
        timeout: Duration::from_secs(3),
    };
    let root = tempfile::tempdir().unwrap();
    let mut client = connect(ep, root.path()).await;
    client.initialize().await;
    let v = body(&client.call("get_status", json!({})).await);
    assert!(v["lastMessageAt"].is_null(), "{v}");
    assert_eq!(v["listener"]["state"], "listening");
}

#[tokio::test]
async fn the_buffer_is_bounded_by_max_events() {
    let editor = ([127, 0, 0, 1], free_port()).into();
    let game = FakeGame::start_with_save_path(editor, vec![], "")
        .await
        .unwrap();
    let ep = Endpoints {
        game: game.addr(),
        editor,
        timeout: Duration::from_secs(5),
    };
    let root = tempfile::tempdir().unwrap();
    let mut client = connect_with(ep, root.path(), 3).await;
    client.initialize().await;
    let mut h = Harness {
        client,
        game: Arc::new(game),
        ep,
        root,
    };
    push_prints(&h, 5).await;
    let v = console(&mut h, json!({"since": 0})).await;
    assert_eq!(seqs(&v), [3, 4, 5]);
    assert_eq!(v["dropped"], 2);
    let status = body(&h.client.call("get_status", json!({})).await);
    assert_eq!(status["buffer"]["evicted"], 2);
    assert_eq!(status["buffer"]["events"], 3);
}

#[tokio::test]
async fn an_undecodable_message_is_counted_and_does_not_stop_the_buffer() {
    let mut h = plain().await;
    let mut stream = tokio::net::TcpStream::connect(h.ep.editor).await.unwrap();
    stream.write_all(b"not json").await.unwrap();
    stream.shutdown().await.unwrap();
    drop(stream);
    settle().await;
    push_prints(&h, 1).await;
    let v = body(&h.client.call("get_status", json!({})).await);
    assert_eq!(v["undecodable"], 1, "{v}");
    assert!(v["lastUndecodable"].is_string(), "{v}");
    let c = console(&mut h, json!({})).await;
    assert_eq!(seqs(&c), [1]);
    assert_eq!(c["events"][0]["message"], "p1");
}

// ---- Without the editor port, and the save tools ----

/// A server whose editor port is held by someone else: the editor tools fail,
/// the save tools work.
async fn without_editor(
    fetcher: Option<save_tools::FetcherFactory>,
) -> (Client, tempfile::TempDir) {
    let ep = Endpoints::new(1, free_port());
    let held = Listener::bind(ep.editor).await.unwrap();
    std::mem::forget(held); // busy until the test process ends
    local_with(ep, fetcher).await
}

async fn local_with(
    ep: Endpoints,
    fetcher: Option<save_tools::FetcherFactory>,
) -> (Client, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let mut server = EditorServer::start(ep, resolve_root(Some(root.path())).unwrap(), 10).await;
    if let Some(fetcher) = fetcher {
        server = server.with_fetcher(fetcher);
    }
    let mut client = attach(server).await;
    client.initialize().await;
    (client, root)
}

/// The text of a failed call: the `error` field of its JSON.
fn error_text(result: &Value) -> String {
    assert!(is_error(result), "expected a tool error: {result}");
    body(result)["error"]
        .as_str()
        .expect("an error text")
        .to_owned()
}

const SAVE_A: &str = r#"{
  "SaveName": "T",
  "ObjectStates": [
    { "GUID": "aaaaaa", "Nickname": "Deck", "Transform": { "posX": 5000, "posY": 1, "posZ": 0 },
      "CustomImage": { "ImageURL": "https://example.com/ok.png" } },
    { "GUID": "bbbbbb", "Nickname": "Card",
      "CustomImage": { "ImageURL": "https://example.com/dead.png" } },
    { "GUID": "bbbbbb", "Nickname": "Card copy" }
  ]
}"#;

const SAVE_B: &str = r#"{
  "SaveName": "T",
  "ObjectStates": [
    { "GUID": "aaaaaa", "Nickname": "Deck", "Transform": { "posX": 1, "posY": 1, "posZ": 0 } },
    { "GUID": "dddddd", "Nickname": "Fresh" }
  ]
}"#;

fn write(root: &tempfile::TempDir, name: &str, content: &str) {
    std::fs::write(root.path().join(name), content).unwrap();
}

/// A fake HTTP client: URLs containing "dead" answer 404, the rest 200.
struct FakeFetch;

impl ttskit_core::Fetch for FakeFetch {
    fn fetch(
        &self,
        request: &ttskit_core::FetchRequest<'_>,
    ) -> Result<ttskit_core::FetchResponse, ttskit_core::FetchError> {
        let status = if request.url.contains("dead") {
            404
        } else {
            200
        };
        Ok(ttskit_core::FetchResponse {
            status,
            ..Default::default()
        })
    }
}

fn fake_fetcher() -> save_tools::FetcherFactory {
    Arc::new(|_| Ok(Box::new(FakeFetch) as Box<dyn ttskit_core::Fetch + Send>))
}

#[tokio::test]
async fn the_server_starts_when_the_editor_port_is_busy() {
    let (mut client, root) = without_editor(None).await;
    write(&root, "save.json", SAVE_A);

    let status = client.call("get_status", json!({})).await;
    assert!(!is_error(&status), "{status}");
    let status = body(&status);
    assert_eq!(status["editorAvailable"], false);
    assert_eq!(status["gameReachable"], false);
    assert!(
        status["editorError"]
            .as_str()
            .unwrap()
            .contains("already in use"),
        "{status}"
    );

    let calls = [
        ("get_scripts", json!({})),
        ("execute_lua", json!({"code": "print(1)"})),
        ("send_custom_message", json!({"message": {"a": 1}})),
        ("get_console", json!({})),
        ("save_and_play", json!({"dir": "."})),
    ];
    for (tool, args) in calls {
        let result = client.call(tool, args).await;
        let error = error_text(&result);
        assert!(error.contains("editor port"), "{tool}: {error}");
        assert!(error.contains("already in use"), "{tool}: {error}");
    }

    let scan = client.call("save_scan", json!({"save": "save.json"})).await;
    assert!(!is_error(&scan), "{scan}");
    assert_eq!(body(&scan)["total"], 2);
}

#[tokio::test]
async fn save_scan_groups_asset_urls() {
    let (mut client, root) = without_editor(None).await;
    write(&root, "save.json", SAVE_A);
    let r = client
        .call(
            "save_scan",
            json!({"save": "save.json", "occurrences": true}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    let doc = body(&r);
    assert_eq!(doc["total"], 2);
    assert!(doc["hosts"].is_object(), "{doc}");
    assert!(doc.to_string().contains("https://example.com/dead.png"));

    write(&root, "bad.json", "not json");
    let r = client.call("save_scan", json!({"save": "bad.json"})).await;
    assert!(error_text(&r).contains("bad.json"));
    let r = client
        .call("save_scan", json!({"save": "missing.json"}))
        .await;
    assert!(error_text(&r).contains("missing.json"));
}

#[tokio::test]
async fn save_lint_reports_findings_as_a_normal_result() {
    let (mut client, root) = without_editor(None).await;
    write(&root, "save.json", SAVE_A);
    let r = client.call("save_lint", json!({"save": "save.json"})).await;
    assert!(!is_error(&r), "{r}");
    let doc = body(&r);
    assert!(
        doc["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rule"] == "duplicate-guid"),
        "{doc}"
    );
    assert_eq!(doc["level"], "warning");

    // Options: allow the rule and raise the level.
    let r = client
        .call(
            "save_lint",
            json!({"save": "save.json", "allow": ["duplicate-guid"], "level": "error"}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(body(&r)["summary"]["error"], 0);

    // The project config in the root is the default; a bad one is an error.
    write(&root, "ttskit.toml", "[lint\n");
    let r = client.call("save_lint", json!({"save": "save.json"})).await;
    assert!(error_text(&r).contains("invalid config"));
    write(&root, "ttskit.toml", "");
    let r = client.call("save_lint", json!({"save": "save.json"})).await;
    assert!(!is_error(&r), "{r}");

    let r = client
        .call(
            "save_lint",
            json!({"save": "save.json", "deny": ["no-such-rule"]}),
        )
        .await;
    assert!(is_error(&r));
    let r = client
        .call("save_lint", json!({"save": "save.json", "level": "loud"}))
        .await;
    assert!(error_text(&r).contains("invalid level"));
    let r = client.call("save_lint", json!({"save": "nope.json"})).await;
    assert!(error_text(&r).contains("nope.json"));
}

#[tokio::test]
async fn save_diff_equal_and_different() {
    let (mut client, root) = without_editor(None).await;
    write(&root, "a.json", SAVE_A);
    write(&root, "b.json", SAVE_B);

    let r = client
        .call("save_diff", json!({"old": "a.json", "new": "a.json"}))
        .await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(body(&r)["objects"], json!([]));

    let r = client
        .call(
            "save_diff",
            json!({"old": "a.json", "new": "b.json", "scripts": true}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    let doc = body(&r);
    assert!(!doc["objects"].as_array().unwrap().is_empty(), "{doc}");
    assert!(doc.to_string().contains("Fresh"), "{doc}");

    let args = json!({"old": "a.json", "new": "b.json", "tolerance": -1.0});
    let r = client.call("save_diff", args).await;
    assert!(error_text(&r).contains("tolerance"));
    let args = json!({"old": "a.json", "new": "b.json", "tolerance": 10000.0});
    let r = client.call("save_diff", args).await;
    assert!(!is_error(&r), "{r}");
    let r = client
        .call("save_diff", json!({"old": "a.json", "new": "missing.json"}))
        .await;
    assert!(error_text(&r).contains("missing.json"));
}

#[tokio::test]
async fn save_check_urls_reports_dead_urls_as_a_normal_result() {
    let (mut client, root) = without_editor(Some(fake_fetcher())).await;
    write(&root, "save.json", SAVE_A);
    let r = client
        .call(
            "save_check_urls",
            json!({"save": "save.json", "retries": 0}),
        )
        .await;
    assert!(!is_error(&r), "{r}");
    let doc = body(&r);
    assert_eq!(doc["summary"]["total"], 2);
    assert_eq!(doc["summary"]["dead"], 1);
    assert_eq!(doc["summary"]["ok"], 1);
    let urls = doc["urls"].as_array().unwrap();
    let dead = urls.iter().find(|u| u["result"] == "dead").unwrap();
    assert_eq!(dead["url"], "https://example.com/dead.png");
    assert_eq!(dead["status"], 404);

    // Host filters work as in the CLI.
    let r = client
        .call(
            "save_check_urls",
            json!({"save": "save.json", "skip_host": ["example.com"]}),
        )
        .await;
    assert_eq!(body(&r)["summary"]["skipped"], 2);

    // Failures: bad arguments, an unreadable save.
    let r = client
        .call(
            "save_check_urls",
            json!({"save": "save.json", "concurrency": 0}),
        )
        .await;
    assert!(error_text(&r).contains("concurrency"));
    let r = client
        .call(
            "save_check_urls",
            json!({"save": "save.json", "timeout": 601}),
        )
        .await;
    assert!(error_text(&r).contains("timeout"));
    let r = client
        .call("save_check_urls", json!({"save": "gone.json"}))
        .await;
    assert!(error_text(&r).contains("gone.json"));
}

#[tokio::test]
async fn save_check_urls_reports_a_failing_client() {
    let failing: save_tools::FetcherFactory = Arc::new(|_| Err("no client today".to_owned()));
    let (mut client, root) = without_editor(Some(failing)).await;
    write(&root, "save.json", SAVE_A);
    let r = client
        .call("save_check_urls", json!({"save": "save.json"}))
        .await;
    assert_eq!(error_text(&r), "no client today");
}

#[tokio::test]
async fn manifest_show_reads_the_manifest_in_the_root() {
    let (mut client, root) = without_editor(None).await;
    // Missing: the empty manifest, as the CLI shows.
    let r = client.call("manifest_show", json!({})).await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(body(&r)["entries"], json!([]));

    let manifest = r#"{
  "version": 1,
  "entries": [
    {
      "path": "a.png",
      "cloud_path": "Game/a.png",
      "sha1": "0123456789ABCDEF0123456789ABCDEF01234567",
      "size": 3,
      "url": "https://example.invalid/ugc/1/0123456789ABCDEF0123456789ABCDEF01234567/"
    }
  ]
}
"#;
    write(&root, "ttskit.manifest.json", manifest);
    let r = client.call("manifest_show", json!({})).await;
    assert!(!is_error(&r), "{r}");
    assert_eq!(body(&r)["entries"][0]["path"], "a.png");
    write(&root, "other.json", manifest);
    let r = client
        .call("manifest_show", json!({"manifest": "other.json"}))
        .await;
    assert_eq!(body(&r)["entries"][0]["cloud_path"], "Game/a.png");

    write(&root, "broken.json", "{");
    let r = client
        .call("manifest_show", json!({"manifest": "broken.json"}))
        .await;
    assert!(is_error(&r));
    let r = client
        .call("manifest_show", json!({"manifest": "absent.json"}))
        .await;
    assert!(is_error(&r));
}

#[tokio::test]
async fn every_save_tool_refuses_paths_outside_the_root() {
    let (mut client, root) = without_editor(Some(fake_fetcher())).await;
    write(&root, "save.json", SAVE_A);
    let outside = tempfile::tempdir().unwrap();
    let outside_save = outside.path().join("save.json");
    std::fs::write(&outside_save, SAVE_A).unwrap();
    let outside_text = outside_save.to_str().unwrap();

    for path in ["../save.json", outside_text] {
        let calls = [
            ("save_scan", json!({"save": path})),
            ("save_lint", json!({"save": path})),
            ("save_lint", json!({"save": "save.json", "config": path})),
            ("save_diff", json!({"old": path, "new": "save.json"})),
            ("save_diff", json!({"old": "save.json", "new": path})),
            ("save_check_urls", json!({"save": path})),
            ("manifest_show", json!({"manifest": path})),
        ];
        for (tool, args) in calls {
            let r = client.call(tool, args.clone()).await;
            let error = error_text(&r);
            assert!(
                error.contains("`..`") || error.contains("outside the project root"),
                "{tool} {args}: {error}"
            );
        }
    }
}
