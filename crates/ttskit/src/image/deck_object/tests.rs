use super::*;
use serde_json::Value;
use ttskit_image::{CardDescription, SheetDescription};

fn sheet(file: &str, columns: u32, rows: u32, cards: u32, first: u32) -> SheetDescription {
    SheetDescription {
        file: file.to_owned(),
        columns,
        rows,
        width: columns * 10,
        height: rows * 10,
        card_count: cards,
        hidden_slot: Some(columns * rows - 1),
        cards: (0..cards)
            .map(|k| CardDescription {
                index: first + k,
                slot: k,
                column: k % columns,
                row: k / columns,
                source: format!("cards/card{:02}.png", first + k),
            })
            .collect(),
    }
}

fn description(sheets: Vec<SheetDescription>, back_is_hidden: bool) -> Description {
    Description {
        format_version: 1,
        name: "deck".to_owned(),
        card_width: 10,
        card_height: 10,
        back_is_hidden,
        sheets,
    }
}

fn urls(names: &[&str]) -> Vec<String> {
    names
        .iter()
        .map(|n| format!("https://example.invalid/{n}/"))
        .collect()
}

fn build(d: &Description, faces: &[&str], backs: &[&str], unique: bool) -> Result<Value, String> {
    build_object(&ObjectInput {
        description: d,
        face_urls: &urls(faces),
        back_urls: &urls(backs),
        unique_backs: unique,
        meta: &Meta::default(),
        nicknames_from_files: false,
    })
}

fn deck(value: &Value) -> &Value {
    &value["ObjectStates"][0]
}

fn ids(o: &Value) -> Vec<u64> {
    o["DeckIDs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_u64().unwrap())
        .collect()
}

/// The keys of every object in `value`, in order, as paths. Arrays are
/// represented by their first element.
fn shape(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let p = format!("{path}/{k}");
                out.push(p.clone());
                shape(v, &p, out);
            }
        }
        Value::Array(items) => {
            if let Some(first) = items.first() {
                shape(first, &format!("{path}[]"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn field_set_and_order_match_a_deck_made_in_the_game() {
    let reference: Value =
        serde_json::from_str(include_str!("../../../../../tests/fixtures/game_deck.json")).unwrap();
    let d = description(vec![sheet("deck.png", 10, 7, 3, 0)], false);
    let ours = build(&d, &["f"], &["b"], false).unwrap();
    let (mut a, mut b) = (Vec::new(), Vec::new());
    shape(&reference, "", &mut a);
    shape(&ours, "", &mut b);
    assert_eq!(a, b);
    // Plain fields have the same defaults.
    let (r, o) = (deck(&reference), deck(&ours));
    for key in [
        "Name",
        "Description",
        "GMNotes",
        "Locked",
        "Grid",
        "Snap",
        "Hands",
        "SidewaysCard",
        "HideWhenFaceDown",
        "DeckIDs",
    ] {
        assert_eq!(r[key], o[key], "{key}");
    }
    let (rc, oc) = (&r["CustomDeck"]["1"], &o["CustomDeck"]["1"]);
    for key in [
        "NumWidth",
        "NumHeight",
        "BackIsHidden",
        "UniqueBack",
        "Type",
    ] {
        assert_eq!(rc[key], oc[key], "{key}");
    }
    let (rc, oc) = (&r["ContainedObjects"][0], &o["ContainedObjects"][0]);
    for key in ["Name", "CardID", "Hands", "SidewaysCard", "Tooltip"] {
        assert_eq!(rc[key], oc[key], "{key}");
    }
}

#[test]
fn one_sheet() {
    let d = description(vec![sheet("deck.png", 10, 7, 69, 0)], false);
    let v = build(&d, &["face"], &["back"], false).unwrap();
    let o = deck(&v);
    assert_eq!(o["Name"], "DeckCustom");
    assert_eq!(ids(o), (100..169).collect::<Vec<u64>>());
    let cards = o["ContainedObjects"].as_array().unwrap();
    assert_eq!(cards.len(), 69);
    assert_eq!(cards[68]["CardID"], 168);
    assert_eq!(cards[0]["Name"], "Card");
    let entry = &o["CustomDeck"]["1"];
    assert_eq!(entry["FaceURL"], "https://example.invalid/face/");
    assert_eq!(entry["BackURL"], "https://example.invalid/back/");
    assert_eq!(entry["NumWidth"], 10);
    assert_eq!(entry["NumHeight"], 7);
    assert_eq!(entry["BackIsHidden"], false);
    assert_eq!(entry["UniqueBack"], false);
    assert_eq!(o["CustomDeck"].as_object().unwrap().len(), 1);
}

#[test]
fn several_sheets_get_several_custom_deck_entries() {
    let d = description(
        vec![
            sheet("deck_1.png", 10, 7, 69, 0),
            sheet("deck_2.png", 2, 2, 3, 69),
        ],
        true,
    );
    let v = build(&d, &["f1", "f2"], &["b"], false).unwrap();
    let o = deck(&v);
    let ids = ids(o);
    assert_eq!(ids.len(), 72);
    assert_eq!(&ids[68..], &[168, 200, 201, 202]);
    let keys: Vec<&String> = o["CustomDeck"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["1", "2"]);
    let second = &o["CustomDeck"]["2"];
    assert_eq!(second["FaceURL"], "https://example.invalid/f2/");
    assert_eq!(second["BackURL"], "https://example.invalid/b/");
    assert_eq!(second["BackIsHidden"], true);
    assert_eq!(second["NumWidth"], 2);
}

#[test]
fn unique_backs_use_one_back_per_sheet() {
    let d = description(
        vec![sheet("a.png", 2, 2, 3, 0), sheet("b.png", 2, 2, 1, 3)],
        false,
    );
    let v = build(&d, &["f1", "f2"], &["b1", "b2"], true).unwrap();
    let c = &deck(&v)["CustomDeck"];
    assert_eq!(c["1"]["BackURL"], "https://example.invalid/b1/");
    assert_eq!(c["2"]["BackURL"], "https://example.invalid/b2/");
    assert_eq!(c["1"]["UniqueBack"], true);
    assert!(build(&d, &["f1", "f2"], &["b1"], true).is_err());
    assert!(build(&d, &["f1", "f2"], &["b1", "b2"], false).is_err());
    assert!(build(&d, &["f1"], &["b1"], false).is_err());
}

#[test]
fn a_single_card_is_a_card_object() {
    let d = description(vec![sheet("deck.png", 2, 2, 1, 0)], false);
    let v = build(&d, &["f"], &["b"], false).unwrap();
    let o = deck(&v);
    assert_eq!(o["Name"], "CardCustom");
    assert_eq!(o["CardID"], 100);
    assert_eq!(
        o["CustomDeck"]["1"]["FaceURL"],
        "https://example.invalid/f/"
    );
    assert!(o.get("DeckIDs").is_none());
    assert_eq!(o["ContainedObjects"], serde_json::json!([]));
}

#[test]
fn output_is_deterministic_and_guids_are_unique() {
    let d = description(vec![sheet("deck.png", 10, 7, 69, 0)], false);
    let a = build(&d, &["f"], &["b"], false).unwrap();
    let b = build(&d, &["f"], &["b"], false).unwrap();
    assert_eq!(a, b);
    let mut guids = vec![deck(&a)["GUID"].as_str().unwrap().to_owned()];
    guids.extend(
        deck(&a)["ContainedObjects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["GUID"].as_str().unwrap().to_owned()),
    );
    assert!(guids.iter().all(|g| {
        g.len() == 6
            && g.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    }));
    let set: BTreeSet<&String> = guids.iter().collect();
    assert_eq!(set.len(), guids.len());
    // A different deck gets a different GUID.
    let d2 = description(vec![sheet("deck.png", 10, 7, 68, 0)], false);
    let other = build(&d2, &["f"], &["b"], false).unwrap();
    assert_ne!(deck(&a)["GUID"], deck(&other)["GUID"]);
}

#[test]
fn text_from_meta_and_file_names() {
    let d = description(vec![sheet("deck.png", 2, 2, 3, 0)], false);
    let text = |n: &str, d: &str, g: Option<&str>| Text {
        nickname: Some(n.to_owned()),
        description: Some(d.to_owned()),
        gm_notes: g.map(str::to_owned),
    };
    let meta = Meta {
        deck: text("My deck", "About", Some("Secret")),
        cards: vec![text("Ace", "The first", None)],
    };
    let faces = urls(&["f"]);
    let backs = urls(&["b"]);
    let input = |meta, files| ObjectInput {
        description: &d,
        face_urls: &faces,
        back_urls: &backs,
        unique_backs: false,
        meta,
        nicknames_from_files: files,
    };
    let v = build_object(&input(&meta, true)).unwrap();
    let o = deck(&v);
    assert_eq!(o["Nickname"], "My deck");
    assert_eq!(o["Description"], "About");
    assert_eq!(o["GMNotes"], "Secret");
    let c = o["ContainedObjects"].as_array().unwrap();
    assert_eq!(c[0]["Nickname"], "Ace");
    assert_eq!(c[0]["Description"], "The first");
    assert_eq!(c[0]["GMNotes"], "");
    assert_eq!(c[1]["Nickname"], "card01");
    assert_eq!(c[2]["Nickname"], "card02");
    // Without the option, cards without text stay empty.
    let v = build_object(&input(&meta, false)).unwrap();
    assert_eq!(deck(&v)["ContainedObjects"][1]["Nickname"], "");
    // More card entries than cards is an error.
    let long = Meta {
        deck: Text::default(),
        cards: vec![Text::default(); 4],
    };
    let err = build_object(&input(&long, false)).unwrap_err();
    assert!(err.contains("4 cards"), "{err}");
}

#[test]
fn source_stems() {
    assert_eq!(source_stem("cards/ace of spades.png"), "ace of spades");
    assert_eq!(source_stem("a.b.png"), "a.b");
    assert_eq!(source_stem("noext"), "noext");
}

fn meta_description() -> Description {
    description(
        vec![
            sheet("deck_1.png", 2, 2, 3, 0),
            sheet("deck_2.png", 2, 2, 1, 3),
        ],
        false,
    )
}

#[test]
fn meta_with_a_card_list() {
    let d = meta_description();
    let meta = parse_meta(
        r#"{"format_version": 1,
            "deck": {"nickname": "D", "gm_notes": "G"},
            "cards": [{"nickname": "A", "description": "a"}, null, {"gm_notes": "n"}]}"#,
        &d,
    )
    .unwrap();
    assert_eq!(meta.deck.nickname.as_deref(), Some("D"));
    assert_eq!(meta.deck.gm_notes.as_deref(), Some("G"));
    assert_eq!(meta.deck.description, None);
    assert_eq!(meta.cards.len(), 3);
    assert_eq!(meta.cards[0].description.as_deref(), Some("a"));
    assert_eq!(meta.cards[1], Text::default());
    assert_eq!(meta.cards[2].gm_notes.as_deref(), Some("n"));
}

#[test]
fn meta_with_cards_by_source_or_file_name() {
    let d = meta_description();
    let meta = parse_meta(
        r#"{"cards": {"cards/card01.png": {"nickname": "One"}, "card03.png": {"nickname": "Four"}}}"#,
        &d,
    )
    .unwrap();
    assert_eq!(meta.cards.len(), 4);
    assert_eq!(meta.cards[1].nickname.as_deref(), Some("One"));
    assert_eq!(meta.cards[3].nickname.as_deref(), Some("Four"));
    assert_eq!(meta.cards[0], Text::default());
}

#[test]
fn meta_is_checked() {
    let d = meta_description();
    for (text, expect) in [
        ("{", "not valid"),
        (r#"{"format_version": 2}"#, "format_version 2"),
        (r#"{"deck": {"name": "x"}}"#, "unknown field"),
        (r#"{"cards": [null, null, null, null, null]}"#, "5 cards"),
        (r#"{"cards": {"nothing.png": {}}}"#, "matches no card"),
        (
            r#"{"cards": {"card01.png": {}, "cards/card01.png": {}}}"#,
            "both set",
        ),
    ] {
        let err = parse_meta(text, &d).unwrap_err();
        assert!(err.contains(expect), "{text}: {err}");
    }
    // The same file name in two folders is ambiguous by name alone.
    let mut d = meta_description();
    d.sheets[1].cards[0].source = "other/card01.png".to_owned();
    let err = parse_meta(r#"{"cards": {"card01.png": {}}}"#, &d).unwrap_err();
    assert!(err.contains("ambiguous"), "{err}");
}
