mod common;

use common::*;
use serde_json::{Value, json};
use ttskit_core::Save;
use ttskit_diff::{ChangeKind, ObjectStatus, ParentRef, render_text, to_json};

fn cube(guid: &str, extra: Value) -> Value {
    let mut v = obj(guid, "Cube");
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    v
}

#[test]
fn equal_saves_are_empty() {
    let s = with_objects(vec![obj("a", "A")]);
    let d = run(&s, &s.clone());
    assert!(d.is_empty());
    assert_eq!(render_text(&d, true), "no differences\n");
}

#[test]
fn added_and_removed() {
    let old = with_objects(vec![obj("a", "A"), obj("b", "B")]);
    let new = with_objects(vec![obj("b", "B"), obj("c", "C")]);
    let d = run(&old, &new);
    assert_eq!(
        statuses(&d),
        vec![
            (ObjectStatus::Added, "c".into()),
            (ObjectStatus::Removed, "a".into())
        ]
    );
    let text = render_text(&d, false);
    assert!(text.starts_with("1 added, 1 removed, 0 moved, 0 changed, 0 global\n"));
    assert!(text.contains("+ C (c)\n"));
    assert!(text.contains("- A (a)\n"));
}

#[test]
fn move_between_containers() {
    let old = with_objects(vec![
        bag("b1", "Bag one", vec![obj("x", "X")]),
        bag("b2", "Bag two", vec![]),
    ]);
    let new = with_objects(vec![
        bag("b1", "Bag one", vec![]),
        bag("b2", "Bag two", vec![obj("x", "X")]),
    ]);
    let d = run(&old, &new);
    assert_eq!(d.objects.len(), 1);
    let x = find(&d, "x");
    assert_eq!(x.status, ObjectStatus::Moved);
    let m = x.moved.as_ref().unwrap();
    assert_eq!(
        m.from,
        ParentRef::Object {
            guid: "b1".into(),
            nickname: "Bag one".into()
        }
    );
    assert!(matches!(&m.to, ParentRef::Object { guid, .. } if guid == "b2"));
    assert!(render_text(&d, false).contains("> X (x) moved from Bag one (b1) to Bag two (b2)"));
}

#[test]
fn move_between_top_level_and_container() {
    let old = with_objects(vec![bag("b1", "Bag", vec![]), obj("x", "X")]);
    let new = with_objects(vec![bag("b1", "Bag", vec![obj("x", "X")])]);
    let d = run(&old, &new);
    let x = find(&d, "x");
    assert_eq!(x.moved.as_ref().unwrap().from, ParentRef::TopLevel);
    let back = run(&new, &old);
    assert_eq!(
        find(&back, "x").moved.as_ref().unwrap().to,
        ParentRef::TopLevel
    );
    assert!(render_text(&back, false).contains("from Bag (b1) to top level"));
}

#[test]
fn move_between_states() {
    let old = with_objects(vec![json!({
        "GUID": "p", "Nickname": "P",
        "States": { "2": obj("s", "S") }
    })]);
    let new = with_objects(vec![json!({
        "GUID": "p", "Nickname": "P",
        "ContainedObjects": [obj("s", "S")]
    })]);
    assert!(run(&old, &new).objects.is_empty());
    let top = with_objects(vec![obj("s", "S")]);
    assert_eq!(find(&run(&old, &top), "s").status, ObjectStatus::Moved);
}

#[test]
fn moved_and_changed_reports_both() {
    let old = with_objects(vec![bag("b1", "Bag", vec![]), obj("x", "Old")]);
    let new = with_objects(vec![bag("b1", "Bag", vec![obj("x", "New")])]);
    let d = run(&old, &new);
    let x = find(&d, "x");
    assert_eq!(x.status, ObjectStatus::Moved);
    assert_eq!(x.changes[0].path, "Nickname");
    let text = render_text(&d, false);
    assert!(text.contains("> New (x) moved"));
    assert!(text.contains("~ New (x) changed: other"));
}

#[test]
fn container_content_change_does_not_change_container() {
    let old = with_objects(vec![bag("b1", "Bag", vec![obj("x", "X")])]);
    let new = with_objects(vec![bag("b1", "Bag", vec![obj("x", "Y")])]);
    let d = run(&old, &new);
    assert_eq!(d.objects.len(), 1);
    assert_eq!(d.objects[0].guid, "x");
}

fn at(x: &str) -> Value {
    serde_json::from_str(&format!(
        r#"{{"posX":{x},"posY":1.0,"rotY":90.0,"scaleX":1.0}}"#
    ))
    .unwrap()
}

fn with_transform(x: &str) -> Save {
    with_objects(vec![cube("a", json!({ "Transform": at(x) }))])
}

#[test]
fn transform_exact_and_number_text() {
    let d = run(&with_transform("1.0"), &with_transform("1.0"));
    assert!(d.is_empty());
    let d = run(&with_transform("1.0"), &with_transform("1"));
    let c = &find(&d, "a").changes;
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].kind, ChangeKind::Transform);
    assert_eq!(c[0].path, "Transform.posX");
    let text = render_text(&d, true);
    assert!(text.contains("~ Cube (a) changed: transform"));
    assert!(text.contains("transform Transform.posX: 1.0 -> 1"));
}

#[test]
fn transform_tolerance() {
    let (a, b) = (with_transform("1.0"), with_transform("1.004"));
    assert!(run_with(&a, &b, 0.01, false).is_empty());
    assert!(run_with(&a, &b, 0.005, false).is_empty());
    assert!(!run_with(&a, &b, 0.003, false).is_empty());
    assert!(!run(&a, &b).is_empty());
}

#[test]
fn tolerance_applies_only_to_transform() {
    let old = with_objects(vec![cube("a", json!({ "Value": 1.0 }))]);
    let new = with_objects(vec![cube("a", json!({ "Value": 1.001 }))]);
    let d = run_with(&old, &new, 1.0, false);
    assert_eq!(find(&d, "a").changes[0].kind, ChangeKind::Other);
}

#[test]
fn script_counts_and_full_diff() {
    let old = with_objects(vec![cube("a", json!({ "LuaScript": "a\nb\nc\n" }))]);
    let new = with_objects(vec![cube("a", json!({ "LuaScript": "a\nB\nc\nd\n" }))]);
    let d = run(&old, &new);
    let c = &find(&d, "a").changes[0];
    assert_eq!(c.kind, ChangeKind::Script);
    let lines = c.lines.as_ref().unwrap();
    assert_eq!((lines.added, lines.removed), (2, 1));
    assert!(lines.diff.is_none());
    assert!(render_text(&d, true).contains("script LuaScript: +2 -1"));

    let d = run_with(&old, &new, 0.0, true);
    let diff = find(&d, "a").changes[0]
        .lines
        .as_ref()
        .unwrap()
        .diff
        .clone()
        .unwrap();
    assert!(diff.contains("-b") && diff.contains("+B") && diff.contains("+d"));
    assert!(render_text(&d, true).contains("        +B"));
    assert!(!render_text(&d, false).contains("+B"));
}

#[test]
fn xml_ui_and_script_state() {
    let old = with_objects(vec![cube(
        "a",
        json!({ "XmlUI": "<a/>", "LuaScriptState": "{}" }),
    )]);
    let new = with_objects(vec![cube(
        "a",
        json!({ "XmlUI": "<b/>", "LuaScriptState": "{\"x\":1}" }),
    )]);
    let d = run_with(&old, &new, 0.0, true);
    let kinds: Vec<_> = find(&d, "a").changes.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, vec![ChangeKind::XmlUi, ChangeKind::ScriptState]);
    // Script state never gets the full diff.
    assert!(
        find(&d, "a").changes[1]
            .lines
            .as_ref()
            .unwrap()
            .diff
            .is_none()
    );
    assert!(
        find(&d, "a").changes[0]
            .lines
            .as_ref()
            .unwrap()
            .diff
            .is_some()
    );
}

#[test]
fn asset_urls_and_other_fields() {
    let old = with_objects(vec![cube(
        "a",
        json!({ "CustomDeck": { "1": { "FaceURL": "http://x/1.png", "NumWidth": 10 } }, "Description": "d" }),
    )]);
    let new = with_objects(vec![cube(
        "a",
        json!({ "CustomDeck": { "1": { "FaceURL": "http://x/2.png", "NumWidth": 11 } }, "Description": "d2" }),
    )]);
    let d = run(&old, &new);
    let c = &find(&d, "a").changes;
    let got: Vec<_> = c.iter().map(|c| (c.kind, c.path.as_str())).collect();
    assert_eq!(
        got,
        vec![
            (ChangeKind::Assets, "CustomDeck.1.FaceURL"),
            (ChangeKind::Other, "CustomDeck.1.NumWidth"),
            (ChangeKind::Other, "Description"),
        ]
    );
    assert_eq!(c[0].old, Some(json!("http://x/1.png")));
    assert_eq!(c[0].new, Some(json!("http://x/2.png")));
    let text = render_text(&d, true);
    assert!(text.contains("assets CustomDeck.1.FaceURL: \"http://x/1.png\" -> \"http://x/2.png\""));
}

#[test]
fn other_field_added_removed_and_truncated() {
    let long = "x".repeat(300);
    let old = with_objects(vec![cube("a", json!({ "Tags": ["a"], "Gone": 1 }))]);
    let new = with_objects(vec![cube("a", json!({ "Tags": ["a", "b"], "Long": long }))]);
    let d = run(&old, &new);
    let paths: Vec<_> = find(&d, "a")
        .changes
        .iter()
        .map(|c| c.path.as_str())
        .collect();
    assert_eq!(paths, vec!["Gone", "Long", "Tags[1]"]);
    let text = render_text(&d, true);
    assert!(text.contains("Gone: 1 -> <absent>"));
    assert!(text.contains("..."));
    assert!(!text.contains(&"x".repeat(100)));
}

#[test]
fn key_order_is_not_a_difference() {
    let old = Save::from_str_for_test(r#"{"ObjectStates":[{"GUID":"a","B":1,"C":2}]}"#);
    let new = Save::from_str_for_test(r#"{"ObjectStates":[{"C":2,"GUID":"a","B":1}]}"#);
    assert!(run(&old, &new).is_empty());
}

trait FromStrForTest {
    fn from_str_for_test(s: &str) -> Save;
}
impl FromStrForTest for Save {
    fn from_str_for_test(s: &str) -> Save {
        s.parse().unwrap()
    }
}

#[test]
fn duplicate_guids_pair_by_index() {
    let old = with_objects(vec![obj("d", "one"), obj("d", "two")]);
    let new = with_objects(vec![obj("d", "one"), obj("d", "TWO")]);
    let d = run(&old, &new);
    assert_eq!(d.duplicate_guids.len(), 1);
    assert_eq!(
        (
            d.duplicate_guids[0].old_count,
            d.duplicate_guids[0].new_count
        ),
        (2, 2)
    );
    assert_eq!(d.objects.len(), 1);
    assert_eq!(d.objects[0].nickname, "TWO");
    assert!(d.objects[0].ambiguity.is_some());
    let text = render_text(&d, false);
    assert!(text.contains("[ambiguous: duplicate GUID]"));
    assert!(text.contains("! duplicate GUID d: 2 in old, 2 in new"));
}

#[test]
fn duplicate_guids_unpaired_fallback() {
    let old = with_objects(vec![obj("d", "one"), obj("d", "two")]);
    let new = with_objects(vec![obj("d", "one")]);
    let d = run(&old, &new);
    assert_eq!(statuses(&d), vec![(ObjectStatus::Removed, "d".into())]);
    assert!(d.objects[0].ambiguity.is_some());
    assert_eq!(d.duplicate_guids[0].new_count, 1);

    // A duplicate does not move between parents; it is added and removed.
    let old = with_objects(vec![bag("b", "B", vec![obj("d", "x")]), obj("d", "x")]);
    let new = with_objects(vec![bag("b", "B", vec![]), obj("d", "x"), obj("d", "x")]);
    let d = run(&old, &new);
    assert!(d.objects.iter().all(|o| o.moved.is_none()));
    assert_eq!(d.objects.len(), 2);
}

#[test]
fn missing_guid_matches_by_position_only() {
    let anon = |n: &str| json!({ "Nickname": n, "Name": "Cube" });
    let old = with_objects(vec![anon("a"), anon("b")]);
    let new = with_objects(vec![anon("a"), anon("c")]);
    let d = run(&old, &new);
    assert_eq!(d.objects.len(), 1);
    assert_eq!(d.objects[0].ambiguity, Some(ttskit_diff::Ambiguity::NoGuid));
    assert_eq!(d.objects[0].changes[0].path, "Nickname");
    assert!(d.duplicate_guids.is_empty());
    assert!(render_text(&d, false).contains("[ambiguous: no GUID]"));

    let longer = with_objects(vec![anon("a"), anon("b"), anon("z")]);
    let d = run(&old, &longer);
    assert_eq!(d.objects.len(), 1);
    assert_eq!(d.objects[0].status, ObjectStatus::Added);
}

#[test]
fn output_is_sorted_and_deterministic() {
    let objs = |order: &[&str]| {
        with_objects(
            order
                .iter()
                .map(|g| cube(g, json!({ "Description": g.to_string() })))
                .collect(),
        )
    };
    let old = objs(&["a", "b", "c", "d"]);
    let new_a = with_objects(
        ["d", "c", "b", "a"]
            .iter()
            .map(|g| cube(g, json!({ "Description": format!("{g}!") })))
            .collect(),
    );
    let new_b = with_objects(
        ["b", "d", "a", "c"]
            .iter()
            .map(|g| cube(g, json!({ "Description": format!("{g}!") })))
            .collect(),
    );
    let old_shuffled = objs(&["c", "a", "d", "b"]);
    let d1 = run(&old, &new_a);
    let guids: Vec<_> = d1.objects.iter().map(|o| o.guid.as_str()).collect();
    assert_eq!(guids, vec!["a", "b", "c", "d"]);
    for (o, n) in [(&old, &new_b), (&old_shuffled, &new_a), (&old, &new_a)] {
        let d2 = run(o, n);
        assert_eq!(render_text(&d1, true), render_text(&d2, true));
        assert_eq!(to_json(&d1).to_string(), to_json(&d2).to_string());
    }
}

#[test]
fn json_shape() {
    let old = with_objects(vec![bag("b", "B", vec![]), obj("x", "X")]);
    let new = with_objects(vec![bag("b", "B", vec![obj("x", "Y")])]);
    let j = to_json(&run(&old, &new));
    assert_eq!(j["objects"][0]["status"], "moved");
    assert_eq!(j["objects"][0]["moved"]["from"]["top_level"], true);
    assert_eq!(j["objects"][0]["moved"]["to"]["guid"], "b");
    assert_eq!(j["objects"][0]["changes"][0]["kind"], "other");
    assert_eq!(j["objects"][0]["changes"][0]["new"], "Y");
    assert_eq!(j["globals"], json!([]));
    assert_eq!(j["duplicate_guids"], json!([]));
}
