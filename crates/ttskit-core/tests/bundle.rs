//! Tests for `--include`: inlining `require()` and `<Include>` on pack.

use std::path::{Path, PathBuf};

use serde_json::Value;
use ttskit_core::{Eol, PackOptions, pack};

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A tree with a global script and UI, an object with a script, and a lib
/// directory next to it.
struct Fixture {
    _tmp: tempfile::TempDir,
    tree: PathBuf,
    lib: PathBuf,
}

fn fixture(script: &str, ui: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("tree");
    let lib = tmp.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    write(
        &tree,
        "save.json",
        r#"{"LuaScript": "file:script.lua", "XmlUI": "file:ui.xml", "ObjectStates": "dir:objects"}"#,
    );
    write(&tree, "script.lua", script);
    write(&tree, "ui.xml", ui);
    write(&tree, "objects/index.json", r#"["A.1", "B.2"]"#);
    for (dir, script) in [("A.1", "local u = require(\"util\")"), ("B.2", "-- none")] {
        write(
            &tree,
            &format!("objects/{dir}/object.json"),
            r#"{"GUID": "x", "LuaScript": "file:script.lua"}"#,
        );
        write(&tree, &format!("objects/{dir}/script.lua"), script);
    }
    Fixture {
        _tmp: tmp,
        tree,
        lib,
    }
}

fn run(f: &Fixture, includes: &[&Path]) -> Result<Value, String> {
    let options = PackOptions {
        keep_eol: false,
        native: Eol::Crlf,
        includes: includes.iter().map(|p| p.to_path_buf()).collect(),
        ..PackOptions::new(false)
    };
    pack(&f.tree, &options)
        .map(|p| serde_json::from_slice(&p.bytes).unwrap())
        .map_err(|e| e.to_string())
}

fn lua(v: &Value) -> &str {
    v["LuaScript"].as_str().unwrap()
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn without_include_scripts_are_untouched() {
    let f = fixture("local a = require(\"util\")", "<Include src=\"x\"/>");
    write(&f.lib, "util.lua", "return 1");
    let v = run(&f, &[]).unwrap();
    assert_eq!(lua(&v), "local a = require(\"util\")");
    assert_eq!(v["XmlUI"], "<Include src=\"x\"/>");
}

#[test]
fn inlines_nested_shared_and_dotted_modules() {
    let f = fixture(
        "local a = require(\"a\")\nlocal b = require 'sub.b'\nlocal c = require(\"sub/b\")\nprint(a, b, c)",
        "",
    );
    write(
        &f.lib,
        "a.lua",
        "local s = require(\"shared\")\nreturn { s = s }",
    );
    write(
        &f.lib,
        "sub/b.lua",
        "local s = require(\"shared\")\nreturn s",
    );
    write(&f.lib, "shared.ttslua", "return 42");
    write(&f.lib, "util.lua", "return 'util'");
    let v = run(&f, &[&f.lib]).unwrap();
    let main = lua(&v);
    assert!(main.contains("__bundle_register(\"a\""), "{main}");
    assert!(main.contains("__bundle_register(\"sub.b\""), "{main}");
    // `sub/b` is the same file as `sub.b`: an alias, not a second copy.
    assert!(
        main.contains("__bundle_register(\"sub/b\", function(require, _LOADED, __bundle_register, __bundle_modules)\nreturn require(\"sub.b\")"),
        "{main}"
    );
    // Used by two modules, registered once.
    assert_eq!(count(main, "__bundle_register(\"shared\""), 1, "{main}");
    assert_eq!(count(main, "return 42"), 1);
    assert!(main.contains("__bundle_register(\"__root\""));
    assert!(main.ends_with("return __bundle_require(\"__root\")\n"));
    assert!(!main.contains('\r'));

    // Every object is bundled on its own: A uses `util`, B needs nothing.
    let objects = v["ObjectStates"].as_array().unwrap();
    let a = objects[0]["LuaScript"].as_str().unwrap();
    assert!(a.contains("__bundle_register(\"util\"") && a.contains("return 'util'"));
    assert_eq!(objects[1]["LuaScript"], "-- none");
}

#[test]
fn lua_cycles_are_registered_once() {
    let f = fixture("require(\"a\")", "");
    write(&f.lib, "a.lua", "local b = require(\"b\")\nreturn {}");
    write(&f.lib, "b.lua", "local a = require(\"a\")\nreturn {}");
    write(&f.lib, "util.lua", "return 1");
    let v = run(&f, &[&f.lib]).unwrap();
    let main = lua(&v);
    assert_eq!(count(main, "__bundle_register(\"a\""), 1);
    assert_eq!(count(main, "__bundle_register(\"b\""), 1);
}

#[test]
fn only_real_literal_requires_are_followed() {
    let f = fixture(
        "-- require(\"in_comment\")\n--[[ require(\"in_block\") ]]\nlocal s = \"require('in_string')\"\nlocal l = [[ require(\"in_long\") ]]\nlocal d = require(name)\nlocal m = obj.require(\"member\")\nlocal n = obj:require(\"method\")\nlocal r = require(\"real\")\nlocal cat = x..require(\"after_concat\")\n",
        "",
    );
    write(&f.lib, "real.lua", "return 1");
    write(&f.lib, "after_concat.lua", "return 2");
    write(&f.lib, "util.lua", "return 3");
    let v = run(&f, &[&f.lib]).unwrap();
    let main = lua(&v);
    assert!(main.contains("__bundle_register(\"real\""));
    assert!(main.contains("__bundle_register(\"after_concat\""));
    for name in [
        "in_comment",
        "in_block",
        "in_string",
        "in_long",
        "member",
        "method",
    ] {
        assert!(
            !main.contains(&format!("__bundle_register(\"{name}\"")),
            "{name}"
        );
    }
}

#[test]
fn already_bundled_script_is_left_alone() {
    let script = "__bundle_register(\"x\", function() end)\nrequire(\"missing\")";
    let f = fixture(script, "");
    write(&f.lib, "util.lua", "return 1");
    assert_eq!(lua(&run(&f, &[&f.lib]).unwrap()), script);
}

#[test]
fn include_directories_are_searched_in_order() {
    let f = fixture("require(\"m\")", "");
    let other = f.lib.parent().unwrap().join("other");
    write(&other, "m.lua", "return 'other'");
    write(&f.lib, "m.lua", "return 'lib'");
    write(&f.lib, "util.lua", "return 1");
    let v = run(&f, &[&other, &f.lib]).unwrap();
    assert!(lua(&v).contains("return 'other'") && !lua(&v).contains("return 'lib'"));
    let v = run(&f, &[&f.lib, &other]).unwrap();
    assert!(lua(&v).contains("return 'lib'"));
}

#[test]
fn missing_module_names_module_and_requiring_file() {
    let f = fixture("require(\"nope\")", "");
    write(&f.lib, "util.lua", "return 1");
    let message = run(&f, &[&f.lib]).unwrap_err();
    assert!(message.contains("\"nope\""), "{message}");
    assert!(message.contains("script.lua"), "{message}");
    assert!(message.contains("nope.lua"), "{message}");

    // Missing from inside a module: that module's file is named.
    let f = fixture("require(\"a\")", "");
    write(&f.lib, "a.lua", "require(\"deep.gone\")");
    write(&f.lib, "util.lua", "return 1");
    let message = run(&f, &[&f.lib]).unwrap_err();
    assert!(message.contains("\"deep.gone\""), "{message}");
    assert!(message.contains("a.lua"), "{message}");
}

#[test]
fn module_names_cannot_leave_the_include_directory() {
    for name in ["../secret", "..x", "a..b", "/abs"] {
        let f = fixture(&format!("require(\"{name}\")"), "");
        write(&f.lib, "util.lua", "return 1");
        let message = run(&f, &[&f.lib]).unwrap_err();
        assert!(message.contains("not a valid"), "{name}: {message}");
    }
}

#[test]
fn include_dir_must_exist() {
    let f = fixture("", "");
    let message = run(&f, &[&f.lib.join("missing")]).unwrap_err();
    assert!(message.contains("not a directory"), "{message}");
}

#[test]
fn line_endings_follow_the_normal_rules() {
    let f = fixture("require(\"a\")\r\nprint(1)\r\n", "");
    write(&f.lib, "a.lua", "return 1\r\n");
    write(&f.lib, "util.lua", "return 1");
    let v = run(&f, &[&f.lib]).unwrap();
    assert!(!lua(&v).contains('\r'));
}

#[test]
fn xml_includes_are_inlined_recursively() {
    let f = fixture(
        "",
        "<Panel>\n  <Include src=\"widgets/button\"/>\n  <Include src='row' ></Include>\n  <!-- <Include src=\"in_comment\"/> -->\n  <![CDATA[<Include src=\"in_cdata\"/>]]>\n</Panel>",
    );
    write(
        &f.lib,
        "widgets/button.xml",
        "<Button>\r\n<Include src=\"icon\"/></Button>",
    );
    write(&f.lib, "icon.xml", "<Image/>");
    write(&f.lib, "row.xml", "<Row/>");
    write(&f.lib, "util.lua", "return 1");
    let v = run(&f, &[&f.lib]).unwrap();
    assert_eq!(
        v["XmlUI"],
        "<Panel>\n  <Button>\n<Image/></Button>\n  <Row/>\n  <!-- <Include src=\"in_comment\"/> -->\n  <![CDATA[<Include src=\"in_cdata\"/>]]>\n</Panel>"
    );
}

#[test]
fn xml_missing_include_and_cycle_are_errors() {
    let f = fixture("", "<Include src=\"gone\"/>");
    write(&f.lib, "util.lua", "return 1");
    let message = run(&f, &[&f.lib]).unwrap_err();
    assert!(
        message.contains("\"gone\"") && message.contains("ui.xml"),
        "{message}"
    );

    let f = fixture("", "<Include src=\"a\"/>");
    write(&f.lib, "util.lua", "return 1");
    write(&f.lib, "a.xml", "<Include src=\"b\"/>");
    write(&f.lib, "b.xml", "<Include src=\"a\"/>");
    let message = run(&f, &[&f.lib]).unwrap_err();
    assert!(message.contains("include cycle"), "{message}");
    assert!(
        message.contains("a.xml") && message.contains("b.xml"),
        "{message}"
    );

    let f = fixture("", "<Include/>");
    write(&f.lib, "util.lua", "return 1");
    let message = run(&f, &[&f.lib]).unwrap_err();
    assert!(message.contains("no src attribute"), "{message}");
}
