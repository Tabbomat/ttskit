//! Text and JSON renderers. Both follow the (already sorted) order of
//! [`SaveDiff`].

use std::collections::BTreeSet;
use std::fmt::Write;

use serde_json::{Map, Value, json};

use crate::model::{Ambiguity, Change, ChangeKind, ObjectDiff, ObjectStatus, ParentRef, SaveDiff};

/// Longest value shown in verbose text output.
const MAX_VALUE_CHARS: usize = 80;

fn who(nickname: &str, guid: &str) -> String {
    let name = if nickname.is_empty() {
        "<unnamed>"
    } else {
        nickname
    };
    if guid.is_empty() {
        format!("{name} (no GUID)")
    } else {
        format!("{name} ({guid})")
    }
}

fn parent_text(parent: &ParentRef) -> String {
    match parent {
        ParentRef::TopLevel => "top level".to_owned(),
        ParentRef::Object { guid, nickname } => who(nickname, guid),
    }
}

fn value_text(value: Option<&Value>) -> String {
    let text = match value {
        None => return "<absent>".to_owned(),
        Some(v) => v.to_string(),
    };
    if text.chars().count() > MAX_VALUE_CHARS {
        let cut: String = text.chars().take(MAX_VALUE_CHARS).collect();
        format!("{cut}...")
    } else {
        text
    }
}

fn kinds_text(changes: &[Change]) -> String {
    let kinds: BTreeSet<ChangeKind> = changes.iter().map(|c| c.kind).collect();
    let names: Vec<&str> = kinds.into_iter().map(ChangeKind::as_str).collect();
    names.join(", ")
}

fn write_changes(out: &mut String, changes: &[Change]) {
    for c in changes {
        if let Some(lines) = &c.lines {
            let _ = writeln!(
                out,
                "      {} {}: +{} -{}",
                c.kind.as_str(),
                c.path,
                lines.added,
                lines.removed
            );
            if let Some(diff) = &lines.diff {
                for line in diff.lines() {
                    let _ = writeln!(out, "        {line}");
                }
            }
        } else {
            let _ = writeln!(
                out,
                "      {} {}: {} -> {}",
                c.kind.as_str(),
                c.path,
                value_text(c.old.as_ref()),
                value_text(c.new.as_ref())
            );
        }
    }
}

fn ambiguity_text(a: Option<Ambiguity>) -> &'static str {
    match a {
        None => "",
        Some(Ambiguity::DuplicateGuid) => " [ambiguous: duplicate GUID]",
        Some(Ambiguity::NoGuid) => " [ambiguous: no GUID]",
    }
}

fn write_object(out: &mut String, o: &ObjectDiff, verbose: bool) {
    let name = who(&o.nickname, &o.guid);
    let amb = ambiguity_text(o.ambiguity);
    match o.status {
        ObjectStatus::Added => {
            let _ = writeln!(out, "+ {name}{amb}");
        }
        ObjectStatus::Removed => {
            let _ = writeln!(out, "- {name}{amb}");
        }
        ObjectStatus::Moved | ObjectStatus::Changed => {
            if let Some(m) = &o.moved {
                let _ = writeln!(
                    out,
                    "> {name}{amb} moved from {} to {}",
                    parent_text(&m.from),
                    parent_text(&m.to)
                );
            }
            if !o.changes.is_empty() {
                let _ = writeln!(out, "~ {name}{amb} changed: {}", kinds_text(&o.changes));
            }
        }
    }
    if verbose {
        write_changes(out, &o.changes);
    }
}

/// Renders the diff as text: a summary, then one line per object and global
/// change; `verbose` adds the details of every change. Equal saves render as
/// `no differences`.
pub fn render_text(diff: &SaveDiff, verbose: bool) -> String {
    let mut out = String::new();
    if diff.is_empty() {
        out.push_str("no differences\n");
    } else {
        let count = |s: ObjectStatus| diff.objects.iter().filter(|o| o.status == s).count();
        let _ = writeln!(
            out,
            "{} added, {} removed, {} moved, {} changed, {} global",
            count(ObjectStatus::Added),
            count(ObjectStatus::Removed),
            count(ObjectStatus::Moved),
            count(ObjectStatus::Changed),
            diff.globals.len()
        );
        for o in &diff.objects {
            write_object(&mut out, o, verbose);
        }
        for g in &diff.globals {
            let _ = writeln!(
                out,
                "~ global {} ({}): {}",
                g.group.as_str(),
                g.key,
                kinds_text(&g.changes)
            );
            if verbose {
                write_changes(&mut out, &g.changes);
            }
        }
    }
    for d in &diff.duplicate_guids {
        let _ = writeln!(
            out,
            "! duplicate GUID {}: {} in old, {} in new",
            d.guid, d.old_count, d.new_count
        );
    }
    out
}

fn parent_json(parent: &ParentRef) -> Value {
    match parent {
        ParentRef::TopLevel => json!({ "top_level": true }),
        ParentRef::Object { guid, nickname } => json!({ "guid": guid, "nickname": nickname }),
    }
}

fn change_json(c: &Change) -> Value {
    let mut m = Map::new();
    m.insert("kind".into(), c.kind.as_str().into());
    m.insert("path".into(), c.path.as_str().into());
    if let Some(old) = &c.old {
        m.insert("old".into(), old.clone());
    }
    if let Some(new) = &c.new {
        m.insert("new".into(), new.clone());
    }
    if let Some(lines) = &c.lines {
        let mut l = Map::new();
        l.insert("added".into(), lines.added.into());
        l.insert("removed".into(), lines.removed.into());
        if let Some(diff) = &lines.diff {
            l.insert("diff".into(), diff.as_str().into());
        }
        m.insert("lines".into(), Value::Object(l));
    }
    Value::Object(m)
}

fn object_json(o: &ObjectDiff) -> Value {
    let mut m = Map::new();
    m.insert("status".into(), o.status.as_str().into());
    m.insert("guid".into(), o.guid.as_str().into());
    m.insert("nickname".into(), o.nickname.as_str().into());
    if let Some(a) = o.ambiguity {
        m.insert("ambiguous".into(), a.as_str().into());
    }
    if let Some(mv) = &o.moved {
        m.insert(
            "moved".into(),
            json!({ "from": parent_json(&mv.from), "to": parent_json(&mv.to) }),
        );
    }
    m.insert(
        "changes".into(),
        Value::Array(o.changes.iter().map(change_json).collect()),
    );
    Value::Object(m)
}

/// Renders the diff as JSON with the keys `objects`, `globals` and
/// `duplicate_guids`, in the order of the [`SaveDiff`].
pub fn to_json(diff: &SaveDiff) -> Value {
    json!({
        "objects": diff.objects.iter().map(object_json).collect::<Vec<_>>(),
        "globals": diff.globals.iter().map(|g| json!({
            "group": g.group.as_str(),
            "key": g.key,
            "changes": g.changes.iter().map(change_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "duplicate_guids": diff.duplicate_guids.iter().map(|d| json!({
            "guid": d.guid,
            "old_count": d.old_count,
            "new_count": d.new_count,
        })).collect::<Vec<_>>(),
    })
}
