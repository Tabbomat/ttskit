//! Tests for mapping known URLs to `asset:{PATH}` placeholders on unpack.

use ttskit_core::save::write::to_vec_with;
use ttskit_core::{AssetMap, Eol, PackOptions, Unpacked, pack, unpack, unpack_with_assets};

fn map(entries: &[(&str, &str)]) -> AssetMap {
    let mut map = AssetMap::new();
    for (path, url) in entries {
        map.insert(*path, *url, false);
    }
    map
}

/// A save in the exact format TTS writes.
fn save_bytes(value: serde_json::Value) -> Vec<u8> {
    to_vec_with(&value, Eol::Lf, false)
}

fn text(unpacked: &Unpacked, path: &str) -> String {
    let file = unpacked
        .files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no file {path}"));
    String::from_utf8(file.data.clone()).unwrap()
}

fn sample() -> Vec<u8> {
    save_bytes(serde_json::json!({
        "SaveName": "t",
        "Note": "see http://h/a.png",
        "LuaScript": "local u = \"http://h/a.png\"\nlocal v = \"http://h/b.png\"\n",
        "XmlUI": "<Image image=\"http://h/a.png\"/>\n",
        "LuaScriptState": "{\"img\":\"http://h/b.png\"}",
        "TabStates": {"0": {"title": "n", "body": "read http://h/a.png"}},
        "ObjectStates": [{
            "GUID": "aaaaaa",
            "Name": "Card",
            "Nickname": "c",
            "CustomImage": {"ImageURL": "http://h/a.png", "ImageSecondaryURL": "http://h/other.png"},
            "LuaScript": "x = \"http://h/b.png\""
        }]
    }))
}

fn sample_map() -> AssetMap {
    map(&[("a.png", "http://h/a.png"), ("sub/b.png", "http://h/b.png")])
}

#[test]
fn maps_json_lua_xml_state_and_notebook() {
    let unpacked = unpack_with_assets(&sample(), Eol::Lf, &sample_map()).unwrap();
    let save = text(&unpacked, "save.json");
    assert!(save.contains("see asset:{a.png}"), "{save}");
    assert!(!save.contains("http://h/a.png"), "{save}");
    assert!(
        text(&unpacked, "script.lua").contains("\"asset:{a.png}\""),
        "lua"
    );
    assert!(text(&unpacked, "script.lua").contains("\"asset:{sub/b.png}\""));
    assert!(text(&unpacked, "ui.xml").contains("image=\"asset:{a.png}\""));
    assert!(text(&unpacked, "state.json").contains("\"img\": \"asset:{sub/b.png}\""));
    let note = unpacked
        .files
        .iter()
        .find(|f| f.path.starts_with("notebook/"))
        .unwrap();
    assert!(String::from_utf8_lossy(&note.data).contains("read asset:{a.png}"));
    let object = unpacked
        .files
        .iter()
        .find(|f| f.path.ends_with("object.json"))
        .unwrap();
    let object = String::from_utf8_lossy(&object.data);
    assert!(
        object.contains("\"ImageURL\": \"asset:{a.png}\""),
        "{object}"
    );
    assert!(object.contains("http://h/other.png"), "{object}");
    // Note, Lua x2 (root), Lua (object), XML, state, notebook, ImageURL.
    assert_eq!(unpacked.report.assets_mapped, 8);
}

#[test]
fn round_trip_gives_the_original_bytes() {
    let input = sample();
    let assets = sample_map();
    let unpacked = unpack_with_assets(&input, Eol::Lf, &assets).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tree");
    unpacked.write_to_dir(&dir).unwrap();
    let opts = PackOptions {
        assets,
        ..PackOptions::new(true)
    };
    let packed = pack(&dir, &opts).unwrap();
    assert!(packed.bytes == input, "round trip differs");
    assert_eq!(packed.assets.len(), 2);
}

#[test]
fn empty_map_matches_plain_unpack() {
    let plain = unpack(&sample(), Eol::Lf).unwrap();
    let mapped = unpack_with_assets(&sample(), Eol::Lf, &AssetMap::new()).unwrap();
    assert_eq!(plain, mapped);
    assert_eq!(plain.report.assets_mapped, 0);
}

#[test]
fn unmatched_urls_stay() {
    let assets = map(&[("z.png", "http://h/zzz.png")]);
    let mapped = unpack_with_assets(&sample(), Eol::Lf, &assets).unwrap();
    assert_eq!(mapped, unpack(&sample(), Eol::Lf).unwrap());
}

#[test]
fn scheme_spelling_must_match() {
    let assets = map(&[("a.png", "https://h/a.png"), ("c.png", "HTTP://H/A.PNG")]);
    let mapped = unpack_with_assets(&sample(), Eol::Lf, &assets).unwrap();
    assert_eq!(mapped.report.assets_mapped, 0);
    assert!(text(&mapped, "save.json").contains("http://h/a.png"));
}

#[test]
fn keys_are_not_rewritten() {
    let input = save_bytes(serde_json::json!({
        "http://h/a.png": "http://h/a.png",
        "Other": {"http://h/a.png": ["http://h/a.png"]}
    }));
    let mapped = unpack_with_assets(&input, Eol::Lf, &map(&[("a.png", "http://h/a.png")])).unwrap();
    let save = text(&mapped, "save.json");
    assert!(
        save.contains("\"http://h/a.png\": \"asset:{a.png}\""),
        "{save}"
    );
    assert!(save.contains("\"http://h/a.png\": ["), "{save}");
    assert_eq!(mapped.report.assets_mapped, 2);
}

#[test]
fn prefix_url_does_not_break_the_longer_one() {
    let input = save_bytes(serde_json::json!({
        "A": "http://h/a",
        "B": "http://h/a.png",
        "C": "http://h/a.png and http://h/a"
    }));
    let assets = map(&[("short", "http://h/a"), ("long", "http://h/a.png")]);
    let mapped = unpack_with_assets(&input, Eol::Lf, &assets).unwrap();
    let save = text(&mapped, "save.json");
    assert!(save.contains("\"A\": \"asset:{short}\""), "{save}");
    assert!(save.contains("\"B\": \"asset:{long}\""), "{save}");
    assert!(
        save.contains("\"C\": \"asset:{long} and asset:{short}\""),
        "{save}"
    );
    assert_eq!(mapped.report.assets_mapped, 4);
}

#[test]
fn same_url_picks_the_smallest_path() {
    let input = save_bytes(serde_json::json!({"A": "http://h/a"}));
    let assets = map(&[
        ("z.png", "http://h/a"),
        ("b.png", "http://h/a"),
        ("m.png", "http://h/a"),
    ]);
    let mapped = unpack_with_assets(&input, Eol::Lf, &assets).unwrap();
    assert!(text(&mapped, "save.json").contains("asset:{b.png}"));
}

#[test]
fn empty_urls_are_ignored() {
    let assets = map(&[("e.png", ""), ("a.png", "http://h/a.png")]);
    let mapped = unpack_with_assets(&sample(), Eol::Lf, &assets).unwrap();
    assert!(!text(&mapped, "save.json").contains("asset:{e.png}"));
    assert_eq!(mapped.report.assets_mapped, 5);
}
