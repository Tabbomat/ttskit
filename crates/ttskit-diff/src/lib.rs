//! Semantic diff of Tabletop Simulator saves (unofficial, not affiliated with
//! Berserk Games).
//!
//! [`diff`] matches scene objects of two [`Save`]s by `GUID` across the whole
//! save, so an object that moved between containers is reported as moved, not
//! as removed and added. Numbers are compared as text; only transform numbers
//! can use a tolerance. Output is sorted and deterministic.
//!
//! Objects whose GUID is missing or occurs more than once are ambiguous. They
//! are paired only when they sit at the same index of the same parent list;
//! the rest is reported as added or removed and flagged.

mod collect;
mod compare;
mod matching;
mod model;
mod render;

use std::collections::BTreeSet;

use serde_json::Value;
use ttskit_core::Save;

pub use model::{
    Ambiguity, Change, ChangeKind, DiffOptions, DuplicateGuid, GlobalDiff, GlobalGroup, LineChange,
    Move, ObjectDiff, ObjectStatus, ParentRef, SaveDiff,
};
pub use render::{render_text, to_json};

/// Computes the differences between `old` and `new`.
pub fn diff(old: &Save, new: &Save, options: &DiffOptions) -> SaveDiff {
    let (objects, duplicate_guids) = matching::diff_objects(old, new, options);
    SaveDiff {
        objects,
        globals: diff_globals(old, new, options),
        duplicate_guids,
    }
}

fn group_of(key: &str) -> GlobalGroup {
    match key {
        "LuaScript" => GlobalGroup::Script,
        "XmlUI" => GlobalGroup::XmlUi,
        "LuaScriptState" => GlobalGroup::ScriptState,
        "CustomUIAssets" => GlobalGroup::UiAssets,
        "Hands" => GlobalGroup::Hands,
        "TabStates" => GlobalGroup::TabStates,
        k if k.starts_with("Table") => GlobalGroup::Table,
        k if k.starts_with("Sky") => GlobalGroup::Sky,
        k if k.starts_with("Light") => GlobalGroup::Lighting,
        _ => GlobalGroup::Other,
    }
}

fn diff_globals(old: &Save, new: &Save, options: &DiffOptions) -> Vec<GlobalDiff> {
    let (om, nm) = (old.map(), new.map());
    let keys: BTreeSet<&String> = om.keys().chain(nm.keys()).collect();
    let mut out = Vec::new();
    for key in keys.into_iter().filter(|k| k.as_str() != "ObjectStates") {
        let (o, n): (Option<&Value>, Option<&Value>) = (om.get(key), nm.get(key));
        let changes = compare::global_changes(key, o, n, options);
        if !changes.is_empty() {
            out.push(GlobalDiff {
                group: group_of(key),
                key: key.clone(),
                changes,
            });
        }
    }
    out.sort_by(|a, b| (a.group, &a.key).cmp(&(b.group, &b.key)));
    out
}
