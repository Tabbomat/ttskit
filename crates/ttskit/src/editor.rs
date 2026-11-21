//! `ttskit editor ...` commands: talk to a running Tabletop Simulator through
//! the External Editor API (unofficial).

use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use similar::TextDiff;
use ttskit_editor::{
    Desired, EditorError, FieldChange, Listener, ObjectChange, ScriptPlan, ScriptState, send,
};

/// Where the two ends of the API live.
#[derive(Debug, Clone, Copy)]
pub struct Endpoints {
    /// The game (we send here).
    pub game: SocketAddr,
    /// Our listener address (the game sends here).
    pub editor: SocketAddr,
    pub timeout: Duration,
}

impl Endpoints {
    pub fn new(game_port: u16, editor_port: u16) -> Self {
        Self {
            game: ([127, 0, 0, 1], game_port).into(),
            editor: ([127, 0, 0, 1], editor_port).into(),
            timeout: ttskit_editor::DEFAULT_TIMEOUT,
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start async runtime: {e}"))
}

fn editor_err(e: EditorError) -> String {
    e.to_string()
}

fn len_of(s: &Option<String>) -> usize {
    s.as_deref().map_or(0, str::len)
}

/// `ttskit editor get-scripts`: list (or with `json`, dump) the scripts and UI
/// of the loaded game. Read-only.
pub fn get_scripts(ep: Endpoints, json: bool, out: &mut dyn Write) -> Result<(), String> {
    let rt = runtime()?;
    let scripts = rt.block_on(async {
        let listener = Listener::bind(ep.editor).await?;
        ttskit_editor::fetch_scripts(&listener, ep.game, ep.timeout).await
    });
    let scripts = scripts.map_err(editor_err)?;
    write_scripts(&scripts, json, out).map_err(|e| e.to_string())
}

fn write_scripts(scripts: &[ScriptState], json: bool, out: &mut dyn Write) -> std::io::Result<()> {
    if json {
        let doc = json!({ "scriptStates": scripts });
        return writeln!(out, "{}", serde_json::to_string_pretty(&doc)?);
    }
    for s in scripts {
        writeln!(
            out,
            "{:<8} {:<30} script {} bytes, ui {} bytes",
            s.guid,
            s.name,
            len_of(&s.script),
            len_of(&s.ui)
        )?;
    }
    writeln!(out, "{} objects with a script or UI", scripts.len())
}

// ---- reading the unpacked tree ------------------------------------------

/// Script and UI of every object in the unpacked tree at `dir`, by GUID. The
/// tree is packed in memory first, so markers, `--include` bundling and all
/// pack errors behave exactly as in `save pack`. The global script is GUID `-1`.
pub fn desired_from_tree(
    dir: &Path,
    includes: Vec<std::path::PathBuf>,
) -> Result<Vec<Desired>, String> {
    let options = ttskit_core::PackOptions {
        includes,
        ..ttskit_core::PackOptions::new(false)
    };
    let packed = ttskit_core::pack(dir, &options).map_err(|e| e.to_string())?;
    let save = ttskit_core::Save::from_slice(&packed.bytes).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    collect(save.value(), "-1", &mut found);
    Ok(found)
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn collect(node: &Value, guid: &str, found: &mut Vec<Desired>) {
    found.push(Desired {
        guid: guid.to_owned(),
        script: opt_str(node, "LuaScript"),
        ui: opt_str(node, "XmlUI"),
    });
    let objects = |key: &str| -> Vec<&Value> {
        node.get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    };
    let mut children: Vec<&Value> = Vec::new();
    for key in ["ObjectStates", "ContainedObjects", "ChildObjects"] {
        children.extend(objects(key));
    }
    if let Some(states) = node.get("States").and_then(Value::as_object) {
        children.extend(states.values());
    }
    for child in children {
        if let Some(g) = child.get("GUID").and_then(Value::as_str) {
            collect(child, g, found);
        }
    }
}

// ---- save-and-play ------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct SaveAndPlayOptions {
    pub dry_run: bool,
    pub yes: bool,
    pub allow_delete: bool,
    pub json: bool,
}

fn change_name(c: &FieldChange) -> &'static str {
    match c {
        FieldChange::Unchanged => "unchanged",
        FieldChange::Add { .. } => "add",
        FieldChange::Modify { .. } => "modify",
        FieldChange::Delete { .. } => "delete",
    }
}

fn write_field_diff(label: &str, change: &FieldChange, out: &mut dyn Write) -> std::io::Result<()> {
    let (old, new) = match change {
        FieldChange::Unchanged => return Ok(()),
        FieldChange::Add { new } => ("", new.as_str()),
        FieldChange::Modify { old, new } => (old.as_str(), new.as_str()),
        FieldChange::Delete { old } => (old.as_str(), ""),
    };
    // Compare without line-ending noise, as the plan does.
    let (old, new) = (old.replace("\r\n", "\n"), new.replace("\r\n", "\n"));
    writeln!(out, "  {label}: {}", change_name(change))?;
    let diff = TextDiff::from_lines(&old, &new);
    let text = diff
        .unified_diff()
        .context_radius(3)
        .header("game", "source")
        .to_string();
    for line in text.lines() {
        writeln!(out, "    {line}")?;
    }
    Ok(())
}

fn write_object(c: &ObjectChange, out: &mut dyn Write) -> std::io::Result<()> {
    if c.listed {
        writeln!(out, "{} ({})", c.name, c.guid)?;
    } else {
        writeln!(
            out,
            "({}) not listed by the game: no script or UI yet; a first one is added if the object exists",
            c.guid
        )?;
    }
    write_field_diff("script", &c.script, out)?;
    write_field_diff("ui", &c.ui, out)
}

fn write_plan(plan: &ScriptPlan, out: &mut dyn Write) -> std::io::Result<()> {
    for c in &plan.changes {
        write_object(c, out)?;
    }
    writeln!(
        out,
        "{} objects change, {} in the game are not in the source (left alone)",
        plan.changes.len(),
        plan.untouched
    )?;
    Ok(())
}

fn plan_json(plan: &ScriptPlan, sent: bool) -> Value {
    json!({
        "changes": plan.changes.iter().map(|c| json!({
            "guid": c.guid,
            "name": c.name,
            "listed": c.listed,
            "script": change_name(&c.script),
            "ui": change_name(&c.ui),
        })).collect::<Vec<_>>(),
        "untouched": plan.untouched,
        "sent": sent,
    })
}

/// `ttskit editor save-and-play`: compare the unpacked tree with the live game,
/// print the diff and, only with `--yes`, send Save & Play for the objects
/// that changed. `--dry-run` never sends.
///
/// `desired` comes from [`desired_from_tree`]. The listener is bound by the
/// caller so tests can use an ephemeral port.
pub fn save_and_play(
    desired: &[Desired],
    ep: Endpoints,
    options: SaveAndPlayOptions,
    out: &mut dyn Write,
) -> Result<(), String> {
    let rt = runtime()?;
    rt.block_on(async {
        let listener = Listener::bind(ep.editor).await.map_err(editor_err)?;
        save_and_play_with(&listener, desired, ep, options, out).await
    })
}

pub async fn save_and_play_with(
    listener: &Listener,
    desired: &[Desired],
    ep: Endpoints,
    options: SaveAndPlayOptions,
    out: &mut dyn Write,
) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let live = ttskit_editor::fetch_scripts(listener, ep.game, ep.timeout)
        .await
        .map_err(editor_err)?;
    let plan = ttskit_editor::plan(&live, desired).map_err(|e| e.to_string())?;
    let will_send = !options.dry_run && !plan.is_empty() && options.yes;
    if options.json {
        writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&plan_json(&plan, false)).map_err(|e| e.to_string())?
        )
        .map_err(io)?;
    } else {
        write_plan(&plan, out).map_err(io)?;
    }
    if plan.is_empty() {
        if !options.json {
            writeln!(out, "Nothing to do; the game is not reloaded.").map_err(io)?;
        }
        return Ok(());
    }
    if plan.deletes() && !options.allow_delete {
        return Err(
            "the source has no script or UI for objects that have one in the game, \
             so Save & Play would delete them (marked `delete` above); nothing was sent. \
             Add the missing files, or pass --allow-delete if that is intended"
                .into(),
        );
    }
    if options.dry_run {
        if !options.json {
            writeln!(out, "Dry run: nothing was sent.").map_err(io)?;
        }
        return Ok(());
    }
    if !will_send {
        return Err(
            "Save & Play reloads the whole game and discards unsaved changes in it; \
             nothing was sent. Review the changes above and run again with --yes"
                .into(),
        );
    }
    send(ep.game, &plan.message(), ep.timeout)
        .await
        .map_err(editor_err)?;
    if !options.json {
        writeln!(
            out,
            "Sent Save & Play for {} objects; the game reloads.",
            plan.changes.len()
        )
        .map_err(io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttskit_editor::fake::FakeGame;

    fn state(guid: &str, script: &str, ui: Option<&str>) -> ScriptState {
        ScriptState {
            name: format!("obj {guid}"),
            guid: guid.into(),
            script: Some(script.into()),
            ui: ui.map(Into::into),
        }
    }

    fn want(guid: &str, script: &str, ui: Option<&str>) -> Desired {
        Desired {
            guid: guid.into(),
            script: Some(script.into()),
            ui: ui.map(Into::into),
        }
    }

    async fn setup(live: Vec<ScriptState>) -> (Listener, FakeGame, Endpoints) {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let editor = listener.local_addr().unwrap();
        let game = FakeGame::start(editor, live).await.unwrap();
        let ep = Endpoints {
            game: game.addr(),
            editor,
            timeout: Duration::from_secs(5),
        };
        (listener, game, ep)
    }

    fn sent_save_and_play(game: &FakeGame) -> Vec<Value> {
        game.received()
            .into_iter()
            .filter(|v| v["messageID"] == 1)
            .collect()
    }

    async fn run(
        live: Vec<ScriptState>,
        desired: Vec<Desired>,
        options: SaveAndPlayOptions,
    ) -> (Result<(), String>, String, Vec<Value>) {
        let (listener, game, ep) = setup(live).await;
        let mut out = Vec::new();
        let r = save_and_play_with(&listener, &desired, ep, options, &mut out).await;
        // The fake logs a message after our send returns; give it a moment.
        let mut sent = sent_save_and_play(&game);
        for _ in 0..12 {
            if !sent.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            sent = sent_save_and_play(&game);
        }
        (r, String::from_utf8(out).unwrap(), sent)
    }

    const YES: SaveAndPlayOptions = SaveAndPlayOptions {
        dry_run: false,
        yes: true,
        allow_delete: false,
        json: false,
    };

    #[tokio::test]
    async fn without_yes_the_diff_is_shown_and_nothing_is_sent() {
        let opts = SaveAndPlayOptions { yes: false, ..YES };
        let (r, out, sent) = run(
            vec![state("a", "old\n", None)],
            vec![want("a", "new\n", None)],
            opts,
        )
        .await;
        assert!(r.unwrap_err().contains("--yes"));
        assert!(out.contains("-old") && out.contains("+new"), "{out}");
        assert!(sent.is_empty());
    }

    #[tokio::test]
    async fn dry_run_prints_the_diff_and_sends_nothing_even_with_yes() {
        let opts = SaveAndPlayOptions {
            dry_run: true,
            ..YES
        };
        let (r, out, sent) = run(
            vec![state("a", "old\n", None)],
            vec![want("a", "new\n", None)],
            opts,
        )
        .await;
        r.unwrap();
        assert!(out.contains("-old") && out.contains("+new"), "{out}");
        assert!(out.contains("Dry run"));
        assert!(sent.is_empty());
    }

    #[tokio::test]
    async fn yes_sends_only_changed_objects_with_full_state() {
        let (r, _, sent) = run(
            vec![
                state("a", "old", Some("<ui/>")),
                state("b", "same", None),
                state("c", "untouched", None),
            ],
            vec![want("a", "new", Some("<ui/>")), want("b", "same", None)],
            YES,
        )
        .await;
        r.unwrap();
        assert_eq!(sent.len(), 1);
        let states = sent[0]["scriptStates"].as_array().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0]["guid"], "a");
        assert_eq!(states[0]["script"], "new");
        assert_eq!(states[0]["ui"], "<ui/>");
    }

    #[tokio::test]
    async fn deletion_is_refused_even_with_yes() {
        // The source has no UI for an object whose UI exists in the game.
        let (r, out, sent) = run(
            vec![state("a", "s", Some("<ui/>"))],
            vec![want("a", "s2", None)],
            YES,
        )
        .await;
        assert!(r.unwrap_err().contains("--allow-delete"));
        assert!(out.contains("ui: delete"), "{out}");
        assert!(sent.is_empty());
    }

    #[tokio::test]
    async fn allowed_deletion_sends_an_explicit_empty_field() {
        let opts = SaveAndPlayOptions {
            allow_delete: true,
            ..YES
        };
        let (r, _, sent) = run(
            vec![state("a", "s", Some("<ui/>"))],
            vec![want("a", "s2", None)],
            opts,
        )
        .await;
        r.unwrap();
        let st = &sent[0]["scriptStates"][0];
        assert_eq!(st["ui"], "");
        assert_eq!(st["script"], "s2");
    }

    #[tokio::test]
    async fn nothing_changed_does_not_reload_the_game() {
        let (r, out, sent) = run(
            vec![state("a", "s\r\n", None)],
            vec![want("a", "s\n", None)],
            YES,
        )
        .await;
        r.unwrap();
        assert!(out.contains("Nothing to do"));
        assert!(sent.is_empty());
    }

    #[tokio::test]
    async fn ambiguous_guid_is_an_error_and_sends_nothing() {
        let (r, _, sent) = run(
            vec![state("a", "s", None)],
            vec![want("a", "1", None), want("a", "2", None)],
            YES,
        )
        .await;
        assert!(r.unwrap_err().contains("more than once"));
        assert!(sent.is_empty());
    }

    #[tokio::test]
    async fn json_output_lists_changes() {
        let opts = SaveAndPlayOptions {
            dry_run: true,
            json: true,
            ..YES
        };
        let (r, out, _) = run(
            vec![state("a", "old", None)],
            vec![want("a", "new", None)],
            opts,
        )
        .await;
        r.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["changes"][0]["script"], "modify");
        assert_eq!(v["sent"], false);
    }

    #[test]
    fn tree_walk_finds_global_nested_and_state_objects() {
        let save = json!({
            "LuaScript": "g", "XmlUI": "<g/>",
            "ObjectStates": [{
                "GUID": "a", "LuaScript": "a",
                "ContainedObjects": [{"GUID": "b", "XmlUI": "<b/>"}],
                "States": {"2": {"GUID": "c", "LuaScript": "c"}},
                "ChildObjects": [{"GUID": "d"}]
            }]
        });
        let mut found = Vec::new();
        collect(&save, "-1", &mut found);
        let guids: Vec<_> = found.iter().map(|d| d.guid.as_str()).collect();
        assert_eq!(guids, ["-1", "a", "b", "d", "c"]);
        assert_eq!(found[0].script.as_deref(), Some("g"));
        assert_eq!(found[2].ui.as_deref(), Some("<b/>"));
    }
}
