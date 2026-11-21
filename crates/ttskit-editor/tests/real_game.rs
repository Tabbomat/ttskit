//! Manual check against a real running game. Not run by default:
//! `cargo test -p ttskit-editor --test real_game -- --ignored --nocapture`
//! Needs the game running with a save loaded, and ports 39998/39999 free of
//! other editor integrations.

use ttskit_editor::{DEFAULT_TIMEOUT, EDITOR_PORT, GAME_PORT, Incoming, Listener, Outgoing, send};

#[tokio::test]
#[ignore = "needs a running Tabletop Simulator"]
async fn get_scripts_against_real_game() {
    let listener = Listener::bind(([127, 0, 0, 1], EDITOR_PORT).into())
        .await
        .unwrap();
    send(
        ([127, 0, 0, 1], GAME_PORT).into(),
        &Outgoing::GetScripts,
        DEFAULT_TIMEOUT,
    )
    .await
    .unwrap();
    let msg = listener.next_message(DEFAULT_TIMEOUT).await.unwrap();
    let Incoming::LoadGame { script_states, .. } = msg else {
        panic!("expected LoadGame, got {msg:?}");
    };
    println!("received {} script states", script_states.len());
}
