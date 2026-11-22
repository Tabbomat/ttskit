//! Tests for rewriting asset URLs in a save.

use serde_json::json;
use ttskit_core::{RewriteOptions, RuleError, Save, UrlRule, UrlRules, parse_map_tsv};

const A: &str = "https://example.com/a.png";
const B: &str = "https://example.org/b.png";
const C: &str = "https://example.com/c.png";
const LEGACY: &str = "http://cloud-3.steamusercontent.com/ugc/42/ABCD/";
const CURRENT: &str = "https://steamusercontent-a.akamaihd.net/ugc/42/ABCD/";

fn rule(old: &str, new: &str, origin: &str) -> UrlRule {
    UrlRule {
        old: old.into(),
        new: new.into(),
        origin: origin.into(),
    }
}

fn rules(pairs: &[(&str, &str)]) -> UrlRules {
    UrlRules::new(
        pairs
            .iter()
            .enumerate()
            .map(|(i, (o, n))| rule(o, n, &format!("line {}", i + 1)))
            .collect(),
    )
    .unwrap()
}

/// A save in the game's format, with URLs at several places.
fn sample() -> Save {
    let value = json!({
        "SaveName": "Test",
        "LuaScript": format!("local img = \"{A}\"\nprint(\"{A}.bak\", '{C}')\n"),
        "XmlUI": format!("<Image image=\"{A}?x=1&amp;y=2\"/><Image image=\"{A}\"/>"),
        "ObjectStates": [
            {
                "GUID": "aaaaaa",
                "Nickname": "Deck",
                "CustomDeck": {"1": {"FaceURL": A, "BackURL": A, "NumWidth": 10, "Scale": 1.0}},
                "LuaScript": format!("x = \"{A}\""),
                "ContainedObjects": [
                    {"GUID": "bbbbbb", "Nickname": "", "ImageURL": C, "Note": A}
                ]
            },
            {"GUID": "cccccc", "Nickname": "Tile", "MeshURL": LEGACY, "Name": A}
        ]
    });
    Save::from_value(value).unwrap()
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .split("\r\n")
        .map(str::to_owned)
        .collect()
}

#[test]
fn diff_shows_only_replaced_strings() {
    let save = sample();
    let before = save.to_vec();
    let (after, report) = save.rewrite_urls(&rules(&[(A, B)]), RewriteOptions::default());
    let after = after.to_vec();
    let (b, a) = (lines(&before), lines(&after));
    assert_eq!(b.len(), a.len());
    let changed: Vec<_> = b.iter().zip(&a).filter(|(x, y)| x != y).collect();
    // FaceURL, BackURL, Note and Name hold A as a whole value: 4 lines.
    assert_eq!(changed.len(), 4, "{changed:#?}");
    for (x, y) in changed {
        assert_eq!(x.replace(A, B), *y);
    }
    assert_eq!(report.values_changed(), 4);
    // The script strings were not touched without in_scripts.
    assert_eq!(report.scripts_changed(), 0);
    assert!(report.scripts_skipped() > 0);
}

#[test]
fn round_trip_gives_original_bytes() {
    let save = sample();
    let original = save.to_vec();
    let opts = RewriteOptions {
        in_scripts: true,
        ..RewriteOptions::default()
    };
    let (there, _) = save.rewrite_urls(&rules(&[(A, B)]), opts);
    assert_ne!(there.to_vec(), original);
    let (back, _) = there.rewrite_urls(&rules(&[(B, A)]), opts);
    assert_eq!(back.to_vec(), original);
}

#[test]
fn rules_do_not_chain_and_can_swap() {
    let save = Save::from_value(json!({"X": A, "Y": B})).unwrap();
    let (out, _) = save.rewrite_urls(&rules(&[(A, B), (B, A)]), RewriteOptions::default());
    assert_eq!(out.value(), &json!({"X": B, "Y": A}));
}

#[test]
fn report_lists_objects_and_unmatched_rules() {
    let (_, report) = sample().rewrite_urls(
        &rules(&[(A, B), ("https://nowhere.example/z.png", B)]),
        RewriteOptions::default(),
    );
    let first = &report.rules[0];
    assert_eq!(first.value_count(), 4);
    let paths: Vec<_> = first.values.iter().map(|t| t.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "ObjectStates[0].CustomDeck.1.FaceURL",
            "ObjectStates[0].CustomDeck.1.BackURL",
            "ObjectStates[0].ContainedObjects[0].Note",
            "ObjectStates[1].Name",
        ]
    );
    assert_eq!(first.values[0].guid.as_deref(), Some("aaaaaa"));
    assert_eq!(first.values[0].nickname.as_deref(), Some("Deck"));
    // Empty nickname is None; the guid is the nearest object's.
    assert_eq!(first.values[2].guid.as_deref(), Some("bbbbbb"));
    assert_eq!(first.values[2].nickname, None);
    let unmatched: Vec<_> = report.unmatched().map(|r| r.rule.origin.as_str()).collect();
    assert_eq!(unmatched, ["line 2"]);
}

#[test]
fn rule_matching_only_scripts_is_not_unmatched() {
    let save = Save::from_value(json!({"LuaScript": format!("u = '{C}'")})).unwrap();
    let (out, report) = save.rewrite_urls(&rules(&[(C, B)]), RewriteOptions::default());
    assert_eq!(report.unmatched().count(), 0);
    assert_eq!(report.rules[0].script_count(), 1);
    assert_eq!(report.scripts_skipped(), 1);
    assert_eq!(out, save);
}

#[test]
fn scripts_are_rewritten_only_with_in_scripts() {
    let opts = RewriteOptions {
        in_scripts: true,
        ..RewriteOptions::default()
    };
    let (out, report) = sample().rewrite_urls(&rules(&[(A, B)]), opts);
    let map = out.map();
    // Whole-token match only: `a.png.bak` is a different URL.
    assert_eq!(
        map["LuaScript"],
        json!(format!("local img = \"{B}\"\nprint(\"{A}.bak\", '{C}')\n"))
    );
    // XML: `&amp;` is `&`, and a URL with a query is a different URL.
    assert_eq!(
        map["XmlUI"],
        json!(format!(
            "<Image image=\"{A}?x=1&amp;y=2\"/><Image image=\"{B}\"/>"
        ))
    );
    assert_eq!(
        map["ObjectStates"][0]["LuaScript"],
        json!(format!("x = \"{B}\""))
    );
    assert_eq!(report.rules[0].script_count(), 3);
    assert_eq!(report.scripts_changed(), 3);
    assert_eq!(report.scripts_skipped(), 0);
    let script_objects: Vec<_> = report.rules[0]
        .scripts
        .iter()
        .map(|t| t.guid.as_deref())
        .collect();
    assert_eq!(script_objects, [None, None, Some("aaaaaa")]);
}

#[test]
fn xml_amp_in_old_and_new_url() {
    let old = "https://example.com/i.png?a=1&b=2";
    let new = "https://example.com/j.png?a=1&b=2";
    let save = Save::from_value(
        json!({"XmlUI": "<Image image=\"https://example.com/i.png?a=1&amp;b=2\"/>"}),
    )
    .unwrap();
    let opts = RewriteOptions {
        in_scripts: true,
        ..RewriteOptions::default()
    };
    let (out, _) = save.rewrite_urls(&rules(&[(old, new)]), opts);
    assert_eq!(
        out.map()["XmlUI"],
        json!("<Image image=\"https://example.com/j.png?a=1&amp;b=2\"/>")
    );
}

#[test]
fn trailing_punctuation_is_not_part_of_a_script_url() {
    let save =
        Save::from_value(json!({"LuaScript": format!("-- see {A}. Also ({A}), {A}")})).unwrap();
    let opts = RewriteOptions {
        in_scripts: true,
        ..RewriteOptions::default()
    };
    let (out, report) = save.rewrite_urls(&rules(&[(A, B)]), opts);
    assert_eq!(
        out.map()["LuaScript"],
        json!(format!("-- see {B}. Also ({B}), {B}"))
    );
    assert_eq!(report.rules[0].script_count(), 3);
}

#[test]
fn duplicate_old_urls() {
    // Identical repeat: merged and reported.
    let set = UrlRules::new(vec![rule(A, B, "line 1"), rule(A, B, "line 4")]).unwrap();
    assert_eq!(set.rules().len(), 1);
    assert_eq!(set.duplicates().len(), 1);
    assert_eq!(set.duplicates()[0].origin, "line 4");
    // Conflict: an error that names both lines.
    let err = UrlRules::new(vec![rule(A, B, "line 1"), rule(A, C, "line 2")]).unwrap_err();
    let RuleError::Conflict {
        first_origin,
        second_origin,
        ..
    } = &err;
    assert_eq!(
        (first_origin.as_str(), second_origin.as_str()),
        ("line 1", "line 2")
    );
}

#[test]
fn normalize_steam_hosts_changes_only_the_prefix() {
    let save = sample();
    let opts = RewriteOptions {
        normalize_steam_hosts: true,
        ..RewriteOptions::default()
    };
    let (out, report) = save.rewrite_urls(&rules(&[]), opts);
    assert_eq!(out.map()["ObjectStates"][1]["MeshURL"], json!(CURRENT));
    let n = report.normalize.as_ref().unwrap();
    assert_eq!(n.value_count(), 1);
    assert_eq!(report.values_changed(), 1);
    // Off by default.
    let (same, report) = save.rewrite_urls(&rules(&[]), RewriteOptions::default());
    assert_eq!(same, save);
    assert!(report.normalize.is_none());
}

#[test]
fn explicit_rule_wins_over_normalize() {
    let save = Save::from_value(json!({"U": LEGACY})).unwrap();
    let opts = RewriteOptions {
        normalize_steam_hosts: true,
        ..RewriteOptions::default()
    };
    let (out, report) = save.rewrite_urls(&rules(&[(LEGACY, B)]), opts);
    assert_eq!(out.value(), &json!({"U": B}));
    assert_eq!(report.normalize.unwrap().value_count(), 0);
}

#[test]
fn normalize_in_scripts() {
    let save = Save::from_value(json!({"LuaScript": format!("u = \"{LEGACY}\"")})).unwrap();
    let opts = RewriteOptions {
        normalize_steam_hosts: true,
        in_scripts: true,
    };
    let (out, _) = save.rewrite_urls(&rules(&[]), opts);
    assert_eq!(out.map()["LuaScript"], json!(format!("u = \"{CURRENT}\"")));
}

#[test]
fn only_strings_change_numbers_and_order_stay() {
    let text = "{\r\n  \"Z\": 1.0,\r\n  \"U\": \"https://example.com/a.png\",\r\n  \"N\": 1E-05,\r\n  \"A\": -0.0\r\n}";
    let save = Save::from_slice(text.as_bytes()).unwrap();
    let (out, _) = save.rewrite_urls(&rules(&[(A, B)]), RewriteOptions::default());
    assert_eq!(String::from_utf8(out.to_vec()).unwrap(), text.replace(A, B));
}

#[test]
fn tsv_parsing() {
    let text = format!("\u{feff}# comment\r\n\r\n{A}\t{B}\r\n{C}\t{B}\n");
    let parsed = parse_map_tsv(&text).unwrap();
    assert_eq!(parsed, vec![rule(A, B, "line 3"), rule(C, B, "line 4")]);
}

#[test]
fn tsv_problems_are_all_reported_with_line_numbers() {
    let text =
        format!("{A}\t{B}\nno tab here\n{A}\t{B}\t{C}\nftp://x/y\t{B}\n{A}\tnot a url\n\t{B}\n");
    let problems = parse_map_tsv(&text).unwrap_err();
    let lines: Vec<_> = problems.iter().map(|p| p.line).collect();
    assert_eq!(lines, [2, 3, 4, 5, 6]);
}
