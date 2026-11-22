#![allow(dead_code)]

use serde_json::{Value, json};
use ttskit_core::Save;
use ttskit_diff::{DiffOptions, ObjectDiff, ObjectStatus, SaveDiff, diff};

pub fn save(v: Value) -> Save {
    Save::from_value(v).unwrap()
}

/// A save with the given top-level objects.
pub fn with_objects(objects: Vec<Value>) -> Save {
    save(json!({ "SaveName": "t", "ObjectStates": objects }))
}

pub fn obj(guid: &str, nick: &str) -> Value {
    json!({ "GUID": guid, "Nickname": nick, "Name": "Cube" })
}

pub fn bag(guid: &str, nick: &str, children: Vec<Value>) -> Value {
    json!({ "GUID": guid, "Nickname": nick, "Name": "Bag", "ContainedObjects": children })
}

pub fn run(old: &Save, new: &Save) -> SaveDiff {
    diff(old, new, &DiffOptions::default())
}

pub fn run_with(old: &Save, new: &Save, tolerance: f64, scripts: bool) -> SaveDiff {
    diff(old, new, &DiffOptions { tolerance, scripts })
}

pub fn find<'a>(d: &'a SaveDiff, guid: &str) -> &'a ObjectDiff {
    d.objects
        .iter()
        .find(|o| o.guid == guid)
        .unwrap_or_else(|| panic!("no entry for {guid}: {d:?}"))
}

pub fn statuses(d: &SaveDiff) -> Vec<(ObjectStatus, String)> {
    d.objects
        .iter()
        .map(|o| (o.status, o.guid.clone()))
        .collect()
}
