//! Reading the scripts of the loaded game and planning a Save & Play safely.
//!
//! Save & Play deletes the script or UI of every listed object when the field
//! is missing or null in the message, and reloads the whole game. A
//! [`ScriptPlan`] therefore always builds the message from the *live* state:
//! only changed objects are listed, and each carries both fields in full.

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::time::Duration;

use crate::message::{Incoming, Outgoing, ScriptState};
use crate::transport::{EditorError, Listener, send};

/// Ask the game for all scripts and wait for the answer.
///
/// `limit` bounds the send and the wait for the reply. Other messages that
/// arrive meanwhile (print, errors, ...) are skipped.
pub async fn fetch_scripts(
    listener: &Listener,
    game: SocketAddr,
    limit: Duration,
) -> Result<Vec<ScriptState>, EditorError> {
    send(game, &Outgoing::GetScripts, limit).await?;
    let wait = async {
        loop {
            match listener.next_message(limit).await {
                Ok(Incoming::LoadGame { script_states, .. }) => return Ok(script_states),
                Ok(_) => {}
                // A broken unrelated message must not hide the reply.
                Err(EditorError::Codec(_)) => {}
                Err(e) => return Err(e),
            }
        }
    };
    match tokio::time::timeout(limit, wait).await {
        Ok(r) => r,
        Err(_) => Err(EditorError::Timeout(limit)),
    }
}

/// The script and UI wanted for one object. `None` or an empty string means
/// "no script" / "no UI".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Desired {
    pub guid: String,
    pub script: Option<String>,
    pub ui: Option<String>,
}

/// What happens to one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldChange {
    Unchanged,
    /// The object had no text, now it has.
    Add {
        new: String,
    },
    Modify {
        old: String,
        new: String,
    },
    /// The object had text, now it has none.
    Delete {
        old: String,
    },
}

impl FieldChange {
    pub fn is_change(&self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectChange {
    pub guid: String,
    pub name: String,
    pub script: FieldChange,
    pub ui: FieldChange,
    /// False if the game does not list the object (it has no script or UI yet).
    /// The game ignores a GUID that does not exist, without an error.
    pub listed: bool,
    /// The complete state that will be sent for this object.
    pub state: ScriptState,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanError {
    #[error(
        "GUID {0} appears more than once in the source with different scripts or UI; \
         refusing to guess which one to send"
    )]
    AmbiguousGuid(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScriptPlan {
    /// Objects that change, in the order the game listed them.
    pub changes: Vec<ObjectChange>,
    /// Objects the game listed that the source does not contain; untouched.
    pub untouched: usize,
}

fn text(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// Line endings are not a change: the game stores CRLF scripts that a tree
/// packed with LF would otherwise show as completely rewritten.
fn same(a: &str, b: &str) -> bool {
    a.replace("\r\n", "\n") == b.replace("\r\n", "\n")
}

fn field(live: &Option<String>, want: &Option<String>) -> FieldChange {
    match (text(live), text(want)) {
        (None, None) => FieldChange::Unchanged,
        (None, Some(new)) => FieldChange::Add { new: new.into() },
        (Some(old), None) => FieldChange::Delete { old: old.into() },
        (Some(old), Some(new)) if same(old, new) => FieldChange::Unchanged,
        (Some(old), Some(new)) => FieldChange::Modify {
            old: old.into(),
            new: new.into(),
        },
    }
}

/// A changed field takes the new text, an unchanged one is sent exactly as the
/// game has it. A deleted or absent field is sent as "", never omitted.
fn full(change: &FieldChange, live: &Option<String>) -> Option<String> {
    Some(match change {
        FieldChange::Unchanged => live.clone().unwrap_or_default(),
        FieldChange::Add { new } | FieldChange::Modify { new, .. } => new.clone(),
        FieldChange::Delete { .. } => String::new(),
    })
}

/// Compare the live state of the game with what is wanted.
pub fn plan(live: &[ScriptState], desired: &[Desired]) -> Result<ScriptPlan, PlanError> {
    let mut wanted: BTreeMap<&str, &Desired> = BTreeMap::new();
    for d in desired {
        match wanted.get(d.guid.as_str()) {
            Some(prev) => {
                let equal = !field(&prev.script, &d.script).is_change()
                    && !field(&prev.ui, &d.ui).is_change();
                if !equal {
                    return Err(PlanError::AmbiguousGuid(d.guid.clone()));
                }
            }
            None => {
                wanted.insert(&d.guid, d);
            }
        }
    }
    let mut plan = ScriptPlan::default();
    for l in live {
        let Some(d) = wanted.get(l.guid.as_str()) else {
            plan.untouched += 1;
            continue;
        };
        let script = field(&l.script, &d.script);
        let ui = field(&l.ui, &d.ui);
        if !script.is_change() && !ui.is_change() {
            continue;
        }
        plan.changes.push(ObjectChange {
            guid: l.guid.clone(),
            name: l.name.clone(),
            listed: true,
            state: ScriptState {
                name: l.name.clone(),
                guid: l.guid.clone(),
                script: full(&script, &l.script),
                ui: full(&ui, &l.ui),
            },
            script,
            ui,
        });
    }
    // Objects the game does not list have no script or UI yet. A first script
    // is accepted by the game (verified); an object with nothing to add is
    // skipped.
    let live_guids: HashSet<&str> = live.iter().map(|l| l.guid.as_str()).collect();
    let mut seen = HashSet::new();
    for d in desired {
        if live_guids.contains(d.guid.as_str()) || !seen.insert(d.guid.as_str()) {
            continue;
        }
        let none = None;
        let script = field(&none, &d.script);
        let ui = field(&none, &d.ui);
        if !script.is_change() && !ui.is_change() {
            continue;
        }
        plan.changes.push(ObjectChange {
            guid: d.guid.clone(),
            name: String::new(),
            listed: false,
            state: ScriptState {
                name: String::new(),
                guid: d.guid.clone(),
                script: full(&script, &none),
                ui: full(&ui, &none),
            },
            script,
            ui,
        });
    }
    Ok(plan)
}

impl ScriptPlan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// True if any field loses its text.
    pub fn deletes(&self) -> bool {
        self.changes.iter().any(|c| {
            matches!(c.script, FieldChange::Delete { .. })
                || matches!(c.ui, FieldChange::Delete { .. })
        })
    }

    /// The Save & Play message: every changed object with both fields.
    pub fn message(&self) -> Outgoing {
        Outgoing::SaveAndPlay {
            script_states: self.changes.iter().map(|c| c.state.clone()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(guid: &str, script: Option<&str>, ui: Option<&str>) -> ScriptState {
        ScriptState {
            name: format!("obj {guid}"),
            guid: guid.into(),
            script: script.map(Into::into),
            ui: ui.map(Into::into),
        }
    }
    fn want(guid: &str, script: Option<&str>, ui: Option<&str>) -> Desired {
        Desired {
            guid: guid.into(),
            script: script.map(Into::into),
            ui: ui.map(Into::into),
        }
    }

    #[test]
    fn unchanged_objects_are_not_sent() {
        let p = plan(
            &[live("a", Some("x"), Some("<ui/>"))],
            &[want("a", Some("x"), Some("<ui/>"))],
        )
        .unwrap();
        assert!(p.is_empty());
        assert_eq!(p.untouched, 0);
    }

    #[test]
    fn crlf_versus_lf_is_not_a_change() {
        let p = plan(
            &[live("a", Some("a\r\nb"), None)],
            &[want("a", Some("a\nb"), None)],
        )
        .unwrap();
        assert!(p.is_empty());
    }

    #[test]
    fn changed_script_sends_live_ui_untouched() {
        let p = plan(
            &[live("a", Some("old"), Some("<live/>"))],
            &[want("a", Some("new"), Some("<live/>"))],
        )
        .unwrap();
        let Outgoing::SaveAndPlay { script_states } = p.message() else {
            panic!()
        };
        assert_eq!(script_states, vec![live("a", Some("new"), Some("<live/>"))]);
    }

    #[test]
    fn ui_not_in_source_is_deleted_and_sent_as_empty_not_omitted() {
        let p = plan(
            &[live("a", Some("s"), Some("<ui/>"))],
            &[want("a", Some("s2"), None)],
        )
        .unwrap();
        assert!(p.deletes());
        let c = &p.changes[0];
        assert_eq!(
            c.ui,
            FieldChange::Delete {
                old: "<ui/>".into()
            }
        );
        assert_eq!(c.state.ui.as_deref(), Some(""));
    }

    #[test]
    fn every_listed_object_has_both_fields_in_the_message() {
        let p = plan(&[live("a", Some("s"), None)], &[want("a", Some("t"), None)]).unwrap();
        let json = p.message().to_value();
        let st = &json["scriptStates"][0];
        assert_eq!(st["script"], "t");
        assert_eq!(st["ui"], "");
    }

    #[test]
    fn unlisted_objects_get_a_first_script_only_if_they_have_one() {
        let p = plan(
            &[live("a", Some("s"), None), live("b", Some("s"), None)],
            &[want("a", Some("s"), None), want("c", Some("n"), None)],
        )
        .unwrap();
        assert_eq!(p.untouched, 1);
        // "c" is not listed by the game but has a script: it is sent as a first script.
        assert_eq!(p.changes.len(), 1);
        assert!(!p.changes[0].listed);
        assert_eq!(p.changes[0].script, FieldChange::Add { new: "n".into() });
        assert_eq!(p.changes[0].state.ui.as_deref(), Some(""));
        // an unlisted object with nothing to add is skipped
        let p = plan(&[], &[want("d", Some(""), None)]).unwrap();
        assert!(p.is_empty());
    }

    #[test]
    fn added_ui_is_not_a_delete() {
        let p = plan(
            &[live("a", Some("s"), None)],
            &[want("a", Some("s"), Some("<ui/>"))],
        )
        .unwrap();
        assert!(!p.deletes());
        assert_eq!(
            p.changes[0].ui,
            FieldChange::Add {
                new: "<ui/>".into()
            }
        );
    }

    #[test]
    fn duplicate_guid_with_different_text_is_an_error() {
        let err = plan(
            &[live("a", Some("s"), None)],
            &[want("a", Some("1"), None), want("a", Some("2"), None)],
        )
        .unwrap_err();
        assert_eq!(err, PlanError::AmbiguousGuid("a".into()));
        plan(
            &[live("a", Some("s"), None)],
            &[want("a", Some("1"), None), want("a", Some("1"), None)],
        )
        .unwrap();
    }
}
