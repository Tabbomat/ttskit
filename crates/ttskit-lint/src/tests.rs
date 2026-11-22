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
        "ObjectStates": [ { "GUID": "a", "Transform": { "posX": 1 }, "CustomDeck": { "1": { "NumWidth": 1, "NumHeight": 1 } } } ]
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

// --- URL rules ---

fn url_save(fields: Value) -> Value {
    let mut o = json!({ "GUID": "g1", "Nickname": "Card" });
    for (k, v) in fields.as_object().unwrap() {
        o[k] = v.clone();
    }
    json!({ "ObjectStates": [o] })
}

fn rules_of(f: &[Finding]) -> Vec<&str> {
    f.iter().map(|f| f.rule).collect()
}

#[test]
fn empty_url_positive() {
    let f = run_rule(
        json!({
            "CustomUIAssets": [ { "Name": "a", "URL": " " } ],
            "ObjectStates": [ {
                "GUID": "g",
                "CustomDeck": { "1": { "FaceURL": "", "BackURL": "  	", "NumWidth": 1 } },
                "CustomImage": { "ImageURL": "" },
                "CustomMesh": { "MeshURL": "" },
                "CustomAssetbundle": { "AssetbundleURL": "" },
                "CustomPDF": { "PDFUrl": "" }
            } ]
        }),
        "empty-url",
    );
    let paths: Vec<_> = f.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "CustomUIAssets[0].URL",
            "ObjectStates[0].CustomDeck.1.FaceURL",
            "ObjectStates[0].CustomDeck.1.BackURL",
            "ObjectStates[0].CustomImage.ImageURL",
            "ObjectStates[0].CustomMesh.MeshURL",
            "ObjectStates[0].CustomAssetbundle.AssetbundleURL",
            "ObjectStates[0].CustomPDF.PDFUrl",
        ]
    );
    assert!(f[1].message.contains("FaceURL"));
    assert_eq!(f[1].guid.as_deref(), Some("g"));
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn empty_url_negative() {
    let f = run_rule(
        json!({
            "Other": { "URL": "" },
            "ObjectStates": [ {
                "GUID": "g",
                "Custom": { "DiffuseURL": "", "NormalURL": "", "ColliderURL": "", "ImageSecondaryURL": "",
                            "AssetbundleSecondaryURL": "", "LutURL": "", "FaceURL": "https://a.b/c.png",
                            "Name": "", "Count": 0 }
            } ]
        }),
        "empty-url",
    );
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn invalid_url_positive() {
    for bad in [
        "ftp://example.com/a.png",
        "data:image/png;base64,AAAA",
        "example.com/a.png",
        "https://",
        "https:///a.png",
        "https://user@:80/a",
        "https://exa mple.com/a.png",
        "https://example.com/a
b",
        " https://example.com/a.png",
    ] {
        let f = run(url_save(json!({ "FaceURL": bad })));
        assert_eq!(rules_of(&f), ["invalid-url"], "{bad}");
        assert!(f[0].message.contains("FaceURL"));
    }
}

#[test]
fn invalid_url_negative() {
    for good in [
        "https://example.com/a.png",
        "HTTPS://Example.com:8080/a?b#c",
        "https://user:pw@example.com/a",
        "https://[::1]:80/a",
        "https://i.imgur.com/x.png",
    ] {
        let f = run(url_save(json!({ "FaceURL": good })));
        assert!(f.is_empty(), "{good}: {f:?}");
    }
    // Local paths are reported by local-url, not here.
    let f = run(url_save(json!({ "FaceURL": r"C:\x\a.png" })));
    assert_eq!(rules_of(&f), ["local-url"]);
}

#[test]
fn insecure_url_positive() {
    for v in ["http://example.com/a.png", "HTTP://example.com/a.png"] {
        let f = run(url_save(json!({ "ImageURL": v })));
        assert_eq!(rules_of(&f), ["insecure-url"], "{v}");
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].path, "ObjectStates[0].ImageURL");
    }
}

#[test]
fn insecure_url_negative() {
    let f = run(url_save(
        json!({ "ImageURL": "https://example.com/http://x", "Note": "http://example.com" }),
    ));
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn local_url_positive() {
    for v in [
        "file:///x/a.png",
        "FILE:///x/a.png",
        r"C:\Users\x\a.png",
        "d:/images/a.png",
        r"\\host\share\a.png",
        "/home/x/a.png",
        "~/a.png",
        r"Documents\Tabletop Simulator\a.png",
        "x/Tabletop Simulator/a.png",
        r"Mods\Images\a.png",
        "mods/models/a.obj",
        "MODS/ASSETBUNDLES/a",
        "Mods/PDF/a.pdf",
    ] {
        let f = run(url_save(json!({ "FaceURL": v })));
        assert_eq!(rules_of(&f), ["local-url"], "{v}");
        assert_eq!(f[0].severity, Severity::Error);
    }
}

#[test]
fn local_url_negative() {
    for v in [
        "https://example.com/a.png",
        "Tabletop Simulator",
        "https://example.com/Mods/Other/a.png",
        "x:y",
    ] {
        let f = run_rule(url_save(json!({ "FaceURL": v })), "local-url");
        assert!(f.is_empty(), "{v}: {f:?}");
    }
}

#[test]
fn url_fields_are_found_once_in_nested_objects_and_deck_sheets() {
    let f = run_rule(
        json!({
            "ObjectStates": [ {
                "GUID": "deck",
                "CustomDeck": { "1": { "FaceURL": "", "BackURL": "https://a.b/b.png" },
                                "2": { "FaceURL": "", "BackURL": "" } },
                "ContainedObjects": [ {
                    "GUID": "card",
                    "CustomImage": { "ImageURL": "" },
                    "States": { "2": { "GUID": "s", "CustomMesh": { "MeshURL": "" } } },
                    "ContainedObjects": [ { "GUID": "deep", "CustomPDF": { "PDFUrl": "" } } ]
                } ]
            } ]
        }),
        "empty-url",
    );
    let got: Vec<_> = f
        .iter()
        .map(|f| (f.path.as_str(), f.guid.as_deref().unwrap()))
        .collect();
    assert_eq!(
        got,
        [
            ("ObjectStates[0].CustomDeck.1.FaceURL", "deck"),
            ("ObjectStates[0].CustomDeck.2.FaceURL", "deck"),
            ("ObjectStates[0].CustomDeck.2.BackURL", "deck"),
            (
                "ObjectStates[0].ContainedObjects[0].CustomImage.ImageURL",
                "card"
            ),
            (
                "ObjectStates[0].ContainedObjects[0].States.2.CustomMesh.MeshURL",
                "s"
            ),
            (
                "ObjectStates[0].ContainedObjects[0].ContainedObjects[0].CustomPDF.PDFUrl",
                "deep"
            ),
        ]
    );
}

#[test]
fn url_rules_are_independent() {
    let v = json!({ "ObjectStates": [ {
        "GUID": "g",
        "A": { "FaceURL": "" },
        "B": { "FaceURL": "http://a.b/c" },
        "C": { "FaceURL": "ftp://a.b/c" },
        "D": { "FaceURL": "file:///a" }
    } ] });
    let all = rules_of(&run(v.clone())).len();
    assert_eq!(all, 4);
    for id in ["empty-url", "insecure-url", "invalid-url", "local-url"] {
        let mut c = LintConfig::default();
        c.set(id, Setting::Off).unwrap();
        let f = lint(&save(v.clone()), &c);
        assert_eq!(f.len(), 3, "{id}");
        assert!(f.iter().all(|f| f.rule != id));
    }
    let mut c = LintConfig::default();
    for id in ["empty-url", "insecure-url", "invalid-url", "local-url"] {
        c.set(id, Setting::Off).unwrap();
    }
    assert!(lint(&save(v), &c).is_empty());
}

#[test]
fn legacy_steam_url_positive() {
    for v in [
        "http://cloud-3.steamusercontent.com/ugc/1/ABC/",
        "HTTP://Cloud-3.SteamUserContent.com:80/ugc/1/",
    ] {
        let f = run(url_save(json!({ "MeshURL": v })));
        assert_eq!(rules_of(&f), ["legacy-steam-url"], "{v}");
        assert_eq!(f[0].severity, Severity::Info);
    }
}

#[test]
fn legacy_steam_url_negative() {
    let f = run(url_save(json!({
        "A": { "MeshURL": "https://cloud-3.steamusercontent.com/ugc/1/" },
        "B": { "MeshURL": "http://cloud-2.steamusercontent.com/ugc/1/" },
        "C": { "MeshURL": "http://example.com/cloud-3.steamusercontent.com" }
    })));
    assert_eq!(rules_of(&f), ["insecure-url", "insecure-url"]);
}

#[test]
fn placeholders_are_skipped() {
    for v in [
        "<Scythe>scythetablemain",
        "<Scythe>ScytheEncounterCards1.png",
        "{de}<Scythe>a{en}<Scythe>b",
    ] {
        let f = run(url_save(json!({ "FaceURL": v })));
        assert!(f.is_empty(), "{v}: {f:?}");
    }
    // Not placeholders.
    for v in ["<>x", "< a>x", "<a b>x", "<a"] {
        let f = run(url_save(json!({ "FaceURL": v })));
        assert_eq!(rules_of(&f), ["invalid-url"], "{v}");
    }
}

#[test]
fn localized_value_with_one_bad_segment() {
    let f = run(url_save(
        json!({ "FaceURL": "{de}https://a.b/x.png{en}ftp://a.b/x.png{fr}https://a.b/y.png" }),
    ));
    assert_eq!(rules_of(&f), ["invalid-url"]);
    assert!(f[0].message.contains("{en}"), "{}", f[0].message);
    assert!(!f[0].message.contains("{de}"));
}

#[test]
fn localized_all_good_and_braces_in_urls() {
    for v in [
        "{de}https://a.b/x.png{en}https://a.b/y.png",
        "{pt}https://a.b/x.png",
        "https://a.b/x?q={de}",
        "https://a.b/{en}x.png",
    ] {
        let f = run(url_save(json!({ "FaceURL": v })));
        assert!(f.is_empty(), "{v}: {f:?}");
    }
    // A brace group that is not a marker does not make a value localized.
    let f = run(url_save(json!({ "FaceURL": "{x}https://a.b/x.png" })));
    assert_eq!(rules_of(&f), ["invalid-url"]);
}

#[test]
fn localized_segments_use_every_rule() {
    let f = run(url_save(json!({
        "FaceURL": r"{de}http://a.b/x.png{en}http://cloud-3.steamusercontent.com/ugc/1/{fr}{es}C:\x.png{it}https://ok.b/x"
    })));
    assert_eq!(
        rules_of(&f),
        ["insecure-url", "legacy-steam-url", "empty-url", "local-url"]
    );
    assert!(f[0].message.contains("{de}"));
    assert!(f[1].message.contains("{en}"));
    assert!(f[2].message.contains("{fr}"));
    assert!(f[3].message.contains("{es}"));
    // An empty segment of an optional key is ignored.
    let f = run(url_save(json!({ "DiffuseURL": "{de}{en}https://a.b/x" })));
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn verifycache_prefix_is_stripped() {
    for v in [
        "{verifycache}https://a.b/x.unity3d",
        "{VerifyCache}https://a.b/x.unity3d",
        "{verifycache}{de}https://a.b/x{en}https://a.b/y",
        "{de}{verifycache}https://a.b/x{en}{verifycache}https://a.b/y",
    ] {
        let f = run(url_save(json!({ "AssetbundleURL": v })));
        assert!(f.is_empty(), "{v}: {f:?}");
    }
    // The remainder is still classified.
    let f = run(url_save(
        json!({ "AssetbundleURL": "{verifycache}{de}http://a.b/x{en}ftp://a.b/y" }),
    ));
    assert_eq!(rules_of(&f), ["insecure-url", "invalid-url"]);
    let f = run(url_save(json!({ "AssetbundleURL": "{verifycache}" })));
    assert_eq!(rules_of(&f), ["empty-url"]);
    let f = run(url_save(json!({ "DiffuseURL": "{verifycache}" })));
    assert!(f.is_empty());
}

#[test]
fn verifycache_only_counts_at_the_start() {
    let f = run(url_save(
        json!({ "AssetbundleURL": "https://a.b/{verifycache}x", "FaceURL": "x{verifycache}https://a.b/y" }),
    ));
    assert_eq!(rules_of(&f), ["invalid-url"]);
}

// --- script syntax rules ---

fn script_save(fields: Value) -> Value {
    url_save(fields)
}

#[test]
fn lua_syntax_positive() {
    let f = run(script_save(
        json!({ "LuaScript": "local a = 1\nlocal b = = 2\n" }),
    ));
    assert_eq!(rules_of(&f), ["lua-syntax"]);
    assert!(f[0].message.contains("at 2:"), "{}", f[0].message);
    assert_eq!(f[0].path, "ObjectStates[0].LuaScript");
    assert_eq!(f[0].guid.as_deref(), Some("g1"));
    assert_eq!(f[0].severity, Severity::Warning);
    let f = run(script_save(json!({ "LuaScript": "x += 1" })));
    assert_eq!(rules_of(&f), ["lua-syntax"]);
    let f = run(script_save(json!({ "LuaScript": "local x = 7 // 2" })));
    assert_eq!(rules_of(&f), ["lua-syntax"]);
}

#[test]
fn lua_syntax_negative() {
    for src in [
        "",
        "  \n",
        "function onLoad()\n  print('hi')\nend\n",
        "for i = 1, 3 do\n  if i == 2 then goto continue end\n  print(i)\n  ::continue::\nend",
        "#include Util\nlocal x = 1\n",
        "-- comment\r\n#include <Util>\r\nprint(1)",
        "local s = [[long\nstring]]",
    ] {
        let f = run(script_save(json!({ "LuaScript": src })));
        assert!(f.is_empty(), "{src:?}: {f:?}");
    }
}

#[test]
fn lua_include_lines_keep_line_numbers() {
    let f = run(script_save(
        json!({ "LuaScript": "#include A\n#include B\nlocal x = = 1" }),
    ));
    assert!(f[0].message.contains("at 3:"), "{}", f[0].message);
}

#[test]
fn xml_malformed_positive() {
    let f = run(script_save(
        json!({ "XmlUI": "<Panel>\n  <Text>hi</Text>\n" }),
    ));
    assert_eq!(rules_of(&f), ["xml-malformed"]);
    assert!(f[0].message.contains("at "), "{}", f[0].message);
    let f = run(script_save(
        json!({ "XmlUI": "<Defaults>\n<Text>a</Panel>\n</Defaults>" }),
    ));
    assert_eq!(rules_of(&f), ["xml-malformed"]);
    assert!(f[0].message.contains("at 2:"), "{}", f[0].message);
}

#[test]
fn xml_malformed_negative() {
    for src in [
        "",
        " ",
        "<Panel><Text>a &amp; b</Text></Panel>",
        "<Defaults><Text color=\"#fff\"/></Defaults>\n<Panel id=\"p\">\n<Button onClick=\"f\">x</Button></Panel>",
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Panel/>\n<Panel/>",
        "<!-- c --><Panel/>",
    ] {
        let f = run(script_save(json!({ "XmlUI": src })));
        assert!(f.is_empty(), "{src:?}: {f:?}");
    }
}

#[test]
fn xml_error_line_is_relative_to_the_original_text() {
    let f = run(script_save(
        json!({ "XmlUI": "<?xml version=\"1.0\"?>\n<A/>\n<B>\n</C>" }),
    ));
    assert!(f[0].message.contains("at 4:"), "{}", f[0].message);
}

#[test]
fn nested_scripts_are_reported_once_and_cached_results_agree() {
    let bad = "local = 1";
    let f = run(json!({
        "LuaScript": bad,
        "XmlUI": "<a>",
        "ObjectStates": [ {
            "GUID": "a", "LuaScript": bad, "XmlUI": "<a>",
            "ContainedObjects": [ { "GUID": "b", "LuaScript": bad } ],
            "States": { "2": { "GUID": "c", "LuaScript": "x = 1", "XmlUI": "<a/>" } }
        }, { "GUID": "d", "LuaScript": bad } ]
    }));
    let got: Vec<_> = f.iter().map(|f| (f.rule, f.path.as_str())).collect();
    assert_eq!(
        got,
        [
            ("lua-syntax", "LuaScript"),
            ("xml-malformed", "XmlUI"),
            ("lua-syntax", "ObjectStates[0].LuaScript"),
            ("xml-malformed", "ObjectStates[0].XmlUI"),
            (
                "lua-syntax",
                "ObjectStates[0].ContainedObjects[0].LuaScript"
            ),
            ("lua-syntax", "ObjectStates[1].LuaScript"),
        ]
    );
    let tail = |m: &str| m.split(" at ").nth(1).unwrap().to_owned();
    assert_eq!(tail(&f[0].message), tail(&f[2].message));
    assert_eq!(tail(&f[0].message), tail(&f[5].message));
}

#[test]
fn script_rules_are_independent_and_can_be_off() {
    let v = script_save(json!({ "LuaScript": "x = = 1", "XmlUI": "<a>" }));
    for (off, left) in [
        ("lua-syntax", "xml-malformed"),
        ("xml-malformed", "lua-syntax"),
    ] {
        let mut c = LintConfig::default();
        c.set(off, Setting::Off).unwrap();
        let f = lint(&save(v.clone()), &c);
        assert_eq!(rules_of(&f), [left]);
    }
    let mut c = LintConfig::default();
    c.set("lua-syntax", Setting::Off).unwrap();
    c.set("xml-malformed", Setting::Off).unwrap();
    assert!(
        lint(&save(v), &c)
            .iter()
            .all(|f| f.rule != "lua-syntax" && f.rule != "xml-malformed")
    );
}

// --- script state and deck rules ---

fn with_limit(limit: usize) -> LintConfig {
    let mut c = LintConfig::default();
    c.state_limit = limit;
    c
}

#[test]
fn large_script_state_positive_and_limit_edge() {
    let v = json!({
        "LuaScriptState": "12345",
        "ObjectStates": [ { "GUID": "a", "LuaScriptState": "123456" } ]
    });
    let f = lint(&save(v.clone()), &with_limit(5));
    let got: Vec<_> = f.iter().map(|f| (f.rule, f.path.as_str())).collect();
    // Exactly at the limit is fine; one byte over is reported.
    assert_eq!(
        got,
        [("large-script-state", "ObjectStates[0].LuaScriptState")]
    );
    assert!(f[0].message.contains('6') && f[0].message.contains('5'));
    assert_eq!(f[0].guid.as_deref(), Some("a"));
    // Bytes, not characters; the root counts too.
    let f = lint(
        &save(json!({ "LuaScriptState": "\u{e4}\u{e4}\u{e4}" })),
        &with_limit(5),
    );
    assert_eq!(f[0].path, "LuaScriptState");
}

#[test]
fn large_script_state_negative() {
    let v = json!({ "ObjectStates": [ { "GUID": "a", "LuaScriptState": "x".repeat(1000) },
        { "GUID": "b", "LuaScriptState": 5 }, { "GUID": "c" } ] });
    assert!(lint(&save(v.clone()), &LintConfig::default()).is_empty());
    assert!(lint(&save(v), &with_limit(1000)).is_empty());
}

#[test]
fn large_script_state_uses_the_toml_limit() {
    let c = LintConfig::from_toml_table("[lint]\nstate_limit = 3").unwrap();
    let f = lint(&save(json!({ "LuaScriptState": "abcd" })), &c);
    assert_eq!(rules_of(&f), ["large-script-state"]);
}

#[test]
fn script_state_not_json_positive() {
    let f = run(json!({
        "ObjectStates": [ { "GUID": "a", "LuaScript": "local s = JSON.decode(self.script_state)",
                            "LuaScriptState": "{\"a\": 1,\n oops}" } ],
        "LuaScript": "JSON.decode(x)", "LuaScriptState": "not json"
    }));
    let got: Vec<_> = f.iter().map(|f| (f.rule, f.path.as_str())).collect();
    assert_eq!(
        got,
        [
            ("script-state-not-json", "LuaScriptState"),
            ("script-state-not-json", "ObjectStates[0].LuaScriptState"),
        ]
    );
    assert!(f[1].message.contains("line 2"), "{}", f[1].message);
}

#[test]
fn script_state_not_json_negative() {
    let f = run(json!({ "ObjectStates": [
        { "GUID": "a", "LuaScript": "JSON.decode(s)", "LuaScriptState": "{\"a\": [1, 2.50, 1E+400, null]}" },
        { "GUID": "b", "LuaScript": "JSON.decode(s)", "LuaScriptState": "  " },
        { "GUID": "c", "LuaScript": "JSON.decode(s)", "LuaScriptState": "" },
        { "GUID": "d", "LuaScript": "print(1)", "LuaScriptState": "plain text" },
        { "GUID": "e", "LuaScriptState": "plain text" },
        { "GUID": "f", "LuaScript": "JSON.decode(s)" }
    ] }));
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn deck_size_positive() {
    let f = run_rule(
        json!({ "ObjectStates": [ { "GUID": "a", "CustomDeck": {
            "1": { "NumWidth": 11, "NumHeight": 7 },
            "2": { "NumWidth": 10, "NumHeight": 8 },
            "3": { "NumWidth": 0, "NumHeight": 1 },
            "4": { "NumHeight": 1 },
            "5": { "NumWidth": 2.5, "NumHeight": "3" },
            "6": 7
        } } ] }),
        "deck-size",
    );
    let paths: Vec<_> = f.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "ObjectStates[0].CustomDeck.1",
            "ObjectStates[0].CustomDeck.2",
            "ObjectStates[0].CustomDeck.3",
            "ObjectStates[0].CustomDeck.4",
            "ObjectStates[0].CustomDeck.5",
            "ObjectStates[0].CustomDeck.6",
        ]
    );
    assert!(f[0].message.contains("NumWidth") && !f[0].message.contains("NumHeight"));
    assert!(f[3].message.contains("NumWidth is missing"));
    assert!(f[4].message.contains("NumWidth") && f[4].message.contains("NumHeight"));
    assert_eq!(f[0].severity, Severity::Warning);
}

#[test]
fn deck_size_negative() {
    let f = run_rule(
        json!({ "ObjectStates": [ { "GUID": "a", "CustomDeck": {
            "1": { "NumWidth": 10, "NumHeight": 7 },
            "2": { "NumWidth": 1, "NumHeight": 1 }
        } }, { "GUID": "b", "CustomDeck": 5 }, { "GUID": "c", "Other": { "NumWidth": 99 } } ] }),
        "deck-size",
    );
    assert!(f.is_empty(), "{f:?}");
}

fn deck_obj(extra: Value) -> Value {
    let mut o = json!({ "GUID": "d", "CustomDeck": {
        "1": { "NumWidth": 2, "NumHeight": 2 },
        "12": { "NumWidth": 10, "NumHeight": 7 },
        "3": { "NumWidth": 99, "NumHeight": 1 }
    } });
    for (k, v) in extra.as_object().unwrap() {
        o[k] = v.clone();
    }
    json!({ "ObjectStates": [o] })
}

#[test]
fn deck_id_out_of_range_positive() {
    let f = run_rule(
        deck_obj(
            json!({ "DeckIDs": [100, 103, 104, 200, -1, 1.5, "x", 1269, 1270], "CardID": -5 }),
        ),
        "deck-id-out-of-range",
    );
    let paths: Vec<_> = f.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "ObjectStates[0].CardID",
            "ObjectStates[0].DeckIDs[2]",
            "ObjectStates[0].DeckIDs[3]",
            "ObjectStates[0].DeckIDs[4]",
            "ObjectStates[0].DeckIDs[5]",
            "ObjectStates[0].DeckIDs[6]",
            "ObjectStates[0].DeckIDs[8]",
        ]
    );
    assert!(f[0].message.contains("negative"));
    assert!(f[1].message.contains("card 4 of sheet 1"));
    assert!(f[2].message.contains("sheet 2"));
    assert!(f[3].message.contains("negative"));
    assert_eq!(f[0].severity, Severity::Error);
    assert_eq!(f[0].guid.as_deref(), Some("d"));
}

#[test]
fn deck_id_out_of_range_negative() {
    let f = run_rule(
        json!({ "ObjectStates": [
            { "GUID": "a", "CustomDeck": { "1": { "NumWidth": 2, "NumHeight": 2 }, "12": { "NumWidth": 10, "NumHeight": 7 } },
              "DeckIDs": [100, 101, 102, 103, 1200, 1269], "CardID": 4711 },
            { "GUID": "b", "DeckIDs": [999, 5] },
            { "GUID": "c", "CustomDeck": { "1": { "NumWidth": 2, "NumHeight": 2 } }, "CardID": "text" },
            { "GUID": "d", "CustomDeck": { "3": { "NumWidth": 99, "NumHeight": 1 } }, "DeckIDs": [399, 350] }
        ] }),
        "deck-id-out-of-range",
    );
    // Invalid grids are left to deck-size; a deck without CustomDeck is ignored.
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn deck_id_findings_are_capped() {
    let ids: Vec<i64> = (0..50).map(|_| 9999).collect();
    let f = run_rule(deck_obj(json!({ "DeckIDs": ids })), "deck-id-out-of-range");
    assert_eq!(f.len(), 21);
    assert!(f[20].message.contains("30 more"), "{}", f[20].message);
    assert_eq!(f[20].path, "ObjectStates[0].DeckIDs");
}

#[test]
fn deck_rules_see_nested_objects_once_and_can_be_off() {
    let v = json!({ "ObjectStates": [ { "GUID": "deck", "ContainedObjects": [
        { "GUID": "card", "CardID": 500, "CustomDeck": { "5": { "NumWidth": 0, "NumHeight": 1 }, "6": { "NumWidth": 1, "NumHeight": 1 } } }
    ] } ] });
    let f = run(v.clone());
    let got: Vec<_> = f.iter().map(|f| (f.rule, f.path.as_str())).collect();
    assert_eq!(
        got,
        [(
            "deck-size",
            "ObjectStates[0].ContainedObjects[0].CustomDeck.5"
        )]
    );
    let mut c = LintConfig::default();
    c.set("deck-size", Setting::Off).unwrap();
    assert!(lint(&save(v), &c).is_empty());
}
