//! Comparing the fields of two matched JSON maps.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use similar::{ChangeTag, TextDiff};

use crate::model::{Change, ChangeKind, DiffOptions, LineChange};

type Flat<'a> = BTreeMap<String, &'a Value>;

/// The change kind of a text-valued key, if it has one.
pub(crate) fn text_kind(key: &str) -> Option<ChangeKind> {
    match key {
        "LuaScript" => Some(ChangeKind::Script),
        "XmlUI" => Some(ChangeKind::XmlUi),
        "LuaScriptState" => Some(ChangeKind::ScriptState),
        _ => None,
    }
}

fn is_url(value: Option<&Value>) -> bool {
    let Some(Value::String(s)) = value else {
        return false;
    };
    let starts = |prefix: &str| {
        s.get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
    };
    starts("http://") || starts("https://")
}

/// Flattens `value` into leaf paths: `a.b` for object keys, `a[0]` for items.
/// Empty containers and scalars are leaves.
fn flatten<'a>(value: &'a Value, path: String, out: &mut Flat<'a>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                flatten(child, format!("{path}.{key}"), out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (i, child) in items.iter().enumerate() {
                flatten(child, format!("{path}[{i}]"), out);
            }
        }
        _ => {
            out.insert(path, value);
        }
    }
}

fn flatten_entry<'a>(key: &str, value: Option<&'a Value>, out: &mut Flat<'a>) {
    if let Some(value) = value {
        flatten(value, key.to_owned(), out);
    }
}

fn flat_changes(old: &Flat<'_>, new: &Flat<'_>, out: &mut Vec<Change>) {
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for path in paths {
        let (o, n) = (old.get(path).copied(), new.get(path).copied());
        if o == n {
            continue;
        }
        let kind = if is_url(o) || is_url(n) {
            ChangeKind::Assets
        } else {
            ChangeKind::Other
        };
        out.push(Change {
            kind,
            path: path.clone(),
            old: o.cloned(),
            new: n.cloned(),
            lines: None,
        });
    }
}

fn numbers_close(a: &Value, b: &Value, tolerance: f64) -> bool {
    if a == b {
        return true;
    }
    if tolerance > 0.0
        && a.is_number()
        && b.is_number()
        && let (Some(x), Some(y)) = (a.as_f64(), b.as_f64())
    {
        return (x - y).abs() <= tolerance;
    }
    false
}

fn transform_changes(
    old: Option<&Value>,
    new: Option<&Value>,
    tolerance: f64,
    out: &mut Vec<Change>,
) {
    if old == new {
        return;
    }
    if let (Some(Value::Object(o)), Some(Value::Object(n))) = (old, new) {
        let keys: BTreeSet<&String> = o.keys().chain(n.keys()).collect();
        for key in keys {
            let (ov, nv) = (o.get(key), n.get(key));
            let equal = match (ov, nv) {
                (Some(a), Some(b)) => numbers_close(a, b, tolerance),
                (a, b) => a == b,
            };
            if !equal {
                out.push(Change {
                    kind: ChangeKind::Transform,
                    path: format!("Transform.{key}"),
                    old: ov.cloned(),
                    new: nv.cloned(),
                    lines: None,
                });
            }
        }
    } else {
        out.push(Change {
            kind: ChangeKind::Transform,
            path: "Transform".to_owned(),
            old: old.cloned(),
            new: new.cloned(),
            lines: None,
        });
    }
}

/// A change for a text-valued key; `None` when equal.
pub(crate) fn text_change(
    kind: ChangeKind,
    key: &str,
    old: Option<&Value>,
    new: Option<&Value>,
    options: &DiffOptions,
) -> Option<Change> {
    if old == new {
        return None;
    }
    fn as_text(v: Option<&Value>) -> Option<&str> {
        match v {
            None => Some(""),
            Some(Value::String(s)) => Some(s.as_str()),
            Some(_) => None,
        }
    }
    let (Some(o), Some(n)) = (as_text(old), as_text(new)) else {
        return Some(Change {
            kind,
            path: key.to_owned(),
            old: old.cloned(),
            new: new.cloned(),
            lines: None,
        });
    };
    let diff = TextDiff::from_lines(o, n);
    let (mut added, mut removed) = (0, 0);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    // Scripts and UI get the full diff; script state is usually one huge line.
    let full = options.scripts && kind != ChangeKind::ScriptState;
    let diff = full.then(|| {
        diff.unified_diff()
            .context_radius(3)
            .header("old", "new")
            .to_string()
    });
    Some(Change {
        kind,
        path: key.to_owned(),
        old: None,
        new: None,
        lines: Some(LineChange {
            added,
            removed,
            diff,
        }),
    })
}

/// Sorts changes by kind, then path.
pub(crate) fn sort_changes(changes: &mut [Change]) {
    changes.sort_by(|a, b| (a.kind, &a.path).cmp(&(b.kind, &b.path)));
}

/// Differences between two matched scene objects, excluding the children
/// (`ContainedObjects`, `States`), which are matched objects of their own.
pub(crate) fn object_changes(
    old: &Map<String, Value>,
    new: &Map<String, Value>,
    options: &DiffOptions,
) -> Vec<Change> {
    let mut out = Vec::new();
    transform_changes(
        old.get("Transform"),
        new.get("Transform"),
        options.tolerance,
        &mut out,
    );
    let (mut fo, mut fn_) = (Flat::new(), Flat::new());
    let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for key in keys {
        match key.as_str() {
            "Transform" | "ContainedObjects" | "States" => {}
            k => {
                if let Some(kind) = text_kind(k) {
                    out.extend(text_change(kind, k, old.get(k), new.get(k), options));
                } else {
                    flatten_entry(k, old.get(k), &mut fo);
                    flatten_entry(k, new.get(k), &mut fn_);
                }
            }
        }
    }
    flat_changes(&fo, &fn_, &mut out);
    sort_changes(&mut out);
    out
}

/// Differences inside one top-level key present in either map.
pub(crate) fn global_changes(
    key: &str,
    old: Option<&Value>,
    new: Option<&Value>,
    options: &DiffOptions,
) -> Vec<Change> {
    let mut out = Vec::new();
    if let Some(kind) = text_kind(key) {
        out.extend(text_change(kind, key, old, new, options));
    } else {
        let (mut fo, mut fn_) = (Flat::new(), Flat::new());
        flatten_entry(key, old, &mut fo);
        flatten_entry(key, new, &mut fn_);
        flat_changes(&fo, &fn_, &mut out);
    }
    sort_changes(&mut out);
    out
}
