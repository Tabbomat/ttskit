//! Byte-exact round trip of the synthetic fixture, which is written in the
//! exact format Tabletop Simulator uses (CRLF, no trailing newline).

use ttskit_core::Save;

#[test]
fn fixture_round_trips_byte_for_byte() {
    let input = ttskit_testutil::read_fixture("minimal_save.json");

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
