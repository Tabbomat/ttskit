//! Collecting scene objects together with their position.

use serde_json::{Map, Value};
use ttskit_core::Save;

use crate::model::ParentRef;

/// A scene object and where it sits.
pub(crate) struct Obj<'a> {
    pub map: &'a Map<String, Value>,
    /// Empty when the object has no string `GUID`.
    pub guid: String,
    pub nickname: String,
    pub parent: ParentRef,
    /// Same-parent position: parent GUID path, list name and index.
    pub slot: String,
}

fn str_of(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// All scene objects of the save, in document order.
pub(crate) fn collect(save: &Save) -> Vec<Obj<'_>> {
    let mut out = Vec::new();
    if let Some(Value::Array(items)) = save.map().get("ObjectStates") {
        visit_list(items, "ObjectStates", "", &ParentRef::TopLevel, &mut out);
    }
    out
}

fn visit_list<'a>(
    items: &'a [Value],
    list: &str,
    parent_path: &str,
    parent: &ParentRef,
    out: &mut Vec<Obj<'a>>,
) {
    for (i, item) in items.iter().enumerate() {
        if let Value::Object(map) = item {
            visit(map, list, i, parent_path, parent, out);
        }
    }
}

fn visit<'a>(
    map: &'a Map<String, Value>,
    list: &str,
    index: usize,
    parent_path: &str,
    parent: &ParentRef,
    out: &mut Vec<Obj<'a>>,
) {
    let guid = str_of(map, "GUID");
    let nickname = str_of(map, "Nickname");
    let segment = if guid.is_empty() {
        format!("#{index}")
    } else {
        guid.clone()
    };
    let path = format!("{parent_path}/{segment}");
    out.push(Obj {
        map,
        guid: guid.clone(),
        nickname: nickname.clone(),
        parent: parent.clone(),
        slot: format!("{parent_path}|{list}|{index}"),
    });
    let me = ParentRef::Object { guid, nickname };
    if let Some(Value::Array(items)) = map.get("ContainedObjects") {
        visit_list(items, "ContainedObjects", &path, &me, out);
    }
    if let Some(Value::Object(states)) = map.get("States") {
        for (key, value) in states {
            if let Value::Object(state) = value {
                visit(state, &format!("States:{key}"), 0, &path, &me, out);
            }
        }
    }
}
