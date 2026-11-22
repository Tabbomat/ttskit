//! `ttskit save lint` end to end. Local files only.

use std::process::{Command, Output};

/// One duplicate GUID (error) and one position out of bounds (warning) on a
/// named object, one missing GUID in a container (info), and a clean object.
const SAVE: &str = r#"{
  "SaveName": "T",
  "ObjectStates": [
    { "GUID": "aaaaaa", "Nickname": "Deck", "Transform": { "posX": 5000, "posY": 1, "posZ": 0 },
      "ContainedObjects": [ { "Nickname": "Inner" } ] },
    { "GUID": "bbbbbb", "Nickname": "Card" },
    { "GUID": "bbbbbb", "Nickname": "Card copy" }
  ]
}"#;

const CLEAN: &str = r#"{ "ObjectStates": [ { "GUID": "aaaaaa", "Nickname": "Ok" } ] }"#;

struct Dir(tempfile::TempDir);

impl Dir {
    fn new() -> Dir {
        let d = Dir(tempfile::tempdir().unwrap());
        d.write("save.json", SAVE);
        d
    }
    fn path(&self, name: &str) -> String {
        self.0.path().join(name).to_str().unwrap().to_owned()
    }
    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.0.path().join(name), content).unwrap();
    }
    /// Runs `ttskit save lint ARGS` inside the directory, so no stray
    /// ttskit.toml of the caller is picked up.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ttskit"))
            .current_dir(self.0.path())
            .args(["save", "lint"])
            .args(args)
            .output()
            .unwrap()
    }
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

#[test]
fn findings_exit_2_and_are_grouped_by_object() {
    let d = Dir::new();
    let o = d.run(&["save.json"]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    let t = text(&o);
    assert!(
        t.contains("Deck (aaaaaa)\n  warning  position-out-of-bounds  ObjectStates[0]: "),
        "{t}"
    );
    assert!(
        t.contains("Card copy (bbbbbb)\n  error    duplicate-guid  ObjectStates[2]: "),
        "{t}"
    );
    // The default level hides info findings, and says so.
    assert!(!t.contains("missing-guid-nested"), "{t}");
    assert!(
        t.contains(
            "2 findings at or above warning: 1 error, 1 warning, 0 info; 1 below warning hidden"
        ),
        "{t}"
    );
}

#[test]
fn clean_save_exits_0() {
    let d = Dir::new();
    d.write("clean.json", CLEAN);
    let o = d.run(&["clean.json"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(text(&o), "no findings at or above warning\n");
}

#[test]
fn level_filters_what_is_shown_and_counted() {
    let d = Dir::new();
    let o = d.run(&["save.json", "--level", "error"]);
    assert_eq!(code(&o), 2);
    let t = text(&o);
    assert!(t.contains("duplicate-guid") && !t.contains("position-out-of-bounds"));
    let o = d.run(&["save.json", "--level", "info"]);
    assert!(text(&o).contains("missing-guid-nested"));
    // Nothing at error level once the error rule is off.
    let o = d.run(&["save.json", "--level", "error", "--allow", "duplicate-guid"]);
    assert_eq!(code(&o), 0);
    let o = d.run(&["save.json", "--level", "loud"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
}

#[test]
fn allow_warn_and_deny_override_rules() {
    let d = Dir::new();
    let o = d.run(&[
        "save.json",
        "--allow",
        "duplicate-guid",
        "--allow",
        "position-out-of-bounds",
    ]);
    assert_eq!(code(&o), 0, "{}", text(&o));
    let o = d.run(&[
        "save.json",
        "--level",
        "error",
        "--deny",
        "position-out-of-bounds",
    ]);
    assert_eq!(code(&o), 2);
    assert!(text(&o).contains("error    position-out-of-bounds"));
    let o = d.run(&["save.json", "--warn", "duplicate-guid", "--level", "error"]);
    assert_eq!(code(&o), 0, "{}", text(&o));
}

#[test]
fn unknown_rule_id_is_an_error_naming_it() {
    let d = Dir::new();
    for flag in ["--allow", "--warn", "--deny"] {
        let o = d.run(&["save.json", flag, "no-such-rule"]);
        assert_eq!(code(&o), 1, "{flag}");
        assert!(err(&o).contains("no-such-rule"), "{}", err(&o));
    }
}

#[test]
fn config_file_rules_and_state_limit() {
    let d = Dir::new();
    d.write(
        "lint.toml",
        "[lint]\nstate_limit = 3\n[lint.rules]\nduplicate-guid = \"off\"\nposition-out-of-bounds = \"error\"\n",
    );
    d.write(
        "state.json",
        r#"{ "ObjectStates": [ { "GUID": "aaaaaa", "LuaScriptState": "abcd" } ] }"#,
    );
    let o = d.run(&["save.json", "--config", &d.path("lint.toml")]);
    let t = text(&o);
    assert_eq!(code(&o), 2);
    assert!(!t.contains("duplicate-guid"), "{t}");
    assert!(t.contains("error    position-out-of-bounds"), "{t}");
    let o = d.run(&["state.json", "--config", &d.path("lint.toml")]);
    assert_eq!(code(&o), 2);
    assert!(text(&o).contains("large-script-state"));
    // The command line wins over the file.
    let o = d.run(&[
        "save.json",
        "--config",
        &d.path("lint.toml"),
        "--deny",
        "duplicate-guid",
    ]);
    assert!(text(&o).contains("duplicate-guid"));
}

#[test]
fn ttskit_toml_in_the_current_directory_is_used_and_shared() {
    let d = Dir::new();
    d.write(
        "ttskit.toml",
        "[cloud]\nfolder = \"G\"\n[local]\ndir = \"x\"\n[image]\n[[image.pipeline]]\nsrc = \"a/*.png\"\nout = \"b\"\n[lint.rules]\nduplicate-guid = \"off\"\nposition-out-of-bounds = \"off\"\n",
    );
    let o = d.run(&["save.json"]);
    assert_eq!(code(&o), 0, "{} {}", text(&o), err(&o));
}

#[test]
fn missing_explicit_config_and_bad_config_are_errors() {
    let d = Dir::new();
    let o = d.run(&["save.json", "--config", &d.path("nope.toml")]);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("nope.toml"), "{}", err(&o));
    d.write("bad.toml", "[lint.rules]\nbogus = \"off\"\n");
    let o = d.run(&["save.json", "--config", &d.path("bad.toml")]);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("bogus"), "{}", err(&o));
    d.write("bad2.toml", "[lint]\nbogus = 1\n");
    assert_eq!(
        code(&d.run(&["save.json", "--config", &d.path("bad2.toml")])),
        1
    );
}

#[test]
fn unreadable_and_invalid_saves_are_errors() {
    let d = Dir::new();
    d.write("bad.json", "{ not json");
    d.write("array.json", "[1]");
    for name in ["bad.json", "array.json", "missing.json"] {
        let o = d.run(&[name]);
        assert_eq!(code(&o), 1, "{name}");
        assert!(err(&o).contains(name), "{}", err(&o));
    }
    // A save is required unless --list-rules is given.
    assert_eq!(code(&d.run(&[])), 2);
}

#[test]
fn json_output_shape() {
    let d = Dir::new();
    let o = d.run(&["save.json", "--json", "--level", "info"]);
    assert_eq!(code(&o), 2);
    let v: serde_json::Value = serde_json::from_str(&text(&o)).unwrap();
    assert_eq!(v["save"], "save.json");
    assert_eq!(v["level"], "info");
    assert_eq!(v["summary"]["error"], 1);
    assert_eq!(v["summary"]["warning"], 1);
    assert_eq!(v["summary"]["info"], 1);
    let findings = v["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 3);
    let dup = findings
        .iter()
        .find(|f| f["rule"] == "duplicate-guid")
        .unwrap();
    assert_eq!(dup["severity"], "error");
    assert_eq!(dup["path"], "ObjectStates[2]");
    assert_eq!(dup["guid"], "bbbbbb");
    assert_eq!(dup["nickname"], "Card copy");
    assert!(dup["message"].as_str().unwrap().contains("bbbbbb"));
    let nested = findings
        .iter()
        .find(|f| f["rule"] == "missing-guid-nested")
        .unwrap();
    assert_eq!(nested["guid"], "aaaaaa");
    // A finding without an owner has null nickname and guid.
    d.write("noguid.json", r#"{ "ObjectStates": [ {} ] }"#);
    let o = d.run(&["noguid.json", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&text(&o)).unwrap();
    assert!(v["findings"][0]["guid"].is_null());
    assert!(v["findings"][0]["nickname"].is_null());
    // A clean save is an empty list and exit 0.
    d.write("clean.json", CLEAN);
    let o = d.run(&["clean.json", "--json"]);
    assert_eq!(code(&o), 0);
    let v: serde_json::Value = serde_json::from_str(&text(&o)).unwrap();
    assert_eq!(v["findings"].as_array().unwrap().len(), 0);
}

#[test]
fn list_rules_prints_every_rule_without_a_save() {
    let d = Dir::new();
    let o = d.run(&["--list-rules"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&o);
    assert_eq!(t.lines().count(), ttskit_lint::RULES.len());
    for rule in ttskit_lint::RULES {
        let line = t
            .lines()
            .find(|l| l.split_whitespace().next() == Some(rule.id))
            .unwrap_or_else(|| panic!("{} missing:\n{t}", rule.id));
        assert!(line.contains(rule.default.as_str()), "{line}");
        assert!(line.contains(rule.summary), "{line}");
    }
}
