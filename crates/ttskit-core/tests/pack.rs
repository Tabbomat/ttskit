//! Pack tests: unpack the shared fixtures into a temporary directory, edit the
//! tree and pack it again.

use std::path::{Path, PathBuf};

use serde_json::Value;
use ttskit_core::unpack::eol;
use ttskit_core::{Eol, PackError, PackOptions, Save, pack, unpack};

fn options(keep_eol: bool) -> PackOptions {
    PackOptions {
        keep_eol,
        native: Eol::Crlf,
        ..PackOptions::new(keep_eol)
    }
}

fn unpack_to(save: &[u8], eol: Eol) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tree");
    unpack(save, eol).unwrap().write_to_dir(&dir).unwrap();
    (tmp, dir)
}

fn packed(dir: &Path, keep_eol: bool) -> Value {
    let out = pack(dir, &options(keep_eol)).unwrap();
    serde_json::from_slice(&out.bytes).unwrap()
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn err(dir: &Path) -> String {
    pack(dir, &options(false)).unwrap_err().to_string()
}

/// The input with every line break converted to LF, as the default pack
/// output is expected to be.
fn normalized(input: &[u8]) -> Vec<u8> {
    let mut value = Save::from_slice(input).unwrap().into_value();
    fn walk(v: &mut Value) {
        match v {
            Value::String(s) => *s = String::from_utf8(eol::convert(s, Eol::Lf)).unwrap(),
            Value::Array(a) => a.iter_mut().for_each(walk),
            Value::Object(m) => m.values_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut value);
    ttskit_core::save::write::to_vec_with(&value, Eol::Lf, false)
}

#[test]
fn keep_eol_is_byte_exact_for_every_fixture() {
    for path in ttskit_testutil::fixture_saves() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let input = std::fs::read(&path).unwrap();
        for eol in [Eol::Crlf, Eol::Lf] {
            let unpacked = unpack(&input, eol).unwrap();
            let (_tmp, dir) = unpack_to(&input, eol);
            let out = pack(&dir, &options(true)).unwrap();
            assert!(out.warnings.is_empty(), "{name}: {:?}", out.warnings);
            assert_eq!(out.objects, unpacked.report.objects, "{name}");
            if unpacked.report.mixed_eol.is_empty() {
                assert!(
                    out.bytes == input,
                    "{name} ({eol:?}): {}",
                    ttskit_testutil::first_diff(&input, &out.bytes)
                );
            } else {
                // Mixed fields are written in their majority style.
                let a = Save::from_slice(&input).unwrap();
                let b = Save::from_slice(&out.bytes).unwrap();
                assert_eq!(
                    a.map().keys().collect::<Vec<_>>(),
                    b.map().keys().collect::<Vec<_>>()
                );
            }
        }
    }
}

#[test]
fn default_output_is_the_input_with_lf_line_endings() {
    for path in ttskit_testutil::fixture_saves() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let input = std::fs::read(&path).unwrap();
        for eol in [Eol::Crlf, Eol::Lf] {
            let (_tmp, dir) = unpack_to(&input, eol);
            let out = pack(&dir, &options(false)).unwrap();
            assert!(
                out.bytes == normalized(&input),
                "{name} ({eol:?}): {}",
                ttskit_testutil::first_diff(&normalized(&input), &out.bytes)
            );
            assert!(!out.bytes.contains(&b'\r') || !out.bytes.windows(2).any(|w| w == b"\r\n"));
        }
    }
}

#[test]
fn unpack_pack_unpack_gives_an_identical_tree() {
    let input = ttskit_testutil::read_fixture("layout_edge_cases.json");
    for keep_eol in [false, true] {
        let (_tmp, dir) = unpack_to(&input, Eol::Crlf);
        let out = pack(&dir, &options(keep_eol)).unwrap();
        let first = unpack(&input, Eol::Crlf).unwrap();
        let second = unpack(&out.bytes, Eol::Crlf).unwrap();
        let paths =
            |u: &ttskit_core::Unpacked| u.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&first), paths(&second));
        if !keep_eol {
            // Only the recorded style of the files can differ.
            let third = unpack(&out.bytes, Eol::Crlf).unwrap();
            assert_eq!(second.files, third.files);
        }
    }
}

#[test]
fn edits_and_deletions_are_reflected() {
    let input = ttskit_testutil::read_fixture("scripts_ui.json");
    let (_tmp, dir) = unpack_to(&input, Eol::Crlf);
    write(&dir, "script.lua", "print('edited')\r\nreturn 1");
    std::fs::remove_file(dir.join("ui.xml")).unwrap();
    let save = packed(&dir, false);
    assert_eq!(save["LuaScript"], "print('edited')\nreturn 1");
    assert!(save.get("XmlUI").is_none());
    // The other keys keep their order.
    let original = Save::from_slice(&input).unwrap();
    let keys: Vec<&String> = save.as_object().unwrap().keys().collect();
    let expected: Vec<&String> = original.map().keys().filter(|k| *k != "XmlUI").collect();
    assert_eq!(keys, expected);

    let index: Vec<String> =
        serde_json::from_slice(&std::fs::read(dir.join("objects").join("index.json")).unwrap())
            .unwrap();
    let first = dir.join("objects").join(&index[0]);
    let object_json = first.join("object.json");
    let text = std::fs::read_to_string(&object_json).unwrap();
    std::fs::write(
        &object_json,
        text.replacen("\"Nickname\": \"", "\"Nickname\": \"X", 1),
    )
    .unwrap();
    let edited = packed(&dir, false);
    assert!(
        edited["ObjectStates"][0]["Nickname"]
            .as_str()
            .unwrap()
            .starts_with('X')
    );
}

#[test]
fn adding_a_script_to_an_empty_or_missing_key() {
    let save = br#"{"A":1,"LuaScript":"","XmlUI":"x","B":2,"ObjectStates":[{"GUID":"g1","Name":"N","ContainedObjects":[]}]}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Lf);
    // Empty value: replaced in place. Missing key: added at the end.
    write(&dir, "script.lua", "a\r\nb");
    write(&dir, "objects/N.g1/script.lua", "obj");
    write(&dir, "objects/N.g1/state.txt", "plain");
    let out = packed(&dir, false);
    assert_eq!(out["LuaScript"], "a\nb");
    let keys: Vec<&String> = out.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["A", "LuaScript", "XmlUI", "B", "ObjectStates"]);
    let object = &out["ObjectStates"][0];
    let keys: Vec<&String> = object.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "GUID",
            "Name",
            "ContainedObjects",
            "LuaScript",
            "LuaScriptState"
        ]
    );
    assert_eq!(object["LuaScriptState"], "plain");
}

#[test]
fn a_file_next_to_a_literal_value_is_an_error() {
    let save = br#"{"LuaScript":"print(1)"}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Lf);
    write(&dir, "save.json", "{\"LuaScript\": \"print(1)\"}");
    write(&dir, "script.lua", "x");
    let message = err(&dir);
    assert!(message.contains("save.json"), "{message}");
    assert!(message.contains("script.lua"), "{message}");
    // A marker for another file is an error too.
    write(&dir, "save.json", "{\"LuaScript\": \"file:ui.xml\"}");
    let message = err(&dir);
    assert!(message.contains("not allowed here"), "{message}");
    write(&dir, "save.json", "{\"LuaScript\": \"file:../x.lua\"}");
    assert!(err(&dir).contains("not allowed"));
    // Without the file, the literal is kept.
    std::fs::remove_file(dir.join("script.lua")).unwrap();
    write(&dir, "save.json", "{\"LuaScript\": \"print(1)\"}");
    assert_eq!(packed(&dir, false)["LuaScript"], "print(1)");
}

#[test]
fn state_json_and_state_txt_cannot_coexist() {
    let save = br#"{"LuaScriptState":"{\"a\":1}"}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Lf);
    write(&dir, "state.txt", "x");
    assert!(err(&dir).contains("state.json and state.txt"));
    std::fs::remove_file(dir.join("state.json")).unwrap();
    // The marker names state.json but only state.txt exists.
    assert!(err(&dir).contains("state.txt"));
}

#[test]
fn pretty_state_is_compacted_again() {
    let save = br#"{"LuaScriptState":"{\"a\":1,\"b\":[1.0,2]}"}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Crlf);
    assert_eq!(
        packed(&dir, false)["LuaScriptState"],
        "{\"a\":1,\"b\":[1.0,2]}"
    );
    write(&dir, "state.json", "{ \"a\": 5,\n \"c\": null }\n");
    assert_eq!(
        packed(&dir, false)["LuaScriptState"],
        "{\"a\":5,\"c\":null}"
    );
    write(&dir, "state.json", "{ \"a\": ");
    let message = err(&dir);
    assert!(
        message.contains("state.json") && message.contains("invalid JSON"),
        "{message}"
    );
}

#[test]
fn verbatim_state_json_is_not_compacted() {
    let save = br#"{"LuaScriptState":"{\"a\": 1}"}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Lf);
    assert_eq!(packed(&dir, false)["LuaScriptState"], "{\"a\": 1}");
}

#[test]
fn objects_can_be_added_removed_and_reordered() {
    let input = ttskit_testutil::read_fixture("containers.json");
    let (_tmp, dir) = unpack_to(&input, Eol::Lf);
    let before = packed(&dir, false);
    let count = before["ObjectStates"].as_array().unwrap().len();

    // Reorder.
    let index = dir.join("objects").join("index.json");
    let mut names: Vec<String> = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
    names.reverse();
    std::fs::write(&index, serde_json::to_string(&names).unwrap()).unwrap();
    let reordered = packed(&dir, false);
    assert_eq!(
        reordered["ObjectStates"][0]["GUID"],
        before["ObjectStates"][count - 1]["GUID"]
    );

    // A directory without an entry.
    write(&dir, "objects/New.n001/object.json", "{\"GUID\": \"n001\"}");
    let message = err(&dir);
    assert!(
        message.contains("New.n001") && message.contains("not listed"),
        "{message}"
    );

    // Add it to the index.
    names.push("New.n001".to_owned());
    std::fs::write(&index, serde_json::to_string(&names).unwrap()).unwrap();
    let added = packed(&dir, false);
    assert_eq!(added["ObjectStates"].as_array().unwrap().len(), count + 1);
    assert_eq!(added["ObjectStates"][count]["GUID"], "n001");

    // An entry without a directory.
    std::fs::remove_dir_all(dir.join("objects").join("New.n001")).unwrap();
    let message = err(&dir);
    assert!(
        message.contains("index.json") && message.contains("New.n001"),
        "{message}"
    );

    // Remove an object: directory and entry.
    names.pop();
    let gone = names.pop().unwrap();
    std::fs::write(&index, serde_json::to_string(&names).unwrap()).unwrap();
    std::fs::remove_dir_all(dir.join("objects").join(&gone)).unwrap();
    assert_eq!(
        packed(&dir, false)["ObjectStates"]
            .as_array()
            .unwrap()
            .len(),
        count - 1
    );
}

#[test]
fn removing_all_contents_and_states() {
    let input = ttskit_testutil::read_fixture("states.json");
    let (_tmp, dir) = unpack_to(&input, Eol::Lf);
    let states = dir.join("objects").join("State one.sta001").join("states");
    let index = states.join("index.json");
    // Swap the two states.
    write(
        &dir,
        "objects/State one.sta001/states/index.json",
        r#"[{"key":"3","dir":"3.State three.sta003"},{"key":"2","dir":"2.State two.sta002"}]"#,
    );
    let _ = index;
    let save = packed(&dir, false);
    let keys: Vec<&String> = save["ObjectStates"][0]["States"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys, ["3", "2"]);
    // A state key listed twice.
    write(
        &dir,
        "objects/State one.sta001/states/index.json",
        r#"[{"key":"2","dir":"2.State two.sta002"},{"key":"2","dir":"3.State three.sta003"}]"#,
    );
    assert!(err(&dir).contains("listed twice"));
}

#[test]
fn unsafe_index_entries_are_rejected() {
    let save = br#"{"ObjectStates":[{"GUID":"a"}]}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Lf);
    for bad in ["../tree", "a/b", "..", "", "C:evil", "x\\y"] {
        write(
            &dir,
            "objects/index.json",
            &serde_json::to_string(&[bad]).unwrap(),
        );
        let message = err(&dir);
        assert!(
            message.contains("index.json") && message.contains("not a valid"),
            "{bad}: {message}"
        );
    }
    write(&dir, "objects/index.json", "{}");
    assert!(err(&dir).contains("array"));
    write(&dir, "objects/index.json", "[5]");
    assert!(err(&dir).contains("not a string"));
}

#[test]
fn missing_and_malformed_files_name_the_path() {
    let input = ttskit_testutil::read_fixture("containers.json");
    let (_tmp, dir) = unpack_to(&input, Eol::Lf);
    let object = dir
        .join("objects")
        .join("Outer bag.bag001")
        .join("object.json");
    let original = std::fs::read_to_string(&object).unwrap();

    std::fs::write(&object, "{\n  \"a\": ,\n}").unwrap();
    let message = err(&dir);
    assert!(
        message.contains("object.json") && message.contains("line 2"),
        "{message}"
    );

    std::fs::write(&object, "[1]").unwrap();
    assert!(err(&dir).contains("must contain a JSON object"));

    std::fs::remove_file(&object).unwrap();
    let message = err(&dir);
    assert!(
        message.contains("cannot read") && message.contains("object.json"),
        "{message}"
    );
    std::fs::write(&object, original).unwrap();

    write(&dir, "save.json", "{");
    assert!(err(&dir).contains("save.json"));
    std::fs::remove_file(dir.join("save.json")).unwrap();
    assert!(err(&dir).contains("save.json"));

    let missing = tempfile::tempdir().unwrap().path().join("nope");
    assert!(matches!(
        pack(&missing, &options(false)),
        Err(PackError::NotADirectory(_))
    ));
}

#[test]
fn invalid_utf8_is_an_error() {
    let save = br#"{"LuaScript":"x"}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Lf);
    std::fs::write(dir.join("script.lua"), [0x66, 0xFF, 0x6F]).unwrap();
    let message = err(&dir);
    assert!(
        message.contains("script.lua") && message.contains("UTF-8"),
        "{message}"
    );
}

#[test]
fn a_byte_order_mark_is_stripped() {
    let save = br#"{"LuaScript":"x","XmlUI":"<a/>","LuaScriptState":"not json"}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Lf);
    std::fs::write(dir.join("script.lua"), b"\xEF\xBB\xBFprint(1)").unwrap();
    std::fs::write(dir.join("state.txt"), b"\xEF\xBB\xBFhello").unwrap();
    let mut json = b"\xEF\xBB\xBF".to_vec();
    json.extend_from_slice(&std::fs::read(dir.join("save.json")).unwrap());
    std::fs::write(dir.join("save.json"), json).unwrap();
    let out = packed(&dir, false);
    assert_eq!(out["LuaScript"], "print(1)");
    assert_eq!(out["LuaScriptState"], "hello");
}

#[test]
fn unknown_files_warn_and_hidden_files_do_not() {
    let input = ttskit_testutil::read_fixture("containers.json");
    let (_tmp, dir) = unpack_to(&input, Eol::Lf);
    write(&dir, "README.md", "x");
    write(&dir, ".gitattributes", "x");
    write(&dir, "objects/Outer bag.bag001/notes.txt", "x");
    write(&dir, "objects/stray.txt", "x");
    write(&dir, "notebook/orphan.txt", "x");
    let out = pack(&dir, &options(false)).unwrap();
    let mut warnings = out.warnings.clone();
    warnings.sort();
    assert_eq!(
        warnings,
        [
            "ignored unknown file README.md",
            "ignored unknown file notebook/orphan.txt",
            "ignored unknown file objects/Outer bag.bag001/notes.txt",
            "ignored unknown file objects/stray.txt",
        ]
    );
}

#[test]
fn metadata_is_optional_and_checked() {
    let input = ttskit_testutil::read_fixture("layout_edge_cases.json");
    let (_tmp, dir) = unpack_to(&input, Eol::Crlf);
    // Without metadata: nothing is prettified, --keep-eol uses `native`.
    std::fs::remove_dir_all(dir.join(".ttskit")).unwrap();
    let default = pack(&dir, &options(false)).unwrap();
    assert!(default.warnings.is_empty(), "{:?}", default.warnings);
    let kept = pack(&dir, &options(true)).unwrap();
    assert!(kept.bytes.windows(2).any(|w| w == b"\r\n"));
    let lf_native = pack(
        &dir,
        &PackOptions {
            keep_eol: true,
            native: Eol::Lf,
            ..PackOptions::new(true)
        },
    )
    .unwrap();
    assert!(!lf_native.bytes.windows(2).any(|w| w == b"\r\n"));
    let state = packed(&dir, false);
    let board = &state["ObjectStates"][0]["LuaScriptState"];
    assert!(
        board.as_str().unwrap().contains('\n'),
        "state stays prettified: {board}"
    );

    write(&dir, ".ttskit/unpack.json", "{\"format\": 2}");
    let message = err(&dir);
    assert!(message.contains("unknown layout format 2"), "{message}");
    write(&dir, ".ttskit/unpack.json", "{}");
    assert!(err(&dir).contains("missing `format`"));
    write(&dir, ".ttskit/unpack.json", "{");
    assert!(err(&dir).contains("unpack.json"));
}

#[test]
fn keep_eol_leaves_inline_strings_alone_and_default_normalizes_them() {
    let save = b"{\r\n  \"Description\": \"a\\r\\nb\\rc\",\r\n  \"LuaScript\": \"x\\r\\ny\"\r\n}";
    let (_tmp, dir) = unpack_to(save, Eol::Crlf);
    let kept = packed(&dir, true);
    assert_eq!(kept["Description"], "a\r\nb\rc");
    assert_eq!(kept["LuaScript"], "x\r\ny");
    let default = packed(&dir, false);
    assert_eq!(default["Description"], "a\nb\rc");
    assert_eq!(default["LuaScript"], "x\ny");
    let bytes = pack(&dir, &options(true)).unwrap().bytes;
    assert!(bytes.starts_with(b"{\r\n  \"Description\""));
    assert_eq!(
        pack(&dir, &options(false)).unwrap().bytes,
        b"{\n  \"Description\": \"a\\nb\\rc\",\n  \"LuaScript\": \"x\\ny\"\n}"
    );
}

#[test]
fn new_files_without_a_record_get_the_native_line_ending() {
    let save = br#"{"LuaScript":"a\r\nb"}"#;
    let (_tmp, dir) = unpack_to(save, Eol::Lf);
    write(&dir, "ui.xml", "<a>\n</a>");
    let crlf = packed(&dir, true);
    assert_eq!(crlf["LuaScript"], "a\r\nb");
    assert_eq!(crlf["XmlUI"], "<a>\r\n</a>");
}

#[test]
fn notebook_pages_are_edited_and_removed() {
    let input = ttskit_testutil::read_fixture("layout_edge_cases.json");
    let (_tmp, dir) = unpack_to(&input, Eol::Lf);
    write(&dir, "notebook/0.Rules.txt", "new rules");
    std::fs::remove_file(dir.join("notebook").join("1.txt")).unwrap();
    let save = packed(&dir, false);
    assert_eq!(save["TabStates"]["0"]["body"], "new rules");
    assert!(save["TabStates"]["1"].get("body").is_none());
    write(
        &dir,
        "save.json",
        &std::fs::read_to_string(dir.join("save.json"))
            .unwrap()
            .replace("file:notebook/0.Rules.txt", "file:notebook/../x.txt"),
    );
    assert!(err(&dir).contains("must be `file:notebook/<name>.txt`"));
}
