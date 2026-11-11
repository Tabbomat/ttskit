//! Thin, lossless typed views over a save's JSON value.
//!
//! The views borrow a [`serde_json::Map`] and expose the fields tools usually
//! need (scripts, names, GUIDs, nested objects) without copying or
//! restructuring anything. Everything else, including unknown fields, key
//! order and number text, stays in the underlying map and passes through
//! untouched.
//!
//! Field names follow the Tabletop Simulator save format (`SaveName`,
//! `LuaScript`, `ObjectStates`, `ContainedObjects`, `States`, ...).
//!
//! Lenient reading: a missing key or a value of the wrong type yields `None`
//! for string getters and an empty iterator for collections. Entries that are
//! not JSON objects inside `ObjectStates`, `ContainedObjects` and `States` are
//! skipped.
//!
//! Setters replace an existing value in place, so the key order does not
//! change. A missing key is appended at the end of the object.

use serde_json::{Map, Value};

type JsonMap = Map<String, Value>;

fn str_field<'a>(map: &'a JsonMap, key: &str) -> Option<&'a str> {
    map.get(key)?.as_str()
}

fn set_str(map: &mut JsonMap, key: &str, value: impl Into<String>) {
    map.insert(key.to_owned(), Value::String(value.into()));
}

fn objects<'a>(map: &'a JsonMap, key: &str) -> impl Iterator<Item = &'a JsonMap> + 'a {
    map.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
}

fn objects_mut<'a>(map: &'a mut JsonMap, key: &str) -> impl Iterator<Item = &'a mut JsonMap> + 'a {
    map.get_mut(key)
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
}

fn entries<'a>(map: &'a JsonMap, key: &str) -> impl Iterator<Item = (&'a str, &'a JsonMap)> + 'a {
    map.get(key)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.as_str(), v.as_object()?)))
}

fn entries_mut<'a>(
    map: &'a mut JsonMap,
    key: &str,
) -> impl Iterator<Item = (&'a str, &'a mut JsonMap)> + 'a {
    map.get_mut(key)
        .and_then(Value::as_object_mut)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.as_str(), v.as_object_mut()?)))
}

/// Read-only view of a whole save (the top-level object).
#[derive(Debug, Clone, Copy)]
pub struct SaveView<'a> {
    map: &'a JsonMap,
}

impl<'a> SaveView<'a> {
    /// Creates a view over a save's top-level object.
    pub fn new(map: &'a JsonMap) -> Self {
        SaveView { map }
    }

    /// The `SaveName` field.
    pub fn save_name(&self) -> Option<&'a str> {
        str_field(self.map, "SaveName")
    }

    /// The global `LuaScript` field.
    pub fn lua_script(&self) -> Option<&'a str> {
        str_field(self.map, "LuaScript")
    }

    /// The global `LuaScriptState` field.
    pub fn lua_script_state(&self) -> Option<&'a str> {
        str_field(self.map, "LuaScriptState")
    }

    /// The global `XmlUI` field.
    pub fn xml_ui(&self) -> Option<&'a str> {
        str_field(self.map, "XmlUI")
    }

    /// The objects in `ObjectStates`; non-object entries are skipped.
    pub fn object_states(&self) -> impl Iterator<Item = ObjectView<'a>> + 'a {
        objects(self.map, "ObjectStates").map(ObjectView::new)
    }

    /// Any field by key.
    pub fn get(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key)
    }

    /// The underlying map.
    pub fn as_map(&self) -> &'a JsonMap {
        self.map
    }
}

/// Mutable view of a whole save (the top-level object).
#[derive(Debug)]
pub struct SaveViewMut<'a> {
    map: &'a mut JsonMap,
}

impl<'a> SaveViewMut<'a> {
    /// Creates a mutable view over a save's top-level object.
    pub fn new(map: &'a mut JsonMap) -> Self {
        SaveViewMut { map }
    }

    /// A read-only view borrowing from this one.
    pub fn as_view(&self) -> SaveView<'_> {
        SaveView::new(self.map)
    }

    /// Sets the global `LuaScript` (in place if present, else appended).
    pub fn set_lua_script(&mut self, script: impl Into<String>) {
        set_str(self.map, "LuaScript", script);
    }

    /// Sets the global `LuaScriptState` (in place if present, else appended).
    pub fn set_lua_script_state(&mut self, state: impl Into<String>) {
        set_str(self.map, "LuaScriptState", state);
    }

    /// Sets the global `XmlUI` (in place if present, else appended).
    pub fn set_xml_ui(&mut self, xml: impl Into<String>) {
        set_str(self.map, "XmlUI", xml);
    }

    /// Mutable views of the objects in `ObjectStates`; non-object entries are
    /// skipped.
    pub fn object_states_mut(&mut self) -> impl Iterator<Item = ObjectViewMut<'_>> {
        objects_mut(self.map, "ObjectStates").map(ObjectViewMut::new)
    }

    /// Mutable access to any field by key.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.map.get_mut(key)
    }

    /// The underlying map, mutably.
    pub fn as_map_mut(&mut self) -> &mut JsonMap {
        self.map
    }
}

/// Read-only view of one object (a table object, a contained object or a
/// state).
#[derive(Debug, Clone, Copy)]
pub struct ObjectView<'a> {
    map: &'a JsonMap,
}

impl<'a> ObjectView<'a> {
    /// Creates a view over an object's map.
    pub fn new(map: &'a JsonMap) -> Self {
        ObjectView { map }
    }

    /// The `GUID` field.
    pub fn guid(&self) -> Option<&'a str> {
        str_field(self.map, "GUID")
    }

    /// The `Name` field.
    pub fn name(&self) -> Option<&'a str> {
        str_field(self.map, "Name")
    }

    /// The `Nickname` field.
    pub fn nickname(&self) -> Option<&'a str> {
        str_field(self.map, "Nickname")
    }

    /// The object's `LuaScript` field.
    pub fn lua_script(&self) -> Option<&'a str> {
        str_field(self.map, "LuaScript")
    }

    /// The object's `LuaScriptState` field.
    pub fn lua_script_state(&self) -> Option<&'a str> {
        str_field(self.map, "LuaScriptState")
    }

    /// The object's `XmlUI` field.
    pub fn xml_ui(&self) -> Option<&'a str> {
        str_field(self.map, "XmlUI")
    }

    /// The objects in `ContainedObjects`; non-object entries are skipped.
    pub fn contained_objects(&self) -> impl Iterator<Item = ObjectView<'a>> + 'a {
        objects(self.map, "ContainedObjects").map(ObjectView::new)
    }

    /// The entries of the `States` map as `(key, object)` in file order;
    /// non-object entries are skipped.
    pub fn states(&self) -> impl Iterator<Item = (&'a str, ObjectView<'a>)> + 'a {
        entries(self.map, "States").map(|(k, m)| (k, ObjectView::new(m)))
    }

    /// Any field by key.
    pub fn get(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key)
    }

    /// The underlying map.
    pub fn as_map(&self) -> &'a JsonMap {
        self.map
    }
}

/// Mutable view of one object (a table object, a contained object or a
/// state).
#[derive(Debug)]
pub struct ObjectViewMut<'a> {
    map: &'a mut JsonMap,
}

impl<'a> ObjectViewMut<'a> {
    /// Creates a mutable view over an object's map.
    pub fn new(map: &'a mut JsonMap) -> Self {
        ObjectViewMut { map }
    }

    /// A read-only view borrowing from this one.
    pub fn as_view(&self) -> ObjectView<'_> {
        ObjectView::new(self.map)
    }

    /// Sets `LuaScript` (in place if present, else appended).
    pub fn set_lua_script(&mut self, script: impl Into<String>) {
        set_str(self.map, "LuaScript", script);
    }

    /// Sets `LuaScriptState` (in place if present, else appended).
    pub fn set_lua_script_state(&mut self, state: impl Into<String>) {
        set_str(self.map, "LuaScriptState", state);
    }

    /// Sets `XmlUI` (in place if present, else appended).
    pub fn set_xml_ui(&mut self, xml: impl Into<String>) {
        set_str(self.map, "XmlUI", xml);
    }

    /// Mutable views of the objects in `ContainedObjects`; non-object entries
    /// are skipped.
    pub fn contained_objects_mut(&mut self) -> impl Iterator<Item = ObjectViewMut<'_>> {
        objects_mut(self.map, "ContainedObjects").map(ObjectViewMut::new)
    }

    /// Mutable `(key, object)` entries of the `States` map in file order;
    /// non-object entries are skipped.
    pub fn states_mut(&mut self) -> impl Iterator<Item = (&str, ObjectViewMut<'_>)> {
        entries_mut(self.map, "States").map(|(k, m)| (k, ObjectViewMut::new(m)))
    }

    /// Mutable access to any field by key.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.map.get_mut(key)
    }

    /// The underlying map, mutably.
    pub fn as_map_mut(&mut self) -> &mut JsonMap {
        self.map
    }
}

#[cfg(test)]
mod tests {
    use crate::save::Save;

    fn save(json: &str) -> Save {
        json.parse().unwrap()
    }

    fn keys(map: &serde_json::Map<String, serde_json::Value>) -> Vec<&str> {
        map.keys().map(String::as_str).collect()
    }

    #[test]
    fn reads_fields() {
        let s = save(
            r#"{"SaveName":"n","LuaScript":"a","LuaScriptState":"b","XmlUI":"c","X":1,
            "ObjectStates":[{"GUID":"g","Name":"N","Nickname":"k","LuaScript":"l",
            "ContainedObjects":[{"GUID":"c1"}],"States":{"2":{"GUID":"s2"},"1":{"GUID":"s1"}}}]}"#,
        );
        let v = s.view();
        assert_eq!(v.save_name(), Some("n"));
        assert_eq!(v.lua_script(), Some("a"));
        assert_eq!(v.lua_script_state(), Some("b"));
        assert_eq!(v.xml_ui(), Some("c"));
        assert!(v.get("X").is_some());
        let o = v.object_states().next().unwrap();
        assert_eq!(o.guid(), Some("g"));
        assert_eq!(o.name(), Some("N"));
        assert_eq!(o.nickname(), Some("k"));
        assert_eq!(o.lua_script(), Some("l"));
        assert_eq!(o.xml_ui(), None);
        assert_eq!(o.contained_objects().next().unwrap().guid(), Some("c1"));
        let states: Vec<_> = o.states().map(|(k, s)| (k, s.guid())).collect();
        assert_eq!(states, [("2", Some("s2")), ("1", Some("s1"))]);
    }

    #[test]
    fn missing_and_wrong_types() {
        let s = save(r#"{"SaveName":5,"ObjectStates":{"a":1},"LuaScript":null}"#);
        let v = s.view();
        assert_eq!(v.save_name(), None);
        assert_eq!(v.lua_script(), None);
        assert_eq!(v.xml_ui(), None);
        assert_eq!(v.object_states().count(), 0);

        let s = save(
            r#"{"ObjectStates":[1,"x",null,[],{"GUID":7,"ContainedObjects":3,"States":[]},
            {"GUID":"ok","States":{"1":5}}]}"#,
        );
        let objs: Vec<_> = s.view().object_states().collect();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].guid(), None);
        assert_eq!(objs[0].contained_objects().count(), 0);
        assert_eq!(objs[0].states().count(), 0);
        assert_eq!(objs[1].guid(), Some("ok"));
        assert_eq!(objs[1].states().count(), 0);
    }

    #[test]
    fn setter_keeps_position_of_existing_key() {
        let mut s = save(r#"{"A":1,"LuaScript":"old","Z":2}"#);
        s.view_mut().set_lua_script("new");
        assert_eq!(
            s.to_vec(),
            save(r#"{"A":1,"LuaScript":"new","Z":2}"#).to_vec()
        );
        assert_eq!(keys(s.map()), ["A", "LuaScript", "Z"]);
    }

    #[test]
    fn setter_appends_missing_key() {
        let mut s = save(r#"{"A":1,"ObjectStates":[{"B":1}]}"#);
        let mut v = s.view_mut();
        v.set_xml_ui("<x/>");
        v.set_lua_script_state("st");
        for mut o in v.object_states_mut() {
            o.set_lua_script("code");
        }
        assert_eq!(
            keys(s.map()),
            ["A", "ObjectStates", "XmlUI", "LuaScriptState"]
        );
        let o = s.view().object_states().next().unwrap();
        assert_eq!(keys(o.as_map()), ["B", "LuaScript"]);
        assert_eq!(o.lua_script(), Some("code"));
    }

    #[test]
    fn setters_replace_wrong_typed_value() {
        let mut s = save(r#"{"LuaScript":5}"#);
        s.view_mut().set_lua_script("x");
        assert_eq!(s.view().lua_script(), Some("x"));
    }

    #[test]
    fn mut_views_reach_nested_objects() {
        let mut s = save(
            r#"{"ObjectStates":[1,{"ContainedObjects":[{"GUID":"c"}],"States":{"2":{"GUID":"s"}}}]}"#,
        );
        let mut v = s.view_mut();
        let mut o = v.object_states_mut().next().unwrap();
        for mut c in o.contained_objects_mut() {
            c.set_xml_ui("cx");
        }
        for (k, mut st) in o.states_mut() {
            assert_eq!(k, "2");
            st.set_lua_script_state("ss");
            assert_eq!(st.as_view().guid(), Some("s"));
        }
        assert!(o.get_mut("Nope").is_none());
        let o = s.view().object_states().next().unwrap();
        assert_eq!(o.contained_objects().next().unwrap().xml_ui(), Some("cx"));
        assert_eq!(o.states().next().unwrap().1.lua_script_state(), Some("ss"));
    }
}
