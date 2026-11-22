//! Matching objects of two saves and reporting their differences.

use std::collections::BTreeMap;

use ttskit_core::Save;

use crate::collect::{Obj, collect};
use crate::compare::object_changes;
use crate::model::{
    Ambiguity, DiffOptions, DuplicateGuid, Move, ObjectDiff, ObjectStatus, ParentRef,
};

type Counts<'a> = BTreeMap<&'a str, usize>;

fn count_guids<'a>(objs: &'a [Obj<'_>]) -> Counts<'a> {
    let mut counts = Counts::new();
    for o in objs.iter().filter(|o| !o.guid.is_empty()) {
        *counts.entry(o.guid.as_str()).or_default() += 1;
    }
    counts
}

fn ambiguity(guid: &str, old: &Counts<'_>, new: &Counts<'_>) -> Option<Ambiguity> {
    if guid.is_empty() {
        Some(Ambiguity::NoGuid)
    } else if old.get(guid).copied().unwrap_or(0) > 1 || new.get(guid).copied().unwrap_or(0) > 1 {
        Some(Ambiguity::DuplicateGuid)
    } else {
        None
    }
}

fn lone(objs: &[Obj<'_>], i: usize, status: ObjectStatus, amb: Option<Ambiguity>) -> ObjectDiff {
    ObjectDiff {
        status,
        guid: objs[i].guid.clone(),
        nickname: objs[i].nickname.clone(),
        ambiguity: amb,
        moved: None,
        changes: Vec::new(),
    }
}

/// Reports every object difference plus the duplicate GUIDs.
pub(crate) fn diff_objects(
    old: &Save,
    new: &Save,
    options: &DiffOptions,
) -> (Vec<ObjectDiff>, Vec<DuplicateGuid>) {
    let (oo, no) = (collect(old), collect(new));
    let (oc, nc) = (count_guids(&oo), count_guids(&no));

    // Unambiguous GUIDs match by GUID; the rest match by position.
    let mut new_by_key: BTreeMap<(Option<&str>, &str), usize> = BTreeMap::new();
    for (i, o) in no.iter().enumerate() {
        let key = if ambiguity(&o.guid, &oc, &nc).is_none() {
            (None, o.guid.as_str())
        } else {
            (Some(o.slot.as_str()), o.guid.as_str())
        };
        new_by_key.insert(key, i);
    }
    let mut new_taken = vec![false; no.len()];
    let mut out = Vec::new();
    for (i, o) in oo.iter().enumerate() {
        let amb = ambiguity(&o.guid, &oc, &nc);
        let key = if amb.is_none() {
            (None, o.guid.as_str())
        } else {
            (Some(o.slot.as_str()), o.guid.as_str())
        };
        let Some(&j) = new_by_key.get(&key) else {
            out.push(lone(&oo, i, ObjectStatus::Removed, amb));
            continue;
        };
        new_taken[j] = true;
        let n = &no[j];
        let moved = (o.parent_guid() != n.parent_guid()).then(|| Move {
            from: o.parent.clone(),
            to: n.parent.clone(),
        });
        let changes = object_changes(o.map, n.map, options);
        if moved.is_none() && changes.is_empty() {
            continue;
        }
        out.push(ObjectDiff {
            status: if moved.is_some() {
                ObjectStatus::Moved
            } else {
                ObjectStatus::Changed
            },
            guid: n.guid.clone(),
            nickname: n.nickname.clone(),
            ambiguity: amb,
            moved,
            changes,
        });
    }
    for (j, taken) in new_taken.iter().enumerate() {
        if !taken {
            let amb = ambiguity(&no[j].guid, &oc, &nc);
            out.push(lone(&no, j, ObjectStatus::Added, amb));
        }
    }
    out.sort_by_cached_key(|d| {
        (
            d.status,
            d.guid.clone(),
            d.nickname.clone(),
            format!("{:?}{:?}", d.moved, d.changes),
        )
    });

    let mut dups = Vec::new();
    for guid in oc.keys().chain(nc.keys()).copied() {
        let (o, n) = (
            oc.get(guid).copied().unwrap_or(0),
            nc.get(guid).copied().unwrap_or(0),
        );
        if (o > 1 || n > 1) && !dups.iter().any(|d: &DuplicateGuid| d.guid == guid) {
            dups.push(DuplicateGuid {
                guid: guid.to_owned(),
                old_count: o,
                new_count: n,
            });
        }
    }
    dups.sort_by(|a, b| a.guid.cmp(&b.guid));
    (out, dups)
}

impl Obj<'_> {
    /// The GUID of the parent, `None` for the top level.
    fn parent_guid(&self) -> Option<&str> {
        match &self.parent {
            ParentRef::TopLevel => None,
            ParentRef::Object { guid, .. } => Some(guid),
        }
    }
}
