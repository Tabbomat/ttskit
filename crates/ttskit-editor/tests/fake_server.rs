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
