//! Byte-exact round trip of the synthetic fixture, which is written in the
//! exact format Tabletop Simulator uses (CRLF, no trailing newline).

use std::path::Path;

use ttskit_core::Save;

#[test]
fn fixture_round_trips_byte_for_byte() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("minimal_save.json");
    let input = std::fs::read(&path).expect("fixture should be readable");

    assert!(
        input.windows(2).any(|w| w == b"\r\n"),
        "fixture must contain CRLF line endings"
    );
    assert!(
        !input.ends_with(b"\n"),
        "fixture must not end with a newline"
    );

    let save = Save::from_slice(&input).unwrap();
    assert_eq!(save.to_vec(), input);
}
