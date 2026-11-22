//! `ttskit save rewrite-urls` end to end. Local files only.

use std::path::Path;
use std::process::{Command, Output};

const A: &str = "https://example.com/a.png";
const B: &str = "https://example.org/b.png";
const UNUSED: &str = "https://example.com/unused.png";
const SHA: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

fn save_text() -> String {
    format!(
        "{{\r\n  \"SaveName\": \"T\",\r\n  \"LuaScript\": \"u = '{A}'\",\r\n  \"ObjectStates\": [\r\n    {{\r\n      \"GUID\": \"aaaaaa\",\r\n      \"Nickname\": \"Deck\",\r\n      \"FaceURL\": \"{A}\",\r\n      \"Scale\": 1.0\r\n    }}\r\n  ]\r\n}}"
    )
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(["save", "rewrite-urls"])
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

struct Dir(tempfile::TempDir);

impl Dir {
    fn new() -> Dir {
        let d = Dir(tempfile::tempdir().unwrap());
        d.write("save.json", &save_text());
        d.write("map.tsv", &format!("# map\n{A}\t{B}\n{UNUSED}\t{B}\n"));
        d
    }
    fn path(&self, name: &str) -> String {
        self.0.path().join(name).to_str().unwrap().to_owned()
    }
    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.0.path().join(name), content).unwrap();
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.path().join(name)).unwrap()
    }
    fn exists(&self, name: &str) -> bool {
        Path::new(&self.path(name)).exists()
    }
}

#[test]
fn writes_a_new_file_and_leaves_the_input_alone() {
    let d = Dir::new();
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("map.tsv"),
        "--out",
        &d.path("new.json"),
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(d.read("save.json"), save_text());
    // Only the whole value changes; the script is left alone by default.
    assert_eq!(
        d.read("new.json"),
        save_text().replace(
            &format!("\"FaceURL\": \"{A}\""),
            &format!("\"FaceURL\": \"{B}\"")
        )
    );
    let out = text(&o);
    assert!(
        out.contains("line 2: https://example.com/a.png -> https://example.org/b.png"),
        "{out}"
    );
    assert!(out.contains("1 in values, in 1 objects"), "{out}");
    assert!(out.contains("Deck (aaaaaa): 1"), "{out}");
    assert!(
        out.contains("1 in scripts (not replaced, use --in-scripts)"),
        "{out}"
    );
    assert!(out.contains("1 mapping lines match nothing"), "{out}");
    assert!(
        out.contains("line 3: https://example.com/unused.png"),
        "{out}"
    );
}

#[test]
fn in_scripts_and_round_trip() {
    let d = Dir::new();
    d.write("back.tsv", &format!("{B}\t{A}\n"));
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("map.tsv"),
        "--in-scripts",
        "--out",
        &d.path("new.json"),
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(d.read("new.json").contains(&format!("u = '{B}'")));
    let o = run(&[
        &d.path("new.json"),
        "--map",
        &d.path("back.tsv"),
        "--in-scripts",
        "--out",
        &d.path("again.json"),
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(d.read("again.json"), save_text());
}

#[test]
fn dry_run_writes_nothing_and_needs_no_destination() {
    let d = Dir::new();
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("map.tsv"),
        "--dry-run",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(text(&o).contains("Dry run: nothing was written."));
    assert_eq!(d.read("save.json"), save_text());
}

#[test]
fn needs_a_destination_and_never_overwrites_silently() {
    let d = Dir::new();
    let o = run(&[&d.path("save.json"), "--map", &d.path("map.tsv")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--out"), "{}", err(&o));

    d.write("new.json", "keep");
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("map.tsv"),
        "--out",
        &d.path("new.json"),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--yes"), "{}", err(&o));
    assert_eq!(d.read("new.json"), "keep");

    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("map.tsv"),
        "--out",
        &d.path("save.json"),
        "--yes",
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--in-place"), "{}", err(&o));
    assert_eq!(d.read("save.json"), save_text());
}

#[test]
fn in_place_needs_yes() {
    let d = Dir::new();
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("map.tsv"),
        "--in-place",
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(d.read("save.json"), save_text());
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("map.tsv"),
        "--in-place",
        "--yes",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(d.read("save.json").contains(B));
    assert!(!d.exists(".save.json.ttskit-tmp"));
}

#[test]
fn bad_mapping_is_reported_by_line_and_changes_nothing() {
    let d = Dir::new();
    d.write("bad.tsv", &format!("{A}\t{B}\nno tab\n{A}\tnope\n"));
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("bad.tsv"),
        "--out",
        &d.path("new.json"),
    ]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("line 2") && e.contains("line 3"), "{e}");
    assert!(!d.exists("new.json"));
}

#[test]
fn conflicting_duplicate_is_an_error_identical_one_is_merged() {
    let d = Dir::new();
    d.write("conflict.tsv", &format!("{A}\t{B}\n{A}\t{UNUSED}\n"));
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("conflict.tsv"),
        "--dry-run",
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("two different URLs"), "{}", err(&o));

    d.write("same.tsv", &format!("{A}\t{B}\n{A}\t{B}\n"));
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("same.tsv"),
        "--dry-run",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(text(&o).contains("1 repeated lines merged"), "{}", text(&o));
}

#[test]
fn normalize_steam_hosts() {
    let d = Dir::new();
    d.write(
        "steam.json",
        "{\r\n  \"U\": \"http://cloud-3.steamusercontent.com/ugc/42/ABCD/\"\r\n}",
    );
    let o = run(&[
        &d.path("steam.json"),
        "--normalize-steam-hosts",
        "--out",
        &d.path("n.json"),
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        d.read("n.json"),
        "{\r\n  \"U\": \"https://steamusercontent-a.akamaihd.net/ugc/42/ABCD/\"\r\n}"
    );
}

#[test]
fn nothing_to_do_is_an_error() {
    let d = Dir::new();
    let o = run(&[&d.path("save.json"), "--out", &d.path("n.json")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("nothing to do"), "{}", err(&o));
}

#[test]
fn manifest_supplies_new_urls_by_sha1() {
    let d = Dir::new();
    let old = format!("https://steamusercontent-a.akamaihd.net/ugc/1/{SHA}/");
    let new = format!("https://steamusercontent-a.akamaihd.net/ugc/2/{SHA}/");
    d.write(
        "steam.json",
        &format!("{{\r\n  \"U\": \"{old}\",\r\n  \"V\": \"{A}\"\r\n}}"),
    );
    d.write(
        "m.json",
        &format!(
            "{{\"version\":1,\"entries\":[{{\"cloud_path\":\"G/a.png\",\"sha1\":\"{SHA}\",\"size\":3,\"url\":\"{new}\"}}]}}"
        ),
    );
    let o = run(&[
        &d.path("steam.json"),
        "--from-manifest",
        &d.path("m.json"),
        "--out",
        &d.path("n.json"),
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        d.read("n.json"),
        format!("{{\r\n  \"U\": \"{new}\",\r\n  \"V\": \"{A}\"\r\n}}")
    );
    let o = run(&[
        &d.path("steam.json"),
        "--from-manifest",
        &d.path("missing.json"),
        "--dry-run",
    ]);
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn json_output() {
    let d = Dir::new();
    let o = run(&[
        &d.path("save.json"),
        "--map",
        &d.path("map.tsv"),
        "--dry-run",
        "--json",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(&text(&o)).unwrap();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["written"], false);
    assert_eq!(v["values_changed"], 1);
    assert_eq!(v["scripts_skipped"], 1);
    assert_eq!(v["unmatched"], serde_json::json!(["line 3"]));
    assert_eq!(v["rules"][0]["value_uses"][0]["guid"], "aaaaaa");
}
