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
use crate::transport::{EditorError, MessageSource, send};

/// The game's answer to "get scripts".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadedGame {
    /// Objects with a script or UI, in the order the game lists them.
    pub script_states: Vec<ScriptState>,
    /// Full path of the loaded save, if the game reported a non-empty one.
    pub save_path: Option<String>,
}

/// Ask the game for all scripts and wait for the answer.
///
/// `limit` bounds the send and the wait for the reply. Other messages that
/// arrive meanwhile (print, errors, ...) are skipped.
pub async fn fetch_scripts(
    listener: &impl MessageSource,
    game: SocketAddr,
    limit: Duration,
) -> Result<Vec<ScriptState>, EditorError> {
    Ok(fetch_game(listener, game, limit).await?.script_states)
}

/// Like [`fetch_scripts`], and also returns the path of the loaded save.
pub async fn fetch_game(
    listener: &impl MessageSource,
    game: SocketAddr,
    limit: Duration,
) -> Result<LoadedGame, EditorError> {
    send(game, &Outgoing::GetScripts, limit).await?;
    let wait = async {
        loop {
            match listener.next_message(limit).await {
                Ok(Incoming::LoadGame {
                    script_states,
                    save_path,
                }) => {
                    return Ok(LoadedGame {
                        script_states,
                        save_path: save_path.filter(|p| !p.is_empty()),
                    });
                }
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

/// Line endings are not a change: the game stores CRLF scripts that a tree
/// packed with LF would otherwise show as completely rewritten.
fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// The text the game keeps of `sent`: it drops exactly one trailing newline
/// (observed for scripts and UI, `"x\n\t\n"` is stored as `"x\n\t"`), so a
/// source file that ends with a newline matches the game's copy without it.
fn stored(sent: &str) -> String {
    let s = lf(sent);
    match s.strip_suffix('\n') {
        Some(t) => t.to_owned(),
        None => s,
    }
}

fn field(live: &Option<String>, want: &Option<String>) -> FieldChange {
    let live = live.as_deref().filter(|s| !s.is_empty());
    let want = want.as_deref().filter(|s| !stored(s).is_empty());
    match (live, want) {
        (None, None) => FieldChange::Unchanged,
        (None, Some(new)) => FieldChange::Add { new: new.into() },
        (Some(old), None) => FieldChange::Delete { old: old.into() },
        // Equal as written, or equal to what the game keeps: sending would
        // not change the game's text.
        (Some(old), Some(new)) if lf(old) == lf(new) || lf(old) == stored(new) => {
            FieldChange::Unchanged
        }
        (Some(old), Some(new)) => FieldChange::Modify {
            old: old.into(),
            new: new.into(),
        },
    }
}

/// Two source texts that the game would store the same way.
fn same_wanted(a: &Option<String>, b: &Option<String>) -> bool {
    let kept = |s: &Option<String>| s.as_deref().map(stored).unwrap_or_default();
    kept(a) == kept(b)
}

/// Global cannot lose its script or UI: the game puts its default UI back
/// for an empty one, and an empty Global script is not sent either. A
/// missing Global field therefore leaves the game's text alone.
fn keep_global(guid: &str, change: FieldChange) -> FieldChange {
    match change {
        FieldChange::Delete { .. } if guid == crate::exec::GLOBAL_GUID => FieldChange::Unchanged,
        other => other,
    }
}

/// The Global script as it is sent. Observed: Save & Play with an empty Global
/// script makes the game restore its default script and stop listening on its
/// editor port until Save & Play is pressed in the game. A single newline is
/// stored as an empty script (the game drops one trailing newline) and keeps
/// the port open, so it is sent instead of "".
fn global_script(guid: &str, script: Option<String>) -> Option<String> {
    match script {
        Some(s) if s.is_empty() && guid == crate::exec::GLOBAL_GUID => Some("\n".to_owned()),
        other => other,
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
                if !same_wanted(&prev.script, &d.script) || !same_wanted(&prev.ui, &d.ui) {
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
        let script = keep_global(&l.guid, field(&l.script, &d.script));
        let ui = keep_global(&l.guid, field(&l.ui, &d.ui));
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
                script: global_script(&l.guid, full(&script, &l.script)),
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
                script: global_script(&d.guid, full(&script, &none)),
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
    fn one_trailing_newline_the_game_drops_is_not_a_change() {
        // The game stores "x\n" as "x"; a trailing CRLF counts as one newline.
        for source in ["x\n", "x\r\n", "x"] {
            let p = plan(
                &[live("a", Some("x"), None)],
                &[want("a", Some(source), None)],
            )
            .unwrap();
            assert!(p.is_empty(), "{source:?}");
        }
        // Only one: the game keeps "x\n" of "x\n\n", so that is what matches.
        let p = plan(
            &[live("a", Some("x\n"), None)],
            &[want("a", Some("x\n\n"), None)],
        )
        .unwrap();
        assert!(p.is_empty());
        let p = plan(
            &[live("a", Some("x"), None)],
            &[want("a", Some("x\n\n"), None)],
        )
        .unwrap();
        assert_eq!(p.changes.len(), 1);
        // The text is sent as written; the game trims it.
        assert_eq!(p.changes[0].state.script.as_deref(), Some("x\n\n"));
    }

    #[test]
    fn a_source_that_is_only_a_newline_is_empty() {
        let p = plan(&[], &[want("a", Some("\n"), None)]).unwrap();
        assert!(p.is_empty());
        let p = plan(
            &[live("a", Some("s"), Some("<ui/>"))],
            &[want("a", Some("s"), Some("\n"))],
        )
        .unwrap();
        assert!(matches!(p.changes[0].ui, FieldChange::Delete { .. }));
    }

    #[test]
    fn duplicates_that_differ_only_in_the_trailing_newline_are_not_ambiguous() {
        plan(
            &[live("a", Some("s"), None)],
            &[want("a", Some("1\n"), None), want("a", Some("1"), None)],
        )
        .unwrap();
    }

    #[test]
    fn global_script_and_ui_missing_in_the_source_are_left_alone() {
        let g = crate::exec::GLOBAL_GUID;
        let p = plan(&[live(g, Some("s"), Some("<ui/>"))], &[want(g, None, None)]).unwrap();
        assert!(p.is_empty());
        assert!(!p.deletes());
        // A changed Global script still sends the live UI in full.
        let p = plan(
            &[live(g, Some("s"), Some("<ui/>"))],
            &[want(g, Some("t"), None)],
        )
        .unwrap();
        assert!(!p.deletes());
        assert_eq!(p.changes[0].ui, FieldChange::Unchanged);
        assert_eq!(p.changes[0].state.ui.as_deref(), Some("<ui/>"));
        // Objects other than Global are still deleted.
        let p = plan(&[live("a", Some("s"), None)], &[want("a", None, None)]).unwrap();
        assert!(p.deletes());
    }

    #[test]
    fn an_empty_global_script_is_never_sent_as_empty_text() {
        let g = crate::exec::GLOBAL_GUID;
        // The game's Global script is empty and only the UI changes.
        let p = plan(
            &[live(g, Some(""), Some("<a/>"))],
            &[want(g, None, Some("<b/>"))],
        )
        .unwrap();
        assert_eq!(p.changes[0].state.script.as_deref(), Some("\n"));
        let json = p.message().to_value();
        assert_eq!(json["scriptStates"][0]["script"], "\n");
        // Other objects still get "" for a deleted script.
        let p = plan(&[live("a", Some("s"), None)], &[want("a", None, None)]).unwrap();
        assert_eq!(p.changes[0].state.script.as_deref(), Some(""));
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

#[cfg(test)]
mod fetch_tests {
    use super::*;
    use crate::fake::FakeGame;
    use crate::transport::Listener;

    async fn fetch(save_path: Option<&str>) -> LoadedGame {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let editor = listener.local_addr().unwrap();
        let scripts = vec![ScriptState {
            name: "n".into(),
            guid: "a".into(),
            script: Some("s".into()),
            ui: None,
        }];
        let game = match save_path {
            Some(p) => FakeGame::start_with_save_path(editor, scripts, p).await,
            None => FakeGame::start(editor, scripts).await,
        }
        .unwrap();
        fetch_game(&listener, game.addr(), Duration::from_secs(5))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn fetch_game_returns_the_save_path_and_scripts() {
        let game = fetch(Some("saves/TS_Save_1.json")).await;
        assert_eq!(game.save_path.as_deref(), Some("saves/TS_Save_1.json"));
        assert_eq!(game.script_states.len(), 1);
    }

    #[tokio::test]
    async fn an_empty_save_path_is_none() {
        assert_eq!(fetch(None).await.save_path, None);
    }
}
