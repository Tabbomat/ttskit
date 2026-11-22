//! `ttskit save diff` end to end. Local files only.

use std::process::{Command, Output};

const OLD: &str = r#"{
  "ObjectStates": [
    { "GUID": "aaaaaa", "Nickname": "Bag", "Transform": { "posX": 1.0, "posY": 1, "posZ": 0 },
      "ContainedObjects": [ { "GUID": "bbbbbb", "Nickname": "Card" } ] },
    { "GUID": "cccccc", "Nickname": "Gone" }
  ]
}"#;

const NEW: &str = r#"{
  "ObjectStates": [
    { "GUID": "aaaaaa", "Nickname": "Bag", "Transform": { "posX": 1.001, "posY": 1, "posZ": 0 } },
    { "GUID": "bbbbbb", "Nickname": "Card" },
    { "GUID": "dddddd", "Nickname": "Fresh" }
  ]
}"#;

struct Dir(tempfile::TempDir);

impl Dir {
    fn new() -> Dir {
        let d = Dir(tempfile::tempdir().unwrap());
        d.write("old.json", OLD);
        d.write("new.json", NEW);
        d
    }
    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.0.path().join(name), content).unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ttskit"))
            .current_dir(self.0.path())
            .args(["save", "diff"])
            .args(args)
            .output()
            .unwrap()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn different_saves_exit_2_and_list_changes() {
    let d = Dir::new();
    let out = d.run(&["old.json", "new.json"]);
    assert_eq!(out.status.code(), Some(2));
    let text = stdout(&out);
    assert!(text.contains("Fresh (dddddd)"), "{text}");
    assert!(text.contains("Gone (cccccc)"), "{text}");
    assert!(text.contains("Card (bbbbbb)"), "{text}");
    assert!(text.contains("transform"), "{text}");
}

#[test]
fn equal_saves_exit_0() {
    let d = Dir::new();
    let out = d.run(&["old.json", "old.json"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("no differences"));
}

#[test]
fn tolerance_hides_small_transform_changes() {
    let d = Dir::new();
    d.write(
        "a.json",
        r#"{"ObjectStates":[{"GUID":"aaaaaa","Transform":{"posX":1.0}}]}"#,
    );
    d.write(
        "b.json",
        r#"{"ObjectStates":[{"GUID":"aaaaaa","Transform":{"posX":1.001}}]}"#,
    );
    assert_eq!(d.run(&["a.json", "b.json"]).status.code(), Some(2));
    assert_eq!(
        d.run(&["a.json", "b.json", "--tolerance", "0.01"])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn json_output_parses_and_is_stable() {
    let d = Dir::new();
    let first = d.run(&["old.json", "new.json", "--json"]);
    let second = d.run(&["old.json", "new.json", "--json"]);
    assert_eq!(first.status.code(), Some(2));
    assert_eq!(first.stdout, second.stdout);
    let value: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert!(value["objects"].is_array());
}

#[test]
fn errors_exit_1() {
    let d = Dir::new();
    d.write("bad.json", "not json");
    assert_eq!(d.run(&["old.json", "missing.json"]).status.code(), Some(1));
    assert_eq!(d.run(&["old.json", "bad.json"]).status.code(), Some(1));
    assert_eq!(
        d.run(&["old.json", "new.json", "--tolerance=-1"])
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        d.run(&["old.json", "new.json", "--tolerance", "x"])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn help_does_not_overflow_the_stack() {
    let d = Dir::new();
    assert_eq!(d.run(&["--help"]).status.code(), Some(0));
}
