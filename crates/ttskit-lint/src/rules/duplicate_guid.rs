//! `duplicate-guid` and `duplicate-guid-nested`: the same GUID on several
//! scene objects.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use serde_json::{Map, Value};

use super::RuleInfo;
use crate::config::Severity;
use crate::ctx::{Ctx, SceneObject};

pub(super) const INFO: RuleInfo = RuleInfo {
    id: "duplicate-guid",
    default: Severity::Error,
    summary: "the same GUID is used by more than one object",
};

/// Fills in the messages of the findings recorded by [`check`].
pub(crate) fn finish(ctx: &mut Ctx<'_>) {
    let pending = std::mem::take(&mut ctx.pending_first_paths);
    if pending.is_empty() {
        return;
    }
    let targets: HashSet<usize> = pending.iter().map(|(_, m)| addr(m)).collect();
    let mut paths = HashMap::new();
    collect_paths(ctx.root, &targets, &mut String::new(), &mut paths);
    for (index, first) in pending {
        let finding = &mut ctx.findings_mut()[index];
        let guid = finding.guid.as_deref().unwrap_or_default();
        let first = paths.get(&addr(first)).map_or("?", String::as_str);
        finding.message = format!("GUID `{guid}` is already used by `{first}`");
    }
}

pub(super) const NESTED_INFO: RuleInfo = RuleInfo {
    id: "duplicate-guid-nested",
    default: Severity::Info,
    summary: "a GUID repeats and at least one of the objects is inside a container or a state",
};

/// One finding per extra occurrence, naming the first one. A top-level object
/// that repeats an earlier top-level GUID is `duplicate-guid`; any other
/// repeat (nested vs nested, or nested vs top-level) is `duplicate-guid-nested`.
/// Empty GUIDs are left to `missing-guid`.
pub(super) fn check<'a>(ctx: &mut Ctx<'a>, object: &SceneObject<'a, '_>) {
    if !ctx.enabled(INFO.id) && !ctx.enabled(NESTED_INFO.id) {
        return;
    }
    let Some(Value::String(guid)) = object.map.get("GUID") else {
        return;
    };
    if guid.is_empty() {
        return;
    }
    let guid = guid.as_str();
    let top_first = if object.top_level {
        match ctx.seen_top_guids.entry(guid) {
            Entry::Vacant(slot) => {
                slot.insert(object.map);
                None
            }
            Entry::Occupied(slot) => Some(*slot.get()),
        }
    } else {
        None
    };
    let (rule, first) = match (top_first, ctx.seen_guids.entry(guid)) {
        (Some(first), _) => (INFO.id, first),
        (None, Entry::Occupied(slot)) => (NESTED_INFO.id, *slot.get()),
        (None, Entry::Vacant(slot)) => {
            slot.insert(object.map);
            return;
        }
    };
    // The message is completed by `finish`, which needs the path of the first
    // occurrence; building it here would cost a traversal per duplicate.
    let index = ctx.finding_count();
    ctx.report(rule, object.path, object.owner, String::new);
    if ctx.finding_count() > index {
        ctx.pending_first_paths.push((index, first));
    }
}

fn addr(map: &Map<String, Value>) -> usize {
    std::ptr::from_ref(map) as usize
}

/// Records the path of every object whose address is in `targets`.
fn collect_paths(
    value: &Value,
    targets: &HashSet<usize>,
    path: &mut String,
    out: &mut HashMap<usize, String>,
) {
    match value {
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let len = path.len();
                let _ = write!(path, "[{i}]");
                collect_paths(item, targets, path, out);
                path.truncate(len);
            }
        }
        Value::Object(map) => {
            if targets.contains(&addr(map)) {
                out.insert(addr(map), path.clone());
            }
            for (key, child) in map {
                let len = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
                collect_paths(child, targets, path, out);
                path.truncate(len);
            }
        }
        _ => {}
    }
}
