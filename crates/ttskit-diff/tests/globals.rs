mod common;

use common::*;
use serde_json::{Value, json};
use ttskit_core::Save;
use ttskit_diff::{ChangeKind, GlobalGroup, render_text, to_json};

fn base(extra: Value) -> Save {
    let mut v = json!({ "SaveName": "t", "ObjectStates": [] });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    save(v)
}

fn single(old: Value, new: Value) -> ttskit_diff::GlobalDiff {
    let d = run_with(&base(old), &base(new), 0.0, true);
    assert!(d.objects.is_empty());
    assert_eq!(d.globals.len(), 1, "{d:?}");
    d.globals[0].clone()
}

#[test]
fn global_script_ui_and_state() {
    let g = single(
        json!({ "LuaScript": "a\n" }),
        json!({ "LuaScript": "a\nb\n" }),
    );
    assert_eq!(
        (g.group, g.key.as_str()),
        (GlobalGroup::Script, "LuaScript")
    );
    assert_eq!(g.changes[0].kind, ChangeKind::Script);
    let l = g.changes[0].lines.as_ref().unwrap();
    assert_eq!((l.added, l.removed), (1, 0));
    assert!(l.diff.is_some());

    let g = single(json!({ "XmlUI": "<a/>" }), json!({ "XmlUI": "<b/>" }));
    assert_eq!(
        (g.group, g.changes[0].kind),
        (GlobalGroup::XmlUi, ChangeKind::XmlUi)
    );

    let g = single(
        json!({ "LuaScriptState": "1" }),
        json!({ "LuaScriptState": "2" }),
    );
    assert_eq!(g.group, GlobalGroup::ScriptState);
    assert_eq!(g.changes[0].kind, ChangeKind::ScriptState);
}

#[test]
fn global_table_sky_lighting_hands() {
    let g = single(json!({ "Table": "A" }), json!({ "Table": "B" }));
    assert_eq!(g.group, GlobalGroup::Table);
    let g = single(
        json!({ "TableURL": "http://x/1.png" }),
        json!({ "TableURL": "http://x/2.png" }),
    );
    assert_eq!(
        (g.group, g.changes[0].kind),
        (GlobalGroup::Table, ChangeKind::Assets)
    );
    let g = single(json!({ "Sky": "A" }), json!({ "Sky": "B" }));
    assert_eq!(g.group, GlobalGroup::Sky);
    let g = single(
        json!({ "SkyURL": "http://x/a" }),
        json!({ "SkyURL": "http://x/b" }),
    );
    assert_eq!(
        (g.group, g.changes[0].kind),
        (GlobalGroup::Sky, ChangeKind::Assets)
    );
    let g = single(
        json!({ "Lighting": { "LightIntensity": 0.5 } }),
        json!({ "Lighting": { "LightIntensity": 0.7 } }),
    );
    assert_eq!(g.group, GlobalGroup::Lighting);
    assert_eq!(g.changes[0].path, "Lighting.LightIntensity");
    let g = single(
        json!({ "Hands": { "Enable": true } }),
        json!({ "Hands": { "Enable": false } }),
    );
    assert_eq!(g.group, GlobalGroup::Hands);
}

#[test]
fn global_tab_states_and_ui_assets() {
    let g = single(
        json!({ "TabStates": { "1": { "title": "A" } } }),
        json!({ "TabStates": { "1": { "title": "B" } } }),
    );
    assert_eq!(g.group, GlobalGroup::TabStates);
    assert_eq!(g.changes[0].path, "TabStates.1.title");

    let g = single(
        json!({ "CustomUIAssets": [{ "Type": 0, "Name": "a", "URL": "http://x/1" }] }),
        json!({ "CustomUIAssets": [{ "Type": 0, "Name": "a", "URL": "http://x/2" }] }),
    );
    assert_eq!(g.group, GlobalGroup::UiAssets);
    assert_eq!(g.changes[0].kind, ChangeKind::Assets);
    assert_eq!(g.changes[0].path, "CustomUIAssets[0].URL");
}

#[test]
fn global_other_added_and_removed_keys() {
    let g = single(json!({ "Note": "a" }), json!({ "Note": "b" }));
    assert_eq!((g.group, g.key.as_str()), (GlobalGroup::Other, "Note"));
    let g = single(json!({}), json!({ "Note": "b" }));
    assert_eq!(g.changes[0].old, None);
    assert_eq!(g.changes[0].new, Some(json!("b")));
}

#[test]
fn globals_render_and_sort() {
    let old = base(json!({ "Note": "a", "Sky": "A", "LuaScript": "x" }));
    let new = base(json!({ "Note": "b", "Sky": "B", "LuaScript": "y" }));
    let d = run(&old, &new);
    let keys: Vec<_> = d.globals.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(keys, vec!["LuaScript", "Sky", "Note"]);
    let text = render_text(&d, true);
    assert!(text.starts_with("0 added, 0 removed, 0 moved, 0 changed, 3 global\n"));
    assert!(text.contains("~ global script (LuaScript): script"));
    assert!(text.contains("other Sky: \"A\" -> \"B\""));
    assert_eq!(to_json(&d)["globals"][0]["group"], "script");
}

#[test]
fn global_tolerance_not_applied() {
    let old = base(json!({ "Gravity": 0.5 }));
    let new = base(json!({ "Gravity": 0.51 }));
    assert!(!run_with(&old, &new, 1.0, false).is_empty());
}
