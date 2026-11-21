//! Unpack tests over the shared fixtures: a test-only reassembler rebuilds the
//! save from the in-memory tree, which shows that every field ends up in
//! exactly one file and that key order and number text survive.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};
use ttskit_core::unpack::{compact_state, eol};
use ttskit_core::{Eol, Save, Unpacked, WriteOptions, unpack};

const EOLS: [Eol; 2] = [Eol::Crlf, Eol::Lf];

struct Tree {
    files: HashMap<String, Vec<u8>>,
    eol: Map<String, Value>,
    pretty: HashSet<String>,
}

impl Tree {
    fn new(unpacked: &Unpacked) -> Tree {
        let files: HashMap<String, Vec<u8>> = unpacked
            .files
            .iter()
            .map(|f| (f.path.clone(), f.data.clone()))
            .collect();
        assert_eq!(files.len(), unpacked.files.len(), "duplicate paths");
        let meta: Value = serde_json::from_slice(&files[".ttskit/unpack.json"]).unwrap();
        let eol = meta["eol"].as_object().unwrap().clone();
        let pretty = meta["pretty_state"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        Tree { files, eol, pretty }
    }

    fn json(&self, path: &str) -> Value {
        let data = self
            .files
            .get(path)
            .unwrap_or_else(|| panic!("missing {path}"));
        serde_json::from_slice(data).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn text(&self, path: &str) -> String {
        let data = self
            .files
            .get(path)
            .unwrap_or_else(|| panic!("missing {path}"));
        if self.pretty.contains(path) {
            return compact_state(&serde_json::from_slice(data).unwrap());
        }
        let text = String::from_utf8(data.clone()).unwrap();
        match self.eol.get(path).and_then(Value::as_str) {
            None => text,
            Some(style) => {
                let to = if style == "crlf" { Eol::Crlf } else { Eol::Lf };
                String::from_utf8(eol::convert(&text, to)).unwrap()
            }
        }
    }

    fn object(&self, dir: &str, mut value: Value) -> Value {
        let map = value.as_object_mut().unwrap();
        self.resolve(dir, map, false);
        value
    }

    fn children(&self, base: &str, index: &Value) -> Vec<Value> {
        index
            .as_array()
            .unwrap()
            .iter()
            .map(|name| {
                let name = name.as_str().unwrap();
                let dir = format!("{base}{name}/");
                self.object(&dir, self.json(&format!("{dir}object.json")))
            })
            .collect()
    }

    fn resolve(&self, dir: &str, map: &mut Map<String, Value>, root: bool) {
        let is = |map: &Map<String, Value>, key: &str, marker: &str| {
            map.get(key).and_then(Value::as_str) == Some(marker)
        };
        for (key, file) in [("LuaScript", "script.lua"), ("XmlUI", "ui.xml")] {
            if is(map, key, &format!("file:{file}")) {
                let text = self.text(&format!("{dir}{file}"));
                *map.get_mut(key).unwrap() = Value::String(text);
            }
        }
        for file in ["state.json", "state.txt"] {
            if is(map, "LuaScriptState", &format!("file:{file}")) {
                let text = self.text(&format!("{dir}{file}"));
                *map.get_mut("LuaScriptState").unwrap() = Value::String(text);
            }
        }
        let mut dirs = vec![
            ("ContainedObjects", "contents"),
            ("ChildObjects", "children"),
        ];
        if root {
            dirs.push(("ObjectStates", "objects"));
        }
        for (key, sub) in dirs {
            if is(map, key, &format!("dir:{sub}")) {
                let base = format!("{dir}{sub}/");
                let list = self.children(&base, &self.json(&format!("{base}index.json")));
                *map.get_mut(key).unwrap() = Value::Array(list);
            }
        }
        if is(map, "States", "dir:states") {
            let base = format!("{dir}states/");
            let index = self.json(&format!("{base}index.json"));
            let mut states = Map::new();
            for entry in index.as_array().unwrap() {
                let (key, name) = (
                    entry["key"].as_str().unwrap(),
                    entry["dir"].as_str().unwrap(),
                );
                let child = format!("{base}{name}/");
                states.insert(
                    key.to_owned(),
                    self.object(&child, self.json(&format!("{child}object.json"))),
                );
            }
            *map.get_mut("States").unwrap() = Value::Object(states);
        }
        if root && let Some(Value::Object(tabs)) = map.get_mut("TabStates") {
            for entry in tabs.values_mut() {
                let Some(entry) = entry.as_object_mut() else {
                    continue;
                };
                let marker = entry.get("body").and_then(Value::as_str);
                if let Some(path) = marker.and_then(|m| m.strip_prefix("file:notebook/")) {
                    let text = self.text(&format!("notebook/{path}"));
                    *entry.get_mut("body").unwrap() = Value::String(text);
                }
            }
        }
    }

    fn reassemble(&self) -> Value {
        let mut root = self.json("save.json");
        self.resolve("", root.as_object_mut().unwrap(), true);
        root
    }
}

/// Structural comparison including key order. Strings that differ only in line
/// breaks are counted in `eol_diffs` (they come from mixed-ending fields).
fn compare(a: &Value, b: &Value, eol_diffs: &mut usize) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && compare(v1, v2, eol_diffs))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| compare(p, q, eol_diffs))
        }
        (Value::String(x), Value::String(y)) if x != y => {
            let norm = |s: &str| s.replace("\r\n", "\n");
            *eol_diffs += 1;
            norm(x) == norm(y)
        }
        _ => a == b,
    }
}

fn unpack_fixture(name: &str, eol: Eol) -> Unpacked {
    unpack(&ttskit_testutil::read_fixture(name), eol).unwrap()
}

#[test]
fn every_fixture_reassembles_to_the_original() {
    for path in ttskit_testutil::fixture_saves() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let input = std::fs::read(&path).unwrap();
        for eol in EOLS {
            let unpacked = unpack(&input, eol).unwrap();
            let rebuilt = Tree::new(&unpacked).reassemble();
            let save = Save::from_value(rebuilt.clone()).unwrap();
            if unpacked.report.mixed_eol.is_empty() {
                let output = save.to_vec();
                assert!(
                    output == input,
                    "{name} ({eol:?}): {}",
                    ttskit_testutil::first_diff(&input, &output)
                );
            } else {
                let original = Save::from_slice(&input).unwrap();
                let mut diffs = 0;
                assert!(
                    compare(original.value(), &rebuilt, &mut diffs),
                    "{name} ({eol:?}): reassembled save differs"
                );
                assert_eq!(diffs, unpacked.report.mixed_eol.len(), "{name} ({eol:?})");
            }
        }
    }
}

#[test]
fn every_file_uses_the_requested_line_ending() {
    for path in ttskit_testutil::fixture_saves() {
        let input = std::fs::read(&path).unwrap();
        let lf = unpack(&input, Eol::Lf).unwrap();
        for file in &lf.files {
            assert!(
                !file.data.contains(&b'\r') || is_lone_cr_only(&file.data),
                "{}",
                file.path
            );
        }
        let crlf = unpack(&input, Eol::Crlf).unwrap();
        for file in &crlf.files {
            for (i, _) in file.data.iter().enumerate().filter(|(_, b)| **b == b'\n') {
                assert!(i > 0 && file.data[i - 1] == b'\r', "{}", file.path);
            }
        }
        // Paths and report do not depend on the line ending.
        let paths = |u: &Unpacked| u.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&lf), paths(&crlf));
        assert_eq!(lf.report, crlf.report);
    }
}

/// Every `\r` is not followed by `\n` (only lone CRs remain in LF output).
fn is_lone_cr_only(data: &[u8]) -> bool {
    data.windows(2).all(|w| w != b"\r\n")
}

fn paths(u: &Unpacked) -> Vec<&str> {
    u.files.iter().map(|f| f.path.as_str()).collect()
}

fn file<'a>(u: &'a Unpacked, path: &str) -> &'a [u8] {
    &u.files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("missing {path} in {:?}", paths(u)))
        .data
}

fn json_file(u: &Unpacked, path: &str) -> Value {
    serde_json::from_slice(file(u, path)).unwrap()
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

#[test]
fn containers_tree() {
    let u = unpack_fixture("containers.json", Eol::Lf);
    let p = paths(&u);
    for expected in [
        "save.json",
        "objects/index.json",
        "objects/Outer bag.bag001/object.json",
        "objects/Outer bag.bag001/contents/index.json",
        "objects/Outer bag.bag001/contents/Middle bag.bag002/object.json",
        "objects/Outer bag.bag001/contents/Middle bag.bag002/contents/Innermost bag.bag003/contents/Leaf token.leaf01/object.json",
        "objects/Top level deck.deck01/contents/Card 100.c01000/object.json",
        "objects/Top level deck.deck01/contents/Card 102.c01002/object.json",
        ".ttskit/unpack.json",
    ] {
        assert!(p.contains(&expected), "missing {expected}");
    }
    assert_eq!(p.last(), Some(&".ttskit/unpack.json"));
    assert_eq!(
        json_file(&u, "objects/index.json"),
        serde_json::json!([
            "Outer bag.bag001",
            "Top level deck.deck01",
            "Deck bag.bag004"
        ])
    );
    assert_eq!(
        json_file(&u, "objects/Top level deck.deck01/contents/index.json"),
        serde_json::json!(["Card 100.c01000", "Card 101.c01001", "Card 102.c01002"])
    );
    let save = json_file(&u, "save.json");
    assert_eq!(save["ObjectStates"], "dir:objects");
    // The leaf has no extracted fields, so its marker-free file is plain.
    let leaf = json_file(
        &u,
        "objects/Outer bag.bag001/contents/Middle bag.bag002/contents/Innermost bag.bag003/contents/Leaf token.leaf01/object.json",
    );
    assert_eq!(leaf["GUID"], "leaf01");
    assert!(u.report.objects >= 10);
    assert_eq!(u.report.files, u.files.len());
}

#[test]
fn states_tree_and_index() {
    let u = unpack_fixture("states.json", Eol::Crlf);
    for expected in [
        "objects/State one.sta001/object.json",
        "objects/State one.sta001/script.lua",
        "objects/State one.sta001/state.json",
        "objects/State one.sta001/states/index.json",
        "objects/State one.sta001/states/2.State two.sta002/script.lua",
        "objects/State one.sta001/states/2.State two.sta002/state.json",
        "objects/State one.sta001/states/3.State three.sta003/script.lua",
    ] {
        assert!(paths(&u).contains(&expected), "missing {expected}");
    }
    assert!(
        !paths(&u).contains(&"objects/State one.sta001/states/3.State three.sta003/state.json")
    );
    assert_eq!(
        json_file(&u, "objects/State one.sta001/states/index.json"),
        serde_json::json!([
            {"key": "2", "dir": "2.State two.sta002"},
            {"key": "3", "dir": "3.State three.sta003"}
        ])
    );
    let one = json_file(&u, "objects/State one.sta001/object.json");
    assert_eq!(one["LuaScript"], "file:script.lua");
    assert_eq!(one["LuaScriptState"], "file:state.json");
    assert_eq!(one["XmlUI"], "");
    assert_eq!(one["States"], "dir:states");
    let three = json_file(
        &u,
        "objects/State one.sta001/states/3.State three.sta003/object.json",
    );
    // An empty LuaScriptState stays inline.
    assert_eq!(three["LuaScriptState"], "");
    // Pretty state in the requested layout, with a trailing newline.
    assert_eq!(
        file(&u, "objects/State one.sta001/state.json"),
        b"{\r\n  \"s\": 1\r\n}\r\n"
    );
    let meta = json_file(&u, ".ttskit/unpack.json");
    assert!(
        meta["pretty_state"]
            .as_array()
            .unwrap()
            .contains(&Value::from("objects/State one.sta001/state.json"))
    );
}

#[test]
fn markers_keep_key_positions() {
    let input = ttskit_testutil::read_fixture("scripts_ui.json");
    let original = Save::from_slice(&input).unwrap();
    let u = unpack(&input, Eol::Lf).unwrap();
    let save = json_file(&u, "save.json");
    assert_eq!(keys(&save), keys(original.value()));
    assert_eq!(save["LuaScript"], "file:script.lua");
    assert_eq!(save["XmlUI"], "file:ui.xml");
    assert_eq!(save["LuaScriptState"], "file:state.json");
    let first = original.value()["ObjectStates"][0].clone();
    let dir = "objects/".to_owned() + &first_dir(&u);
    let object = json_file(&u, &format!("{dir}/object.json"));
    assert_eq!(keys(&object), keys(&first));
}

fn first_dir(u: &Unpacked) -> String {
    json_file(u, "objects/index.json")[0]
        .as_str()
        .unwrap()
        .to_owned()
}

const EDGE: &str = "layout_edge_cases.json";

#[test]
fn edge_cases_directory_names() {
    let u = unpack_fixture(EDGE, Eol::Lf);
    let index = json_file(&u, "objects/index.json");
    let names: Vec<&str> = index
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Board.f00d01",
            "CRLF script.crl001",
            "Mixed script.mix001",
            "Tie script.tie001",
            "Lone CR.lcr001",
            "bad_________name.inv001",
            "A very long nickname tha.long01",
            "con_.res001",
            "Card.c00001",
            "Card.c00001-2",
            "CARD.C00001-3",
            "Custom_Token.nam001",
            "Custom_Token.nam002",
            "object",
            "object-2",
            "Ends with dots",
            "Empty bag.emp001",
            "Mixed contents.mixc01",
            "Tüte 日本語 🎲.uni001",
            "Outer bag.bag001",
        ]
    );
    let p = paths(&u);
    for name in &names {
        assert!(
            p.contains(&format!("objects/{name}/object.json").as_str()),
            "{name}"
        );
    }
    // Children and contents.
    assert_eq!(
        json_file(&u, "objects/Board.f00d01/children/index.json"),
        serde_json::json!(["Lamp shade.lamp01", "object"])
    );
    assert!(p.contains(&"objects/Board.f00d01/children/Lamp shade.lamp01/script.lua"));
    assert_eq!(
        json_file(&u, "objects/Outer bag.bag001/contents/index.json"),
        serde_json::json!(["Inner.in0002"])
    );
    assert_eq!(u.report.objects, 23);
    // Notebook pages.
    for page in [
        "notebook/0.Rules.txt",
        "notebook/1.txt",
        "notebook/3.LF_ page_.txt",
    ] {
        assert!(p.contains(&page), "{page}");
    }
    assert!(!p.iter().any(|x| x.starts_with("notebook/2")));
}

#[test]
fn edge_cases_inline_values_stay() {
    let u = unpack_fixture(EDGE, Eol::Lf);
    let save = json_file(&u, "save.json");
    assert_eq!(save["TabStates"]["0"]["body"], "file:notebook/0.Rules.txt");
    assert_eq!(save["TabStates"]["0"]["title"], "Rules");
    assert_eq!(save["TabStates"]["1"]["body"], "file:notebook/1.txt");
    assert_eq!(save["TabStates"]["2"]["body"], "");
    assert_eq!(save["LuaScriptState"], "");
    assert_eq!(
        keys(&save["TabStates"]["0"]),
        ["title", "body", "color", "visibleColor", "id"]
    );

    let empty = json_file(&u, "objects/Empty bag.emp001/object.json");
    assert_eq!(empty["ContainedObjects"], serde_json::json!([]));
    assert_eq!(empty["LuaScript"], "");

    // A non-object entry keeps the whole array inline.
    let mixed = json_file(&u, "objects/Mixed contents.mixc01/object.json");
    let inline = mixed["ContainedObjects"].as_array().unwrap();
    assert_eq!(inline.len(), 2);
    assert_eq!(inline[0]["GUID"], "in0001");
    assert_eq!(inline[1], 5);
    assert!(
        !paths(&u)
            .iter()
            .any(|p| p.contains("Mixed contents.mixc01/contents"))
    );

    // A numeric GUID is not a name.
    let numeric = json_file(&u, "objects/object-2/object.json");
    assert_eq!(numeric["GUID"], 5);
}

#[test]
fn edge_cases_numbers_survive_in_object_json() {
    let u = unpack_fixture(EDGE, Eol::Crlf);
    let text = String::from_utf8(file(&u, "objects/Board.f00d01/object.json").to_vec()).unwrap();
    for n in [
        "\"posX\": 1E-05,",
        "\"posY\": -0.0,",
        "\"posZ\": 12345678901234567890123,",
        "\"scaleY\": 1E+20,",
        "\"scaleZ\": -7",
    ] {
        assert!(text.contains(n), "missing {n}");
    }
}

#[test]
fn edge_cases_state_files() {
    let u = unpack_fixture(EDGE, Eol::Lf);
    let board = "objects/Board.f00d01/";
    assert_eq!(
        String::from_utf8(file(&u, &format!("{board}state.json")).to_vec()).unwrap(),
        "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ],\n  \"c\": {\n    \"d\": null,\n    \"e\": \"x\\ny\"\n  }\n}\n"
    );
    // Valid JSON, not compact: verbatim, in state.json.
    assert_eq!(
        file(&u, "objects/Lone CR.lcr001/state.json"),
        br#"{"a": 1}"#
    );
    // Not JSON: state.txt with converted line breaks.
    assert_eq!(
        file(&u, "objects/bad_________name.inv001/state.txt"),
        b"plain text\nstate"
    );
    let meta = json_file(&u, ".ttskit/unpack.json");
    assert_eq!(
        meta["pretty_state"],
        serde_json::json!([format!("{board}state.json")])
    );
}

#[test]
fn edge_cases_text_and_eol_metadata() {
    let u = unpack_fixture(EDGE, Eol::Lf);
    // A lone CR is kept, CRLF becomes LF.
    assert_eq!(file(&u, "objects/Lone CR.lcr001/script.lua"), b"a\rb\nc\r");
    // Trailing line breaks are not added or removed.
    assert_eq!(file(&u, "objects/Board.f00d01/ui.xml"), b"<Panel/>\n");
    assert_eq!(file(&u, "script.lua"), b"-- global\nprint(\"hi\")");
    assert_eq!(file(&u, "notebook/0.Rules.txt"), b"Line one\nLine two\n");

    let crlf = unpack_fixture(EDGE, Eol::Crlf);
    assert_eq!(
        file(&crlf, "objects/Board.f00d01/script.lua"),
        b"-- lf only\r\nfunction onLoad()\r\n  print(1)\r\nend\r\n"
    );
    assert_eq!(
        file(&crlf, "objects/Lone CR.lcr001/script.lua"),
        b"a\rb\r\nc\r"
    );

    let meta = json_file(&u, ".ttskit/unpack.json");
    assert_eq!(meta["format"], 1);
    assert_eq!(meta["ttskit"], env!("CARGO_PKG_VERSION"));
    assert_eq!(meta["layout_eol"], "crlf");
    assert_eq!(
        keys(&meta),
        [
            "format",
            "ttskit",
            "layout_eol",
            "files",
            "eol",
            "mixed_eol",
            "pretty_state"
        ]
    );
    let eol = &meta["eol"];
    assert_eq!(eol["script.lua"], "crlf");
    assert_eq!(eol["objects/Board.f00d01/script.lua"], "lf");
    assert_eq!(eol["objects/Board.f00d01/ui.xml"], "crlf");
    assert_eq!(eol["objects/Lone CR.lcr001/script.lua"], "crlf");
    assert_eq!(eol["objects/Mixed script.mix001/script.lua"], "crlf");
    assert_eq!(eol["objects/Tie script.tie001/script.lua"], "lf");
    assert_eq!(eol["objects/bad_________name.inv001/state.txt"], "crlf");
    assert_eq!(eol["notebook/0.Rules.txt"], "crlf");
    assert_eq!(eol["notebook/3.LF_ page_.txt"], "lf");
    // No line break, no entry.
    assert!(eol.get("notebook/1.txt").is_none());
    assert!(eol.get("objects/Board.f00d01/state.json").is_none());
    assert_eq!(
        meta["mixed_eol"],
        serde_json::json!([
            "objects/Mixed script.mix001/script.lua",
            "objects/Tie script.tie001/script.lua"
        ])
    );
    assert_eq!(u.report.mixed_eol.len(), 2);

    // `files` lists everything except the metadata, in write order.
    let listed: Vec<&str> = meta["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let written: Vec<&str> = paths(&u);
    assert_eq!(listed, written[..written.len() - 1]);
    assert_eq!(listed[0], "save.json");
    assert_eq!(listed[1], "script.lua");
}

#[test]
fn layout_eol_follows_the_input() {
    let lf_save = b"{\n  \"SaveName\": \"x\"\n}";
    let u = unpack(lf_save, Eol::Lf).unwrap();
    assert_eq!(json_file(&u, ".ttskit/unpack.json")["layout_eol"], "lf");
    let compact = unpack(b"{\"SaveName\":\"x\"}", Eol::Lf).unwrap();
    let meta = json_file(&compact, ".ttskit/unpack.json");
    assert!(meta.get("layout_eol").is_none());
    assert_eq!(meta["files"], serde_json::json!(["save.json"]));
    assert_eq!(meta["eol"], serde_json::json!({}));
    assert_eq!(meta["mixed_eol"], serde_json::json!([]));
    assert_eq!(meta["pretty_state"], serde_json::json!([]));
    assert_eq!(compact.report.objects, 0);
    assert_eq!(compact.report.files, 2);
    // BOM accepted.
    assert!(unpack(b"\xEF\xBB\xBF{}", Eol::Lf).is_ok());
    assert!(unpack(b"[1]", Eol::Lf).is_err());
}

#[test]
fn objects_marker_requires_all_objects() {
    let u = unpack(br#"{"ObjectStates":[{"GUID":"a"},3]}"#, Eol::Lf).unwrap();
    assert_eq!(u.report.objects, 0);
    assert_eq!(paths(&u), ["save.json", ".ttskit/unpack.json"]);
    // TabStates outside the notebook shape stay untouched.
    let u = unpack(
        br#"{"TabStates":[{"body":"x"}],"Note":"file:script.lua"}"#,
        Eol::Lf,
    )
    .unwrap();
    assert_eq!(paths(&u), ["save.json", ".ttskit/unpack.json"]);
}

#[test]
fn write_to_dir_creates_and_fills_missing_and_empty_dirs() {
    let u = unpack_fixture("containers.json", Eol::Lf);
    let tmp = tempfile::tempdir().unwrap();

    let missing = tmp.path().join("a").join("out");
    u.write_to_dir(&missing).unwrap();
    for f in &u.files {
        let mut path = missing.clone();
        path.extend(f.path.split('/'));
        assert_eq!(std::fs::read(&path).unwrap(), f.data, "{}", f.path);
    }

    let empty = tmp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    u.write_to_dir(&empty).unwrap();
    assert!(empty.join("save.json").is_file());
    assert!(empty.join(".ttskit").join("unpack.json").is_file());
}

#[test]
fn write_to_dir_refuses_a_non_empty_dir_and_writes_nothing() {
    let u = unpack_fixture("containers.json", Eol::Lf);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("keep.txt"), "mine").unwrap();

    let err = u.write_to_dir(&out).unwrap_err();
    assert!(err.to_string().contains("not empty"), "{err}");
    let entries: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(std::fs::read(out.join("keep.txt")).unwrap(), b"mine");

    // A file in place of the directory is refused too.
    let file_path = tmp.path().join("file");
    std::fs::write(&file_path, "x").unwrap();
    assert!(u.write_to_dir(&file_path).is_err());
    assert_eq!(std::fs::read(&file_path).unwrap(), b"x");
}

fn write_tree(u: &Unpacked, dir: &std::path::Path) {
    u.write_to_dir(dir).unwrap();
}

#[test]
fn plan_with_force_overwrites_only_what_unpack_creates() {
    let first = unpack_fixture("containers.json", Eol::Lf);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    write_tree(&first, &out);
    std::fs::write(out.join("keep.txt"), "mine").unwrap();
    std::fs::write(out.join("save.json"), "edited").unwrap();

    let opts = WriteOptions {
        force: true,
        clean: false,
    };
    let plan = first.plan(&out, opts).unwrap();
    assert_eq!(plan.change, ["save.json"]);
    assert!(plan.create.is_empty() && plan.remove.is_empty());
    assert_eq!(plan.unchanged.len(), first.files.len() - 1);
    // Planning writes nothing.
    assert_eq!(std::fs::read(out.join("save.json")).unwrap(), b"edited");

    first.apply(&out, &plan).unwrap();
    let save = first.files.iter().find(|f| f.path == "save.json").unwrap();
    assert_eq!(std::fs::read(out.join("save.json")).unwrap(), save.data);
    assert_eq!(std::fs::read(out.join("keep.txt")).unwrap(), b"mine");
}

#[test]
fn plan_refuses_a_file_where_a_directory_is_needed() {
    let u = unpack_fixture("containers.json", Eol::Lf);
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("save.json")).unwrap();
    let opts = WriteOptions {
        force: true,
        clean: false,
    };
    assert!(u.plan(tmp.path(), opts).is_err());
}

#[test]
fn clean_removes_only_stale_files_of_the_previous_unpack() {
    let old = unpack_fixture("containers.json", Eol::Lf);
    let new = unpack_fixture("minimal_save.json", Eol::Lf);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    write_tree(&old, &out);
    std::fs::write(out.join("keep.txt"), "mine").unwrap();
    // An unknown file inside a directory of the old tree keeps that directory.
    let stale_dirs: Vec<&str> = old
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| p.starts_with("objects/") && p.ends_with("/object.json"))
        .collect();
    let guarded = stale_dirs[0];
    let guarded_dir = out.join(guarded.rsplit_once('/').unwrap().0);
    std::fs::write(guarded_dir.join("notes.txt"), "mine too").unwrap();

    let opts = WriteOptions {
        force: true,
        clean: true,
    };
    let plan = new.plan(&out, opts).unwrap();
    let new_paths: HashSet<&str> = new.files.iter().map(|f| f.path.as_str()).collect();
    let expected: Vec<&str> = old
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| !new_paths.contains(p))
        .collect();
    assert!(!expected.is_empty());
    let mut got: Vec<&str> = plan.remove.iter().map(String::as_str).collect();
    got.sort_unstable();
    let mut want = expected.clone();
    want.sort_unstable();
    assert_eq!(got, want);
    assert!(!plan.remove.iter().any(|p| p == "keep.txt"));

    new.apply(&out, &plan).unwrap();
    assert_eq!(std::fs::read(out.join("keep.txt")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read(guarded_dir.join("notes.txt")).unwrap(),
        b"mine too"
    );
    assert!(!guarded_dir.join("object.json").exists());
    // A directory that became empty is gone, the root stays.
    let gone = old
        .files
        .iter()
        .map(|f| f.path.as_str())
        .find(|p| p.starts_with("objects/") && !new_paths.contains(p) && *p != guarded)
        .and_then(|p| p.rsplit_once('/'))
        .map(|(d, _)| out.join(d));
    if let Some(dir) = gone.filter(|d| d != &guarded_dir) {
        let leftover = std::fs::read_dir(&dir).map(|mut e| e.next().is_some());
        assert!(!matches!(leftover, Ok(false)), "{} is empty", dir.display());
    }
    assert!(out.is_dir());
}

#[test]
fn clean_without_metadata_removes_nothing_and_bad_metadata_is_refused() {
    let u = unpack_fixture("minimal_save.json", Eol::Lf);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("keep.txt"), "mine").unwrap();
    let opts = WriteOptions {
        force: true,
        clean: true,
    };
    assert!(u.plan(&out, opts).unwrap().remove.is_empty());

    let victim = tmp.path().join("victim.txt");
    std::fs::write(&victim, "outside").unwrap();
    std::fs::create_dir(out.join(".ttskit")).unwrap();
    for bad in [
        r#"{"files": ["../victim.txt"]}"#,
        r#"{"files": ["/etc/passwd"]}"#,
        r#"{"files": ["a\\b"]}"#,
        r#"{"files": [1]}"#,
        r#"{"nofiles": []}"#,
        "not json",
    ] {
        std::fs::write(out.join(".ttskit").join("unpack.json"), bad).unwrap();
        let err = u.plan(&out, opts).unwrap_err();
        assert!(err.to_string().contains("cannot use"), "{bad}: {err}");
    }
    assert!(victim.is_file());
}
