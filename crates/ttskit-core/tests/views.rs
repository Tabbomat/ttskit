//! Typed views over the synthetic fixture.

use serde_json::Value;
use ttskit_core::Save;

fn fixture_bytes() -> Vec<u8> {
    ttskit_testutil::read_fixture("minimal_save.json")
}

fn fixture() -> Save {
    Save::from_slice(&fixture_bytes()).unwrap()
}

#[test]
fn reads_global_fields() {
    let save = fixture();
    let v = save.view();
    assert_eq!(v.save_name(), Some("Minimal Test Save"));
    assert!(v.lua_script().unwrap().starts_with("print(\"hello\")"));
    assert_eq!(
        v.lua_script_state(),
        Some("{\"counter\":3,\"names\":[\"a\",\"b\"]}")
    );
    assert!(v.xml_ui().unwrap().starts_with("<Panel"));
    assert_eq!(v.get("Table"), Some(&Value::String("Table_RPG".into())));
    assert_eq!(v.object_states().count(), 2);
}

#[test]
fn reads_objects_contained_objects_and_states() {
    let save = fixture();
    let objects: Vec<_> = save.view().object_states().collect();

    let bag = objects[0];
    assert_eq!(bag.guid(), Some("aaaaaa"));
    assert_eq!(bag.name(), Some("Bag"));
    assert_eq!(bag.nickname(), Some("Tüte 日本語 🎲"));
    assert_eq!(bag.states().count(), 0);
    let contained: Vec<_> = bag.contained_objects().collect();
    assert_eq!(contained.len(), 1);
    assert_eq!(contained[0].guid(), Some("bbbbbb"));
    assert_eq!(contained[0].name(), Some("Card"));
    assert_eq!(contained[0].nickname(), Some("Ace"));
    assert_eq!(contained[0].lua_script(), Some(""));
    assert_eq!(contained[0].contained_objects().count(), 0);

    let model = objects[1];
    assert_eq!(model.guid(), Some("cccccc"));
    assert_eq!(model.contained_objects().count(), 0);
    let states: Vec<_> = model
        .states()
        .map(|(k, s)| (k, s.guid().unwrap(), s.nickname().unwrap()))
        .collect();
    assert_eq!(
        states,
        [("2", "dddddd", "State two"), ("3", "eeeeee", "State three")]
    );
}

#[test]
fn missing_and_wrong_type_fields() {
    let save = fixture();
    let bag = save.view().object_states().next().unwrap();
    assert_eq!(bag.get("Locked"), Some(&Value::Bool(false)));
    assert_eq!(bag.get("Nope"), None);
    // Present but not a string.
    assert_eq!(bag.lua_script_state(), Some(""));
    let wrong: Save = r#"{"ObjectStates":[{"GUID":1,"ContainedObjects":{},"States":[]},7]}"#
        .parse()
        .unwrap();
    let objs: Vec<_> = wrong.view().object_states().collect();
    assert_eq!(objs.len(), 1);
    assert_eq!(objs[0].guid(), None);
    assert_eq!(objs[0].contained_objects().count(), 0);
    assert_eq!(objs[0].states().count(), 0);
}

#[test]
fn setting_current_values_is_byte_identical() {
    let original = fixture_bytes();
    let mut save = Save::from_slice(&original).unwrap();

    let global = (
        save.view().lua_script().unwrap().to_owned(),
        save.view().lua_script_state().unwrap().to_owned(),
        save.view().xml_ui().unwrap().to_owned(),
    );
    {
        let mut v = save.view_mut();
        v.set_lua_script(global.0);
        v.set_lua_script_state(global.1);
        v.set_xml_ui(global.2);
        for mut o in v.object_states_mut() {
            let (s, st, x) = {
                let view = o.as_view();
                (
                    view.lua_script().unwrap().to_owned(),
                    view.lua_script_state().unwrap().to_owned(),
                    view.xml_ui().unwrap().to_owned(),
                )
            };
            o.set_lua_script(s);
            o.set_lua_script_state(st);
            o.set_xml_ui(x);
        }
    }
    assert_eq!(save.to_vec(), original);
}

#[test]
fn setters_only_change_the_targeted_values() {
    let mut save = fixture();
    {
        let mut v = save.view_mut();
        let mut objects = v.object_states_mut();
        let mut bag = objects.next().unwrap();
        bag.contained_objects_mut()
            .next()
            .unwrap()
            .set_lua_script("print(\"card\")");
        let mut model = objects.next().unwrap();
        for (key, mut state) in model.states_mut() {
            if key == "3" {
                state.set_lua_script_state("saved");
            }
        }
    }

    // Expected result: the same edits applied through raw JSON pointers.
    let mut expected = fixture();
    {
        let root = expected.map_mut();
        let mut root = Value::Object(std::mem::take(root));
        *root
            .pointer_mut("/ObjectStates/0/ContainedObjects/0/LuaScript")
            .unwrap() = Value::String("print(\"card\")".into());
        *root
            .pointer_mut("/ObjectStates/1/States/3/LuaScriptState")
            .unwrap() = Value::String("saved".into());
        expected = Save::from_value(root).unwrap();
    }
    assert_eq!(save.to_vec(), expected.to_vec());
    assert_ne!(save.to_vec(), fixture_bytes());

    // Key order of the edited objects is unchanged.
    let card_keys: Vec<_> = save
        .view()
        .object_states()
        .next()
        .unwrap()
        .contained_objects()
        .next()
        .unwrap()
        .as_map()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        card_keys,
        [
            "GUID",
            "Name",
            "Nickname",
            "Transform",
            "LuaScript",
            "LuaScriptState",
            "XmlUI"
        ]
    );
    // Unknown fields are untouched.
    let bag = save.view().object_states().next().unwrap();
    assert_eq!(
        bag.get("SomeFutureField"),
        fixture()
            .view()
            .object_states()
            .next()
            .unwrap()
            .get("SomeFutureField")
    );
}

#[test]
fn setter_on_missing_key_appends() {
    let mut save = fixture();
    let mut v = save.view_mut();
    let mut objects = v.object_states_mut();
    let mut bag = objects.next().unwrap();
    let last_key = bag
        .contained_objects_mut()
        .next()
        .map(|mut c| {
            assert!(c.as_view().get("Nope").is_none());
            c.as_map_mut().remove("XmlUI");
            c.set_xml_ui("<x/>");
            c.as_map_mut().keys().next_back().cloned()
        })
        .unwrap();
    assert_eq!(last_key.as_deref(), Some("XmlUI"));
}
