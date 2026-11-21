use std::net::SocketAddr;
use std::time::Duration;

use serde_json::json;
use ttskit_editor::fake::FakeGame;
use ttskit_editor::{EditorError, Incoming, Listener, Outgoing, ScriptState, send};

const T: Duration = Duration::from_secs(5);

fn any_local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

async fn editor() -> (Listener, SocketAddr) {
    let l = Listener::bind(any_local()).await.unwrap();
    let a = l.local_addr().unwrap();
    (l, a)
}

fn global(script: &str) -> ScriptState {
    ScriptState {
        name: "Global".into(),
        guid: "-1".into(),
        script: Some(script.into()),
        ui: None,
    }
}

#[tokio::test]
async fn get_scripts_round_trip() {
    let (listener, editor_addr) = editor().await;
    let game = FakeGame::start(editor_addr, vec![global("print(1)")])
        .await
        .unwrap();
    send(game.addr(), &Outgoing::GetScripts, T).await.unwrap();
    let msg = listener.next_message(T).await.unwrap();
    assert_eq!(
        msg,
        Incoming::LoadGame {
            script_states: vec![global("print(1)")],
            save_path: Some(String::new())
        }
    );
    assert_eq!(game.received(), vec![json!({"messageID": 0})]);
}

#[tokio::test]
async fn every_outgoing_message_reaches_the_game() {
    let (_listener, editor_addr) = editor().await;
    let game = FakeGame::start(editor_addr, vec![]).await.unwrap();
    let msgs = [
        Outgoing::SaveAndPlay {
            script_states: vec![global("x")],
        },
        Outgoing::Custom {
            custom_message: json!({"a": 1}),
        },
        Outgoing::ExecuteLua {
            guid: "-1".into(),
            script: "return 1".into(),
        },
    ];
    for m in &msgs {
        send(game.addr(), m, T).await.unwrap();
    }
    // Delivery order across separate connections is not guaranteed; wait for all.
    for _ in 0..100 {
        if game.received().len() == msgs.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut got: Vec<_> = game
        .received()
        .iter()
        .map(|v| v["messageID"].as_u64().unwrap())
        .collect();
    got.sort();
    assert_eq!(got, vec![1, 2, 3]);
}

#[tokio::test]
async fn listener_receives_pushed_messages() {
    let (listener, editor_addr) = editor().await;
    let game = FakeGame::start(editor_addr, vec![]).await.unwrap();
    game.push(
        editor_addr,
        &Incoming::Print {
            message: "hi".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        listener.next_message(T).await.unwrap(),
        Incoming::Print {
            message: "hi".into()
        }
    );
    // A malformed document is an error but the listener keeps working.
    game.push_raw(editor_addr, &json!({"nope": 1}))
        .await
        .unwrap();
    assert!(matches!(
        listener.next_message(T).await,
        Err(EditorError::Codec(_))
    ));
    game.push_raw(editor_addr, &json!({"messageID": 99}))
        .await
        .unwrap();
    assert!(matches!(
        listener.next_message(T).await.unwrap(),
        Incoming::Unknown { message_id: 99, .. }
    ));
}

#[tokio::test]
async fn port_in_use_is_a_clear_error() {
    let (_l, addr) = editor().await;
    let err = Listener::bind(addr).await.err().unwrap();
    assert!(matches!(err, EditorError::PortInUse { .. }), "{err:?}");
    assert!(err.to_string().contains("already in use"));
}

#[tokio::test]
async fn connect_failure_and_read_timeout() {
    let dead = {
        let l = tokio::net::TcpListener::bind(any_local()).await.unwrap();
        l.local_addr().unwrap()
    };
    let err = send(dead, &Outgoing::GetScripts, T).await.unwrap_err();
    assert!(matches!(err, EditorError::Connect { .. }), "{err:?}");

    // A peer that connects but never closes triggers the read timeout.
    let (listener, addr) = editor().await;
    let _stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
    let err = listener
        .next_message(Duration::from_millis(100))
        .await
        .unwrap_err();
    assert!(matches!(err, EditorError::Timeout(_)), "{err:?}");
}

#[tokio::test]
async fn fetch_scripts_skips_unrelated_messages() {
    let (listener, editor_addr) = editor().await;
    let game = FakeGame::start(editor_addr, vec![global("print(1)")])
        .await
        .unwrap();
    // A print arrives before the reply is read; it must not be taken for it.
    game.push(
        editor_addr,
        &Incoming::Print {
            message: "hello".into(),
        },
    )
    .await
    .unwrap();
    let scripts = ttskit_editor::fetch_scripts(&listener, game.addr(), T)
        .await
        .unwrap();
    assert_eq!(scripts, vec![global("print(1)")]);
}

#[tokio::test]
async fn fetch_scripts_times_out_without_a_reply() {
    let (listener, _) = editor().await;
    // Something listens on the game port but never answers.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let err = ttskit_editor::fetch_scripts(
        &listener,
        silent.local_addr().unwrap(),
        Duration::from_millis(200),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, EditorError::Timeout(_)), "{err:?}");
}

// ---- execute Lua and custom messages -------------------------------------

use ttskit_editor::{GLOBAL_GUID, execute_lua, send_custom};

const QUIET: Duration = Duration::from_millis(150);

async fn exec_with(
    replies: Vec<serde_json::Value>,
    limit: Duration,
) -> (
    Result<ttskit_editor::ExecOutcome, EditorError>,
    Vec<serde_json::Value>,
) {
    let (listener, editor_addr) = editor().await;
    let game = FakeGame::start_with(
        editor_addr,
        vec![],
        Box::new(move |m| {
            if m["messageID"] == 3 {
                replies.clone()
            } else {
                vec![]
            }
        }),
    )
    .await
    .unwrap();
    let r = execute_lua(
        &listener,
        game.addr(),
        GLOBAL_GUID,
        "return 42",
        limit,
        QUIET,
    )
    .await;
    (r, game.received())
}

#[tokio::test]
async fn exec_sends_guid_and_script_and_reads_the_return_value() {
    let (r, received) = exec_with(
        vec![json!({"messageID": 5, "returnValue": {"a": [1, 2]}})],
        T,
    )
    .await;
    let r = r.unwrap();
    assert_eq!(r.return_value, Some(json!({"a": [1, 2]})));
    assert!(r.errors.is_empty());
    assert_eq!(received[0]["messageID"], 3);
    assert_eq!(received[0]["guid"], "-1");
    assert_eq!(received[0]["script"], "return 42");
}

#[tokio::test]
async fn exec_collects_prints_until_quiet_and_has_no_return_value() {
    let (r, _) = exec_with(
        vec![
            json!({"messageID": 2, "message": "one"}),
            json!({"messageID": 2, "message": "two"}),
        ],
        T,
    )
    .await;
    let r = r.unwrap();
    assert_eq!(r.prints, ["one", "two"]);
    assert_eq!(r.return_value, None);
}

#[tokio::test]
async fn exec_without_any_reply_is_an_empty_outcome_not_an_error() {
    let (r, _) = exec_with(vec![], T).await;
    assert_eq!(r.unwrap(), ttskit_editor::ExecOutcome::default());
}

#[tokio::test]
async fn exec_reports_lua_errors() {
    let (r, _) = exec_with(
        vec![json!({
            "messageID": 3, "guid": "-1",
            "error": "[Global] executeScript:(1,5-13): boom",
            "errorMessagePrefix": "[Global] Lua Error <executeScript>: boom"
        })],
        T,
    )
    .await;
    let r = r.unwrap();
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.errors[0].guid, "-1");
    assert!(r.errors[0].error.contains("boom"));
}

#[tokio::test]
async fn exec_error_for_unknown_guid_keeps_the_games_prefix() {
    let (r, _) = exec_with(
        vec![json!({
            "messageID": 3, "guid": "-1", "error": "zzzzzz",
            "errorMessagePrefix": "Error finding guid for execute script: "
        })],
        T,
    )
    .await;
    assert_eq!(
        r.unwrap().errors[0].prefix,
        "Error finding guid for execute script: "
    );
}

#[tokio::test]
async fn exec_skips_unrelated_and_broken_messages() {
    let (r, _) = exec_with(
        vec![
            json!({"messageID": 7, "guid": "abc"}),
            json!({"messageID": 2}),
            json!({"messageID": 5, "returnValue": 1}),
        ],
        T,
    )
    .await;
    assert_eq!(r.unwrap().return_value, Some(json!(1)));
}

#[tokio::test]
async fn exec_stops_at_the_limit_when_messages_keep_coming() {
    // A chatty game keeps the quiet window from ever elapsing.
    let (listener, editor_addr) = editor().await;
    let game = FakeGame::start(editor_addr, vec![]).await.unwrap();
    let addr = game.addr();
    let spam = tokio::spawn(async move {
        for _ in 0..40 {
            let _ = game
                .push(
                    editor_addr,
                    &Incoming::Print {
                        message: "tick".into(),
                    },
                )
                .await;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        game
    });
    let started = std::time::Instant::now();
    let r = execute_lua(
        &listener,
        addr,
        "-1",
        "x",
        Duration::from_millis(400),
        Duration::from_millis(300),
    )
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!r.prints.is_empty());
    spam.abort();
}

#[tokio::test]
async fn exec_fails_when_the_game_is_not_running() {
    let (listener, _) = editor().await;
    let dead = {
        let l = tokio::net::TcpListener::bind(any_local()).await.unwrap();
        l.local_addr().unwrap()
    };
    let err = execute_lua(&listener, dead, "-1", "x", T, QUIET)
        .await
        .unwrap_err();
    assert!(matches!(err, EditorError::Connect { .. }), "{err:?}");
}

#[tokio::test]
async fn custom_message_is_delivered_as_an_object() {
    let (_l, editor_addr) = editor().await;
    let game = FakeGame::start(editor_addr, vec![]).await.unwrap();
    send_custom(game.addr(), json!({"hello": "world"}), T)
        .await
        .unwrap();
    for _ in 0..40 {
        if !game.received().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let got = game.received();
    assert_eq!(got[0]["messageID"], 2);
    assert_eq!(got[0]["customMessage"], json!({"hello": "world"}));
}

#[tokio::test]
async fn custom_message_that_is_not_an_object_is_refused_before_sending() {
    let dead = {
        let l = tokio::net::TcpListener::bind(any_local()).await.unwrap();
        l.local_addr().unwrap()
    };
    for bad in [json!(5), json!("s"), json!([1]), json!(null)] {
        let err = send_custom(dead, bad, T).await.unwrap_err();
        assert!(matches!(err, EditorError::CustomNotObject), "{err:?}");
    }
}
