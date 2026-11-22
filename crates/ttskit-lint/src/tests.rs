use serde_json::{Value, json};
use ttskit_core::Save;

use super::*;

fn save(value: Value) -> Save {
    Save::from_value(value).unwrap()
}

fn run(value: Value) -> Vec<Finding> {
    lint(&save(value), &LintConfig::default())
}

fn run_rule(value: Value, rule: &str) -> Vec<Finding> {
    run(value).into_iter().filter(|f| f.rule == rule).collect()
}

fn obj(guid: &str) -> Value {
    json!({ "GUID": guid, "Nickname": format!("n{guid}") })
}

// --- registry and config ---

#[test]
fn registry_is_sorted_and_unique() {
    let ids: Vec<_> = RULES.iter().map(|r| r.id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(ids, sorted);
}

#[test]
fn severity_order_and_parse() {
    assert!(Severity::Info < Severity::Warning && Severity::Warning < Severity::Error);
    assert_eq!("warning".parse::<Severity>().unwrap(), Severity::Warning);
    assert_eq!(Severity::Error.as_str(), "error");
    assert_eq!("off".parse::<Setting>().unwrap(), Setting::Off);
    assert_eq!(
        "info".parse::<Setting>().unwrap(),
        Setting::On(Severity::Info)
    );
    assert!("warn".parse::<Setting>().is_err());
}

#[test]
fn default_config() {
    let c = LintConfig::default();
    assert_eq!(c.state_limit, 1_048_576);
    for r in RULES {
        assert_eq!(c.setting(r.id), Setting::On(r.default));
    }
    assert_eq!(c.severity("duplicate-guid"), Some(Severity::Error));
    assert_eq!(c.severity("missing-guid"), Some(Severity::Warning));
}

#[test]
fn set_unknown_rule_fails() {
    let mut c = LintConfig::default();
    let e = c.set("nope", Setting::Off).unwrap_err();
    assert!(matches!(e, LintError::UnknownRule(ref id) if id == "nope"));
    assert!(e.to_string().contains("nope"));
}

#[test]
fn toml_missing_lint_table_is_default() {
    assert_eq!(
        LintConfig::from_toml_table("").unwrap(),
        LintConfig::default()
    );
}

#[test]
fn toml_overrides_and_other_tables_ignored() {
    let c = LintConfig::from_toml_table(
        r#"
        name = "x"
        [cloud]
        anything = [1, 2]
        [lint]
        state_limit = 2048
        [lint.rules]
        missing-guid = "off"
        duplicate-guid = "info"
        "#,
    )
    .unwrap();
    assert_eq!(c.state_limit, 2048);
    assert_eq!(c.setting("missing-guid"), Setting::Off);
    assert_eq!(c.severity("duplicate-guid"), Some(Severity::Info));
    assert_eq!(c.severity("non-finite-number"), Some(Severity::Error));
}

#[test]
fn toml_errors_name_the_problem() {
    let unknown_rule = LintConfig::from_toml_table("[lint.rules]\nbogus = \"off\"").unwrap_err();
    assert!(unknown_rule.to_string().contains("bogus"));
    let unknown_key = LintConfig::from_toml_table("[lint]\nbogus = 1").unwrap_err();
    assert!(matches!(unknown_key, LintError::UnknownKey(ref k) if k == "bogus"));
    let bad = LintConfig::from_toml_table("[lint.rules]\nmissing-guid = \"loud\"").unwrap_err();
    assert!(bad.to_string().contains("loud"));
    let not_string = LintConfig::from_toml_table("[lint.rules]\nmissing-guid = 3").unwrap_err();
    assert!(not_string.to_string().contains("missing-guid"));
    for text in ["[lint]\nstate_limit = 0", "[lint]\nstate_limit = \"1\""] {
        let e = LintConfig::from_toml_table(text).unwrap_err();
        assert!(e.to_string().contains("state_limit"), "{text}");
    }
    assert!(matches!(
        LintConfig::from_toml_table("[lint"),
        Err(LintError::Toml(_))
    ));
}

// --- engine ---

#[test]
fn severity_override_and_off() {
    let v = json!({ "ObjectStates": [ {}, {} ] });
    let mut c = LintConfig::default();
    c.set("missing-guid", Setting::On(Severity::Error)).unwrap();
    let f = lint(&save(v.clone()), &c);
    assert_eq!(f.len(), 2);
    assert!(f.iter().all(|f| f.severity == Severity::Error));
    c.set("missing-guid", Setting::Off).unwrap();
    assert!(lint(&save(v), &c).is_empty());
}

#[test]
fn findings_are_in_document_order() {
    let f = run(json!({
        "ObjectStates": [
            { "GUID": "a", "ContainedObjects": [ { "Transform": { "posX": 5000 }, "GUID": "b" } ] },
            { "Transform": { "posY": -5000 } },
            { "GUID": "a" }
        ]
    }));
    let got: Vec<_> = f.iter().map(|f| (f.rule, f.path.as_str())).collect();
    assert_eq!(
        got,
        [
            (
                "position-out-of-bounds",
                "ObjectStates[0].ContainedObjects[0]"
            ),
            ("missing-guid", "ObjectStates[1]"),
            ("position-out-of-bounds", "ObjectStates[1]"),
            ("duplicate-guid", "ObjectStates[2]"),
        ]
    );
}

#[test]
fn non_object_entries_do_not_panic() {
    let f = run(json!({
        "ObjectStates": [1, "x", null, [ {"GUID": "z"} ], { "ContainedObjects": [7, "y"], "GUID": "g" }],
        "States": { "a": 1 }
    }));
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn scene_objects_include_contained_and_states() {
    let f = run_rule(
        json!({
            "ObjectStates": [{
                "GUID": "root",
                "ContainedObjects": [ {}, { "GUID": "c", "ContainedObjects": [ {} ] } ],
                "States": { "2": {}, "3": { "ContainedObjects": [ {} ] } }
            }]
        }),
        "missing-guid-nested",
    );
    let paths: Vec<_> = f.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "ObjectStates[0].ContainedObjects[0]",
            "ObjectStates[0].ContainedObjects[1].ContainedObjects[0]",
            "ObjectStates[0].States.2",
            "ObjectStates[0].States.3",
            "ObjectStates[0].States.3.ContainedObjects[0]",
        ]
    );
}

#[test]
fn only_scene_objects_are_checked() {
    // Plain nested objects, a non-root ObjectStates and the root itself are not scene objects.
    let f = run(json!({
        "Other": [ {}, { "Transform": { "posX": 9999 } } ],
        "Nested": { "ObjectStates": [ {} ] },
        "ObjectStates": [ { "GUID": "a", "Transform": { "posX": 1 }, "CustomDeck": { "1": {} } } ]
    }));
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn owner_attribution() {
    let f = run(json!({
        "ObjectStates": [
            { "GUID": "outer", "Nickname": "Box", "ContainedObjects": [
                { "Nickname": "x", "Transform": { "posX": 5000 } },
                { "GUID": "inner", "Nickname": "", "Transform": { "posX": 5000 } }
            ] },
            { "Transform": { "posX": 5000 } }
        ]
    }));
    let pos: Vec<_> = f
        .iter()
        .filter(|f| f.rule == "position-out-of-bounds")
        .map(|f| (f.nickname.as_deref(), f.guid.as_deref()))
        .collect();
    assert_eq!(
        pos,
        [
            (Some("Box"), Some("outer")),
            (None, Some("inner")),
            (None, None)
        ]
    );
}

// --- duplicate-guid ---

#[test]
fn duplicate_guid_positive() {
    let f = run_rule(
        json!({ "ObjectStates": [ obj("a"), obj("b"), obj("a"), { "GUID": "b", "ContainedObjects": [ obj("a") ] } ] }),
        "duplicate-guid",
    );
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].path, "ObjectStates[2]");
    assert!(f[0].message.contains("ObjectStates[0]"));
    assert_eq!(f[0].severity, Severity::Error);
    assert_eq!(f[0].guid.as_deref(), Some("a"));
    assert_eq!(f[0].nickname.as_deref(), Some("na"));
    assert_eq!(f[1].path, "ObjectStates[3]");
    assert!(f[1].message.contains("ObjectStates[1]"));
}

#[test]
fn duplicate_guid_negative() {
    let f = run_rule(
        json!({ "ObjectStates": [
                obj("a"), obj("b"), { "GUID": "", "Nickname": "e" }, { "GUID": "" }, { "GUID": 5 }, { "GUID": 5 },
                { "GUID": "c", "ContainedObjects": [ obj("a"), obj("c") ], "States": { "2": obj("b") } }
            ],
            "Other": { "GUID": "a" } }),
        "duplicate-guid",
    );
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn duplicate_guid_nested_positive() {
    let f = run_rule(
        json!({ "ObjectStates": [
            { "GUID": "a", "ContainedObjects": [ obj("x"), obj("x"), obj("a") ] },
            { "GUID": "b", "States": { "2": obj("x") } },
            obj("x")
        ] }),
        "duplicate-guid-nested",
    );
    let got: Vec<_> = f.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        got,
        [
            "ObjectStates[0].ContainedObjects[1]",
            "ObjectStates[0].ContainedObjects[2]",
            "ObjectStates[1].States.2",
            "ObjectStates[2]",
        ]
    );
    assert!(f[0].message.contains("ObjectStates[0].ContainedObjects[0]"));
    assert!(f[1].message.contains("`ObjectStates[0]`"));
    assert!(f[3].message.contains("ObjectStates[0].ContainedObjects[0]"));
    assert_eq!(f[0].severity, Severity::Info);
}

#[test]
fn duplicate_guid_nested_negative() {
    let f = run_rule(
        json!({ "ObjectStates": [
            { "GUID": "a", "ContainedObjects": [ obj("x"), obj("y"), { "GUID": "" }, { "GUID": "" } ] },
            obj("b"), obj("b")
        ] }),
        "duplicate-guid-nested",
    );
    assert!(f.is_empty(), "{f:?}");
}

// --- missing-guid ---

#[test]
fn missing_guid_positive() {
    let f = run_rule(
        json!({ "ObjectStates": [ {}, { "GUID": "" }, { "GUID": 7 }, { "GUID": null }, obj("ok") ] }),
        "missing-guid",
    );
    let paths: Vec<_> = f.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "ObjectStates[0]",
            "ObjectStates[1]",
            "ObjectStates[2]",
            "ObjectStates[3]"
        ]
    );
    assert_eq!(f[0].severity, Severity::Warning);
}

#[test]
fn missing_guid_negative() {
    let f = run_rule(
        json!({ "ObjectStates": [ obj("a"), { "GUID": "b", "ContainedObjects": [ {} ], "States": { "2": {} } } ] }),
        "missing-guid",
    );
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn missing_guid_nested_positive() {
    let f = run_rule(
        json!({ "ObjectStates": [ { "GUID": "a", "ContainedObjects": [ {}, { "GUID": "" } ], "States": { "2": { "GUID": 1 } } } ] }),
        "missing-guid-nested",
    );
    assert_eq!(f.len(), 3);
    assert_eq!(f[0].severity, Severity::Info);
    assert_eq!(f[0].guid.as_deref(), Some("a"));
}

#[test]
fn missing_guid_nested_negative() {
    let f = run_rule(
        json!({ "ObjectStates": [ {}, { "GUID": "a", "ContainedObjects": [ obj("x") ], "States": { "2": obj("y") } } ] }),
        "missing-guid-nested",
    );
    assert!(f.is_empty(), "{f:?}");
}

// --- position-out-of-bounds ---

#[test]
fn position_out_of_bounds_positive() {
    let f = run_rule(
        json!({ "ObjectStates": [
            { "GUID": "a", "Transform": { "posX": 1001, "posY": -2000.5, "posZ": 3 } },
            { "GUID": "b", "Transform": { "posX": 0, "posY": 0, "posZ": -1000.5 } }
        ] }),
        "position-out-of-bounds",
    );
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].path, "ObjectStates[0]");
    assert!(f[0].message.contains("posY"), "{}", f[0].message);
    assert!(f[1].message.contains("posZ"));
}

#[test]
fn position_out_of_bounds_negative() {
    let f = run_rule(
        json!({ "ObjectStates": [
            { "GUID": "a", "Transform": { "posX": 1000, "posY": -1000.0, "posZ": 99 } },
            { "GUID": "b", "Transform": { "posX": "5000", "posY": null } },
            { "GUID": "c", "Transform": 5 },
            { "GUID": "d", "Transform": { "rotX": 5000 } },
            { "GUID": "e" }
        ] }),
        "position-out-of-bounds",
    );
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn position_handles_exponent_text() {
    let v: Value = serde_json::from_str(
        r#"{"ObjectStates":[{"GUID":"a","Transform":{"posX":1.5E+3,"posY":1E-05}}]}"#,
    )
    .unwrap();
    let f = run_rule(v, "position-out-of-bounds");
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("posX"));
}

// --- non-finite-number ---

#[test]
fn non_finite_number_positive() {
    let v: Value = serde_json::from_str(
        r#"{"ObjectStates":[{"GUID":"a","Nickname":"N","Transform":
            {"posX":1E+999,"posY":"NaN","posZ":"-infinity","rotX":"Infinity","rotY":-1E+999,"rotZ":0}}]}"#,
    )
    .unwrap();
    let f = run(v);
    let nf: Vec<_> = f.iter().filter(|f| f.rule == "non-finite-number").collect();
    let paths: Vec<_> = nf.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "ObjectStates[0].Transform.posX",
            "ObjectStates[0].Transform.posY",
            "ObjectStates[0].Transform.posZ",
            "ObjectStates[0].Transform.rotX",
            "ObjectStates[0].Transform.rotY",
        ]
    );
    assert_eq!(nf[0].severity, Severity::Error);
    assert_eq!(nf[0].guid.as_deref(), Some("a"));
    assert_eq!(nf[0].nickname.as_deref(), Some("N"));
    // Infinite positions are not also reported as out of bounds.
    assert!(f.iter().all(|f| f.rule != "position-out-of-bounds"));
}

#[test]
fn non_finite_number_negative() {
    let f = run_rule(
        json!({ "ObjectStates": [
            { "GUID": "a", "Transform": { "posX": 1.5, "posY": "hello", "posZ": 1e300, "rotX": null } },
            { "GUID": "b", "Nickname": "NaN", "Other": { "x": "NaN" } },
            { "GUID": "c", "Transform": "NaN" }
        ] }),
        "non-finite-number",
    );
    assert!(f.is_empty(), "{f:?}");
}
