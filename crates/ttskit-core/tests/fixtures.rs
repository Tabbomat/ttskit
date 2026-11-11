//! Checks over every synthetic fixture save in `tests/fixtures`, plus targeted
//! view tests for the individual fixtures.

use serde_json::Value;
use ttskit_core::Save;
use ttskit_core::save::view::ObjectView;

fn load(name: &str) -> Save {
    Save::from_slice(&ttskit_testutil::read_fixture(name)).unwrap()
}

/// Depth-first search through `ContainedObjects` for a GUID.
fn find_guid<'a>(
    objects: impl Iterator<Item = ObjectView<'a>>,
    guid: &str,
) -> Option<ObjectView<'a>> {
    for o in objects {
        if o.guid() == Some(guid) {
            return Some(o);
        }
        if let Some(found) = find_guid(o.contained_objects(), guid) {
            return Some(found);
        }
    }
    None
}

fn str_at<'a>(o: ObjectView<'a>, key: &str, field: &str) -> Option<&'a str> {
    o.get(key)?.get(field)?.as_str()
}

#[test]
fn every_fixture_round_trips_byte_for_byte() {
    for path in ttskit_testutil::fixture_saves() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let input = std::fs::read(&path).unwrap();

        assert!(!input.starts_with(b"\xEF\xBB\xBF"), "{name}: has a BOM");
        assert!(input.starts_with(b"{\r\n"), "{name}: must start with CRLF");
        assert!(!input.ends_with(b"\n"), "{name}: has a trailing newline");
        for (i, _) in input.iter().enumerate().filter(|(_, b)| **b == b'\n') {
            assert!(
                i > 0 && input[i - 1] == b'\r',
                "{name}: bare LF at byte {i}"
            );
        }

        let output = Save::from_slice(&input).unwrap().to_vec();
        assert!(
            output == input,
            "{name}: round trip differs: {}",
            ttskit_testutil::first_diff(&input, &output)
        );
    }
}

#[test]
fn expected_fixtures_present() {
    let names: Vec<String> = ttskit_testutil::fixture_saves()
        .iter()
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    for expected in [
        "minimal_save",
        "containers",
        "states",
        "scripts_ui",
        "custom_assets",
        "numbers",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing {expected}.json"
        );
    }
}

#[test]
fn containers_nest_three_bags_deep() {
    let save = load("containers.json");
    let view = save.view();
    let outer = view.object_states().next().unwrap();
    assert_eq!(outer.guid(), Some("bag001"));
    let middle = outer.contained_objects().next().unwrap();
    let inner = middle.contained_objects().next().unwrap();
    assert_eq!(inner.guid(), Some("bag003"));
    let leaf = inner.contained_objects().next().unwrap();
    assert_eq!(leaf.guid(), Some("leaf01"));
    assert_eq!(leaf.contained_objects().count(), 0);

    let found = find_guid(view.object_states(), "leaf01").unwrap();
    assert_eq!(found.nickname(), Some("Leaf token"));
    assert_eq!(
        str_at(found, "CustomImage", "ImageURL"),
        Some("https://example.com/token.png")
    );
}

#[test]
fn containers_deck_cards_match_deck_ids() {
    let save = load("containers.json");
    let view = save.view();
    let card_ids = |deck: ObjectView| -> Vec<i64> {
        deck.contained_objects()
            .map(|c| c.get("CardID").unwrap().as_i64().unwrap())
            .collect()
    };
    let deck_ids = |deck: ObjectView| -> Vec<i64> {
        deck.get("DeckIDs")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect()
    };

    let top = find_guid(view.object_states(), "deck01").unwrap();
    assert_eq!(top.contained_objects().count(), 3);
    assert_eq!(card_ids(top), [100, 101, 102]);
    assert_eq!(deck_ids(top), [100, 101, 102]);
    for card in top.contained_objects() {
        assert!(card.get("CustomDeck").unwrap().get("1").is_some());
    }

    let in_bag = find_guid(view.object_states(), "deck02").unwrap();
    assert_eq!(card_ids(in_bag), deck_ids(in_bag));
    let bag = view
        .object_states()
        .find(|o| o.guid() == Some("bag004"))
        .unwrap();
    assert_eq!(
        bag.contained_objects().next().unwrap().guid(),
        Some("deck02")
    );
}

#[test]
fn states_keep_order_and_per_state_scripts() {
    let save = load("states.json");
    let objects: Vec<_> = save.view().object_states().collect();

    let one = objects[0];
    let states: Vec<_> = one.states().collect();
    let keys: Vec<_> = states.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, ["2", "3"]);
    assert_eq!(one.guid(), Some("sta001"));
    assert_eq!(states[0].1.guid(), Some("sta002"));
    assert_eq!(states[1].1.guid(), Some("sta003"));

    let scripts = [
        one.lua_script(),
        states[0].1.lua_script(),
        states[1].1.lua_script(),
    ];
    assert_ne!(scripts[0], scripts[1]);
    assert_ne!(scripts[1], scripts[2]);
    assert_ne!(scripts[0], scripts[2]);
    assert_eq!(states[0].1.lua_script_state(), Some("{\"s\":2}"));
    assert_eq!(states[1].1.lua_script_state(), Some(""));
    assert!(states[1].1.lua_script().unwrap().contains("\r\n"));
}

#[test]
fn states_can_contain_objects() {
    let save = load("states.json");
    let chest = save.view().object_states().nth(1).unwrap();
    assert_eq!(chest.contained_objects().count(), 0);
    let (key, opened) = chest.states().next().unwrap();
    assert_eq!(key, "2");
    assert_eq!(opened.lua_script(), Some("-- chest open"));
    let contained: Vec<_> = opened.contained_objects().collect();
    assert_eq!(contained.len(), 2);
    assert_eq!(
        contained[0].contained_objects().next().unwrap().nickname(),
        Some("Coin")
    );
    assert_eq!(contained[1].nickname(), Some("Gem"));
}

#[test]
fn scripts_ui_global_script_and_ui() {
    let save = load("scripts_ui.json");
    let view = save.view();
    let script = view.lua_script().unwrap();
    assert!(script.contains("\r\n"));
    assert!(script.contains('\t'));
    assert!(script.contains("require(\"lib.common\")"));
    assert!(script.contains("C:\\\\temp\\\\x"));

    let state: Value = serde_json::from_str(view.lua_script_state().unwrap()).unwrap();
    assert_eq!(state["players"][1], "Red");

    let xml = view.xml_ui().unwrap();
    assert!(xml.contains("<Defaults>"));
    assert!(xml.contains("Rock &amp; Roll"));
    assert!(xml.contains("\r\n"));

    let assets = view.get("CustomUIAssets").unwrap().as_array().unwrap();
    assert_eq!(assets.len(), 2);
    assert_eq!(assets[0]["Name"], "icon_a");
}

#[test]
fn scripts_ui_per_object_fields() {
    let save = load("scripts_ui.json");
    let objects: Vec<_> = save.view().object_states().collect();
    assert_eq!(objects.len(), 3);

    assert!(
        objects[0]
            .lua_script()
            .unwrap()
            .contains("self.UI.setValue")
    );
    assert_eq!(objects[0].lua_script_state(), Some("{\"clicks\":2}"));
    assert!(objects[0].xml_ui().unwrap().contains("Hi &amp; bye"));

    assert_eq!(objects[1].lua_script(), Some("-- only a script\r\n"));
    assert_eq!(objects[1].lua_script_state(), Some(""));
    assert_eq!(objects[1].xml_ui(), Some(""));

    assert_eq!(objects[2].lua_script(), Some(""));
}

#[test]
fn custom_assets_urls_are_readable() {
    let save = load("custom_assets.json");
    let view = save.view();
    let find = |guid: &str| find_guid(view.object_states(), guid).unwrap();

    let image = find("img001");
    assert_eq!(
        str_at(image, "CustomImage", "ImageURL"),
        Some("https://example.com/image.png")
    );
    assert_eq!(
        str_at(image, "CustomImage", "ImageSecondaryURL"),
        Some("https://example.com/image_back.png")
    );
    let mesh = find("mdl001");
    assert_eq!(
        str_at(mesh, "CustomMesh", "MeshURL"),
        Some("https://example.com/model.obj")
    );
    assert_eq!(
        str_at(mesh, "CustomMesh", "NormalURL"),
        Some("https://example.com/model_n.png")
    );
    assert!(
        mesh.get("CustomMesh")
            .unwrap()
            .get("CustomShader")
            .is_some()
    );
    let bundle = find("abd001");
    assert_eq!(
        str_at(bundle, "CustomAssetbundle", "AssetbundleURL"),
        Some("https://example.com/bundle.unity3d")
    );
    let pdf = find("pdf001");
    assert_eq!(
        str_at(pdf, "CustomPDF", "PDFUrl"),
        Some("https://example.com/rules.pdf")
    );

    let deck = find("cdk001");
    assert!(
        str_at(deck, "CustomDeck", "FaceURL").is_none(),
        "keyed by deck number"
    );
    let face = deck.get("CustomDeck").unwrap()["1"]["FaceURL"]
        .as_str()
        .unwrap();
    assert!(face.starts_with("https://steamusercontent-a.akamaihd.net/ugc/"));
}

#[test]
fn custom_assets_top_level_urls() {
    let save = load("custom_assets.json");
    let view = save.view();
    assert_eq!(view.get("SkyURL").unwrap(), "https://example.com/sky.jpg");
    let assets = view.get("CustomUIAssets").unwrap().as_array().unwrap();
    assert_eq!(assets[1]["URL"], "https://example.com/ui/font.ttf");
    let music = view.get("MusicPlayer").unwrap();
    assert_eq!(
        music["CurrentAudioURL"],
        "https://example.com/audio/song.mp3"
    );
    assert_eq!(music["AudioLibrary"].as_array().unwrap().len(), 2);
}

#[test]
fn numbers_keep_their_source_text() {
    let save = load("numbers.json");
    let written = String::from_utf8(save.to_vec()).unwrap();
    for text in [
        "\"posX\": 1.0,",
        "\"posY\": 1E-05,",
        "\"posZ\": -0.0,",
        "\"rotX\": -5.039668E-05,",
        "\"rotY\": 179.999985,",
        "\"scaleX\": 0.333333343,",
        "\"scaleY\": 1E+20,",
        "\"posX\": -7,",
        "\"posY\": 0,",
        "\"Negative\": -7,",
        "\"LongDecimal\": 0.12345678901234567890123,",
        "        9223372036854775808,\r\n",
        "        18446744073709551616,\r\n",
        "        12345678901234567890123,\r\n",
        "        -9223372036854775809\r\n",
        "              2.5E-10,\r\n",
    ] {
        assert!(written.contains(text), "missing {text:?}");
    }
}

#[test]
fn numbers_have_expected_kinds() {
    let save = load("numbers.json");
    let objects: Vec<_> = save.view().object_states().collect();
    let transform = &objects[0].get("Transform").unwrap();
    assert_eq!(transform["posX"].as_f64(), Some(1.0));
    assert!(transform["posX"].is_f64());
    assert!(transform["posY"].as_f64().unwrap() > 0.0);
    assert!(transform["posZ"].as_f64().unwrap().is_sign_negative());

    let big = objects[1].get("Big").unwrap().as_array().unwrap();
    assert_eq!(big[0].as_i64(), Some(i64::MAX));
    assert_eq!(big[1].as_i64(), None);
    assert_eq!(big[1].as_u64(), Some(1 << 63));
    assert_eq!(big[2].as_u64(), Some(u64::MAX));
    assert_eq!(big[3].as_u64(), None);
    assert_eq!(big[5].as_i64(), Some(i64::MIN));
    assert_eq!(objects[1].get("Negative").unwrap().as_i64(), Some(-7));
    assert_eq!(objects[1].get("Zero").unwrap().as_i64(), Some(0));

    let deep = &objects[0].get("Nested").unwrap()["Deep"]["Values"];
    assert_eq!(deep[1].as_i64(), Some(-7));
    assert_eq!(deep[3][0].as_f64(), Some(2.5e-10));
}
