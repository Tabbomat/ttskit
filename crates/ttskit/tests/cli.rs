use std::process::Command;

fn ttskit(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn help_mentions_cloud() {
    let output = ttskit(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("cloud"));
}

#[test]
fn no_arguments_shows_help_and_fails() {
    let output = ttskit(&[]);
    assert_ne!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage"));
}

#[cfg(not(feature = "steam"))]
#[test]
fn cloud_list_without_steam_feature_fails() {
    let output = ttskit(&["cloud", "list"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--features steam"));
}
