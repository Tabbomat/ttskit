//! Tests for `asset:{PATH}` placeholders on pack.

use std::path::{Path, PathBuf};

use serde_json::Value;
use ttskit_core::{AssetMap, PackError, PackOptions, Packed, pack, unpack};

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn tree(save_json: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tree");
    write(&dir, "save.json", save_json);
    (tmp, dir)
}

fn map(entries: &[(&str, &str, bool)]) -> AssetMap {
    let mut map = AssetMap::new();
    for (path, url, stale) in entries {
        map.insert(*path, *url, *stale);
    }
    map
}

fn options(assets: AssetMap) -> PackOptions {
    PackOptions {
        assets,
        ..PackOptions::new(false)
    }
}

fn value(packed: &Packed) -> Value {
    serde_json::from_slice(&packed.bytes).unwrap()
}

#[test]
fn resolves_json_string_values() {
    let (_tmp, dir) = tree(
        r#"{"Skybox": "asset:{Boards/a.png}", "List": ["x asset:{Boards/a.png}"], "N": 3,
            "ObjectStates": [{"CustomImage": {"ImageURL": "asset:{b.png}"}}]}"#,
    );
    let assets = map(&[
        ("Boards/a.png", "http://h/a", false),
        ("b.png", "http://h/b", false),
    ]);
    let packed = pack(&dir, &options(assets)).unwrap();
    let v = value(&packed);
    assert_eq!(v["Skybox"], "http://h/a");
    assert_eq!(v["List"][0], "x http://h/a");
    assert_eq!(
        v["ObjectStates"][0]["CustomImage"]["ImageURL"],
        "http://h/b"
    );
    assert!(packed.warnings.is_empty());
    assert_eq!(packed.assets.len(), 2);
    assert_eq!(packed.assets[0].path, "Boards/a.png");
    assert_eq!(packed.assets[0].url, "http://h/a");
    assert_eq!(packed.assets[0].count, 2);
    assert!(!packed.assets[0].stale);
    assert_eq!(packed.assets[1].path, "b.png");
    assert_eq!(packed.assets[1].count, 1);
}

#[test]
fn resolves_in_lua_xml_state_and_notebook() {
    let (_tmp, dir) = tree(
        r#"{"LuaScript": "file:script.lua", "XmlUI": "file:ui.xml", "LuaScriptState": "file:state.txt",
            "TabStates": {"0": {"title": "t", "body": "file:notebook/a.txt"}}}"#,
    );
    write(&dir, "script.lua", "local u = \"asset:{a.png}\"\n");
    write(&dir, "ui.xml", "<Image image=\"asset:{a.png}\"/>\n");
    write(&dir, "state.txt", "{\"img\":\"asset:{a.png}\"}");
    write(&dir, "notebook/a.txt", "see asset:{a.png}");
    let packed = pack(&dir, &options(map(&[("a.png", "http://h/a", false)]))).unwrap();
    let v = value(&packed);
    assert_eq!(v["LuaScript"], "local u = \"http://h/a\"\n");
    assert_eq!(v["XmlUI"], "<Image image=\"http://h/a\"/>\n");
    assert_eq!(v["LuaScriptState"], "{\"img\":\"http://h/a\"}");
    assert_eq!(v["TabStates"]["0"]["body"], "see http://h/a");
    assert_eq!(packed.assets[0].count, 4);
}

#[test]
fn resolves_in_required_module() {
    let (tmp, dir) = tree(r#"{"LuaScript": "file:script.lua"}"#);
    write(&dir, "script.lua", "local m = require(\"util\")\n");
    let lib = tmp.path().join("lib");
    write(&lib, "util.lua", "return \"asset:{m.png}\"");
    let mut opts = options(map(&[("m.png", "http://h/m", false)]));
    opts.includes = vec![lib];
    let packed = pack(&dir, &opts).unwrap();
    let script = value(&packed)["LuaScript"].as_str().unwrap().to_owned();
    assert!(script.contains("return \"http://h/m\""), "{script}");
    assert!(!script.contains("asset:{"), "{script}");
}

#[test]
fn several_placeholders_in_one_string() {
    let (_tmp, dir) = tree(r#"{"Note": "asset:{a} and asset:{b} and asset:{a}"}"#);
    let assets = map(&[("a", "A", false), ("b", "B", false)]);
    let packed = pack(&dir, &options(assets)).unwrap();
    assert_eq!(value(&packed)["Note"], "A and B and A");
}

#[test]
fn missing_entries_are_all_listed() {
    let (_tmp, dir) =
        tree(r#"{"A": "asset:{z.png}", "B": ["asset:{a.png} asset:{z.png}"], "C": "asset:{ok}"}"#);
    let err = pack(&dir, &options(map(&[("ok", "U", false)]))).unwrap_err();
    match &err {
        PackError::Assets { missing, stale } => {
            assert_eq!(missing, &["a.png", "z.png"]);
            assert!(stale.is_empty());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        err.to_string(),
        "asset placeholders cannot be resolved:\n  no entry for: a.png\n  no entry for: z.png"
    );
}

#[test]
fn empty_map_is_an_error() {
    let (_tmp, dir) = tree(r#"{"A": "asset:{a.png}"}"#);
    let err = pack(&dir, &PackOptions::new(false)).unwrap_err();
    assert!(matches!(err, PackError::Assets { .. }), "{err:?}");
}

#[test]
fn stale_entries_error_together_with_missing() {
    let (_tmp, dir) = tree(r#"{"A": "asset:{b.png} asset:{a.png} asset:{c.png} asset:{b.png}"}"#);
    let assets = map(&[("b.png", "B", true), ("a.png", "A", false)]);
    let err = pack(&dir, &options(assets)).unwrap_err();
    match &err {
        PackError::Assets { missing, stale } => {
            assert_eq!(missing, &["c.png"]);
            assert_eq!(stale, &["b.png"]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        err.to_string(),
        "asset placeholders cannot be resolved:\n  no entry for: c.png\n  changed since last push (stale): b.png"
    );
}

#[test]
fn allow_stale_resolves_with_warning() {
    let (_tmp, dir) = tree(r#"{"A": "asset:{Boards/b.png}", "B": "asset:{a}"}"#);
    let mut opts = options(map(&[
        ("Boards/b.png", "http://h/b", true),
        ("a", "A", false),
    ]));
    opts.allow_stale = true;
    let packed = pack(&dir, &opts).unwrap();
    assert_eq!(value(&packed)["A"], "http://h/b");
    assert_eq!(
        packed.warnings,
        ["asset Boards/b.png is stale: the local file differs from the manifest"]
    );
    let stale: Vec<_> = packed.assets.iter().map(|a| a.stale).collect();
    assert_eq!(stale, [true, false]);
}

#[test]
fn malformed_placeholders_are_left_alone() {
    for text in ["asset:{abc", "asset:{} | asset:{\nx}", "asset:{"] {
        let (_tmp, dir) = tree(&serde_json::json!({ "Note": text }).to_string());
        let packed = pack(&dir, &PackOptions::new(false)).unwrap();
        assert_eq!(value(&packed)["Note"], text);
        assert!(packed.assets.is_empty());
    }
}

#[test]
fn keys_are_not_rewritten() {
    let (_tmp, dir) = tree(r#"{"asset:{a}": "asset:{a}"}"#);
    let packed = pack(&dir, &options(map(&[("a", "U", false)]))).unwrap();
    let v = value(&packed);
    assert_eq!(v["asset:{a}"], "U");
}

#[test]
fn paths_are_case_sensitive() {
    let (_tmp, dir) = tree(r#"{"A": "asset:{A.png}"}"#);
    let err = pack(&dir, &options(map(&[("a.png", "U", false)]))).unwrap_err();
    assert!(matches!(err, PackError::Assets { .. }), "{err:?}");
}

#[test]
fn output_is_unchanged_without_placeholders() {
    for path in ttskit_testutil::fixture_saves() {
        let input = std::fs::read(&path).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tree");
        unpack(&input, ttskit_core::Eol::Lf)
            .unwrap()
            .write_to_dir(&dir)
            .unwrap();
        let plain = pack(&dir, &PackOptions::new(false)).unwrap();
        let with_map = pack(&dir, &options(map(&[("unused.png", "U", true)]))).unwrap();
        assert_eq!(plain.bytes, with_map.bytes, "{}", path.display());
        assert!(plain.assets.is_empty());
        assert!(with_map.assets.is_empty());
    }
}
