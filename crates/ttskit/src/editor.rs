//! `ttskit editor ...` commands: talk to a running Tabletop Simulator through
//! the External Editor API (unofficial).

use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use similar::TextDiff;
use ttskit_editor::{
    ConsoleEvent, Desired, EditorError, FieldChange, Listener, ObjectChange, ScriptPlan,
    ScriptState, send,
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

fn runtime() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::from(format!("cannot start async runtime: {e}")))
}

/// A command failure. `game_unreachable` selects exit status 3: the game did
/// not accept a connection (it is not running, or not listening).
#[derive(Debug)]
pub struct CliError {
    pub message: String,
    pub game_unreachable: bool,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self {
            message,
            game_unreachable: false,
        }
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

fn editor_err(e: EditorError) -> CliError {
    CliError {
        game_unreachable: matches!(e, EditorError::Connect { .. }),
        message: e.to_string(),
    }
}

fn len_of(s: &Option<String>) -> usize {
    s.as_deref().map_or(0, str::len)
}

/// `ttskit editor get-scripts`: list (or with `json`, dump) the scripts and UI
/// of the loaded game. Read-only.
pub fn get_scripts(ep: Endpoints, json: bool, out: &mut dyn Write) -> Result<(), CliError> {
    let rt = runtime()?;
    let scripts = rt.block_on(async {
        let listener = Listener::bind(ep.editor).await?;
        ttskit_editor::fetch_scripts(&listener, ep.game, ep.timeout).await
    });
    let scripts = scripts.map_err(editor_err)?;
    write_scripts(&scripts, json, out).map_err(|e| e.to_string().into())
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
) -> Result<(), CliError> {
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
) -> Result<(), CliError> {
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

// ---- exec and send ------------------------------------------------------

/// The text of a Lua error as the game would show it. The game's prefix
/// already contains the message for Lua errors, but not for "unknown GUID".
fn error_text(e: &ttskit_editor::LuaError) -> String {
    let text = if e.prefix.contains(&e.error) {
        e.prefix.clone()
    } else {
        format!("{}{}", e.prefix, e.error)
    };
    if e.prefix.starts_with("Error finding guid") {
        format!("{text} (the object must exist and already have a script)")
    } else if e.error == "Object reference not set to an instance of an object" {
        // Observed for an existing object that has no script yet.
        format!(
            "{text} (the object most likely has no script; the game can only execute code in an object that already has one)"
        )
    } else {
        text
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn exec_json(guid: &str, outcome: &ttskit_editor::ExecOutcome) -> Value {
    let mut doc = json!({
        "guid": guid,
        "prints": outcome.prints,
        "errors": outcome.errors.iter().map(|e| json!({
            "guid": e.guid,
            "error": e.error,
            "message": error_text(e),
        })).collect::<Vec<_>>(),
        "ok": outcome.errors.is_empty(),
    });
    // Absent when the game sent none, so "no value" differs from a returned null.
    if let Some(v) = &outcome.return_value {
        doc["returnValue"] = v.clone();
    }
    doc
}

/// `ttskit editor exec`: run Lua and print prints and the return value. A Lua
/// error is returned as `Err` after the output (also with `json`).
pub fn exec(
    code: &str,
    guid: &str,
    wait: Duration,
    json: bool,
    ep: Endpoints,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let rt = runtime()?;
    rt.block_on(async {
        let listener = Listener::bind(ep.editor).await.map_err(editor_err)?;
        exec_with(&listener, code, guid, wait, json, ep, out).await
    })
}

pub async fn exec_with(
    listener: &Listener,
    code: &str,
    guid: &str,
    wait: Duration,
    json: bool,
    ep: Endpoints,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let io = |e: std::io::Error| e.to_string();
    let outcome = ttskit_editor::execute_lua(listener, ep.game, guid, code, ep.timeout, wait)
        .await
        .map_err(editor_err)?;
    if json {
        let text =
            serde_json::to_string_pretty(&exec_json(guid, &outcome)).map_err(|e| e.to_string())?;
        writeln!(out, "{text}").map_err(io)?;
    } else {
        for p in &outcome.prints {
            writeln!(out, "{p}").map_err(io)?;
        }
        if let Some(v) = &outcome.return_value {
            writeln!(out, "{}", value_text(v)).map_err(io)?;
        }
    }
    match outcome.errors.as_slice() {
        [] => Ok(()),
        errors => Err(errors
            .iter()
            .map(error_text)
            .collect::<Vec<_>>()
            .join(
                "
",
            )
            .into()),
    }
}

/// `ttskit editor send`: send a custom message (a JSON object) to the game.
pub fn send_message(
    message: &str,
    json: bool,
    ep: Endpoints,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let value: Value =
        serde_json::from_str(message).map_err(|e| format!("the message is not valid JSON: {e}"))?;
    let rt = runtime()?;
    rt.block_on(ttskit_editor::send_custom(ep.game, value, ep.timeout))
        .map_err(editor_err)?;
    if json {
        writeln!(out, "{}", json!({ "sent": true })).map_err(|e| e.to_string().into())
    } else {
        writeln!(out, "Sent; the game does not acknowledge custom messages.")
            .map_err(|e| e.to_string().into())
    }
}

// ---- console -----------------------------------------------------------

/// How long `console` listens.
#[derive(Debug, Clone, Copy)]
pub enum ConsoleMode {
    /// Stop after `quiet` without a message, or after the total timeout.
    Once { quiet: Duration },
    /// Run until stopped (Ctrl+C).
    Follow,
}

fn event_json(at: &jiff::Timestamp, event: &ConsoleEvent) -> Value {
    match event {
        ConsoleEvent::Print { message } => {
            json!({ "time": at.to_string(), "type": "print", "message": message })
        }
        ConsoleEvent::Error(e) => json!({
            "time": at.to_string(),
            "type": "error",
            "guid": e.guid,
            "error": e.error,
            "message": error_text(e),
        }),
    }
}

fn write_event(
    at: jiff::Timestamp,
    event: &ConsoleEvent,
    json: bool,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    if json {
        writeln!(out, "{}", event_json(&at, event))?;
    } else {
        let time = at
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%H:%M:%S%.3f");
        match event {
            ConsoleEvent::Print { message } => writeln!(out, "{time} print  {message}")?,
            ConsoleEvent::Error(e) => {
                writeln!(out, "{time} error  [{}] {}", e.guid, error_text(e))?
            }
        }
    }
    // A followed stream must show up at once, also when stdout is a pipe.
    out.flush()
}

/// `ttskit editor console`: print the game's `print` output and Lua errors as
/// they arrive, stamped with the time they reached this program (the game
/// sends no time). Only messages that arrive while this runs are seen. Ends
/// when `stop` completes. Lua errors are output, not failures.
pub fn console(
    mode: ConsoleMode,
    json: bool,
    ep: Endpoints,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let rt = runtime()?;
    rt.block_on(async {
        let listener = Listener::bind(ep.editor).await.map_err(editor_err)?;
        let stop = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        console_with(&listener, mode, json, ep, out, stop).await
    })
}

pub async fn console_with(
    listener: &Listener,
    mode: ConsoleMode,
    json: bool,
    ep: Endpoints,
    out: &mut dyn Write,
    stop: impl Future<Output = ()>,
) -> Result<(), CliError> {
    let io = |e: std::io::Error| e.to_string();
    tokio::pin!(stop);
    let deadline = tokio::time::Instant::now() + ep.timeout;
    loop {
        let next = listener.next_message(ep.timeout);
        let result = match mode {
            ConsoleMode::Follow => tokio::select! {
                r = next => r,
                () = &mut stop => return Ok(()),
            },
            ConsoleMode::Once { quiet } => {
                let until = (tokio::time::Instant::now() + quiet).min(deadline);
                tokio::select! {
                    r = tokio::time::timeout_at(until, next) => match r {
                        Ok(r) => r,
                        Err(_) => return Ok(()),
                    },
                    () = &mut stop => return Ok(()),
                }
            }
        };
        match result {
            Ok(message) => {
                if let Some(event) = ConsoleEvent::from_incoming(message) {
                    write_event(jiff::Timestamp::now(), &event, json, out).map_err(io)?;
                }
            }
            // One bad message must not end a stream.
            Err(
                e @ (EditorError::Codec(_)
                | EditorError::Timeout(_)
                | EditorError::TooLarge
                | EditorError::NotUtf8),
            ) => {
                eprintln!("warning: skipped a message: {e}");
            }
            Err(e) => return Err(editor_err(e)),
        }
    }
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
    ) -> (Result<(), CliError>, String, Vec<Value>) {
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
        assert!(r.unwrap_err().message.contains("--yes"));
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
        assert!(r.unwrap_err().message.contains("--allow-delete"));
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
        assert!(r.unwrap_err().message.contains("more than once"));
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

    async fn exec_run(replies: Vec<Value>, json: bool) -> (Result<(), CliError>, String) {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let editor = listener.local_addr().unwrap();
        let game = FakeGame::start_with(
            editor,
            vec![],
            Box::new(move |m| {
                if m["messageID"] == 3 {
                    replies.clone()
                } else {
                    vec![]
                }
            }),
        )
        .await
        .unwrap();
        let ep = Endpoints {
            game: game.addr(),
            editor,
            timeout: Duration::from_secs(5),
        };
        let mut out = Vec::new();
        let wait = Duration::from_millis(150);
        let r = exec_with(&listener, "x", "-1", wait, json, ep, &mut out).await;
        (r, String::from_utf8(out).unwrap())
    }

    #[tokio::test]
    async fn exec_prints_output_then_the_return_value() {
        let (r, out) = exec_run(
            vec![
                json!({"messageID": 2, "message": "hi"}),
                json!({"messageID": 5, "returnValue": "done"}),
            ],
            false,
        )
        .await;
        r.unwrap();
        assert_eq!(out, "hi\ndone\n");
    }

    #[tokio::test]
    async fn exec_prints_non_string_values_as_json() {
        let (_, out) = exec_run(
            vec![json!({"messageID": 5, "returnValue": {"a": 1}})],
            false,
        )
        .await;
        assert_eq!(out, "{\"a\":1}\n");
    }

    #[tokio::test]
    async fn exec_with_no_reply_succeeds_silently() {
        let (r, out) = exec_run(vec![], false).await;
        r.unwrap();
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn exec_lua_error_fails_after_printing_output() {
        let (r, out) = exec_run(
            vec![
                json!({"messageID": 2, "message": "before"}),
                json!({"messageID": 3, "guid": "-1", "error": "boom",
                       "errorMessagePrefix": "[Global] Lua Error <executeScript>: boom"}),
            ],
            false,
        )
        .await;
        assert!(
            r.unwrap_err()
                .message
                .contains("Lua Error <executeScript>: boom")
        );
        assert_eq!(out, "before\n");
    }

    #[tokio::test]
    async fn exec_unknown_guid_error_explains_the_script_requirement() {
        let (r, _) = exec_run(
            vec![json!({"messageID": 3, "guid": "-1", "error": "zzz",
                        "errorMessagePrefix": "Error finding guid for execute script: "})],
            false,
        )
        .await;
        let e = r.unwrap_err().message;
        assert!(
            e.contains("zzz") && e.contains("already have a script"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn exec_on_an_object_without_a_script_explains_the_cryptic_game_error() {
        let (r, _) = exec_run(
            vec![json!({"messageID": 3, "guid": "a53de1",
                        "error": "Object reference not set to an instance of an object",
                        "errorMessagePrefix": "[Hand Trigger - a53de1] Lua Error <executeScript>: Object reference not set to an instance of an object"})],
            false,
        )
        .await;
        assert!(r.unwrap_err().message.contains("has no script"));
    }

    #[tokio::test]
    async fn exec_json_distinguishes_no_value_from_null_and_lists_errors() {
        let (r, out) = exec_run(vec![json!({"messageID": 5, "returnValue": null})], true).await;
        r.unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.as_object().unwrap().contains_key("returnValue"));
        assert_eq!(v["ok"], true);

        let (_, out) = exec_run(vec![], true).await;
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(!v.as_object().unwrap().contains_key("returnValue"));

        let (r, out) = exec_run(
            vec![json!({"messageID": 3, "guid": "-1", "error": "boom",
                        "errorMessagePrefix": "P: boom"})],
            true,
        )
        .await;
        assert!(r.is_err());
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["errors"][0]["error"], "boom");
    }

    #[test]
    fn send_message_rejects_invalid_json_and_non_objects_without_connecting() {
        let ep = Endpoints::new(1, 2);
        let mut out = Vec::new();
        let e = send_message("{nope", false, ep, &mut out)
            .unwrap_err()
            .message;
        assert!(e.contains("not valid JSON"), "{e}");
        let e = send_message("[1]", false, ep, &mut out)
            .unwrap_err()
            .message;
        assert!(e.contains("JSON object"), "{e}");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn send_message_delivers_the_object() {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let editor = listener.local_addr().unwrap();
        let game = FakeGame::start(editor, vec![]).await.unwrap();
        let ep = Endpoints {
            game: game.addr(),
            editor,
            timeout: Duration::from_secs(5),
        };
        // send_message owns a runtime, so run it off the async thread.
        let out = std::thread::spawn(move || {
            let mut out = Vec::new();
            send_message("{\"a\":1}", true, ep, &mut out).map(|_| out)
        })
        .join()
        .unwrap()
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap().trim(), "{\"sent\":true}");
        for _ in 0..40 {
            if !game.received().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(game.received()[0]["customMessage"], json!({"a": 1}));
    }

    async fn console_run(
        mode: ConsoleMode,
        json: bool,
        docs: Vec<Value>,
        stop_after: Option<Duration>,
    ) -> (Result<(), CliError>, String) {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let editor = listener.local_addr().unwrap();
        let game = FakeGame::start(editor, vec![]).await.unwrap();
        for doc in &docs {
            game.push_raw(editor, doc).await.unwrap();
        }
        let ep = Endpoints {
            game: game.addr(),
            editor,
            timeout: Duration::from_secs(5),
        };
        let mut out = Vec::new();
        let stop = async move {
            match stop_after {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
        };
        let r = console_with(&listener, mode, json, ep, &mut out, stop).await;
        (r, String::from_utf8(out).unwrap())
    }

    const QUIET: ConsoleMode = ConsoleMode::Once {
        quiet: Duration::from_millis(200),
    };

    fn print_doc(m: &str) -> Value {
        json!({"messageID": 2, "message": m})
    }

    fn error_doc(guid: &str, e: &str) -> Value {
        json!({"messageID": 3, "guid": guid, "error": e,
               "errorMessagePrefix": format!("[Obj] Lua Error: {e}")})
    }

    #[tokio::test]
    async fn console_prints_prints_and_errors_in_order_and_ignores_other_messages() {
        let (r, out) = console_run(
            QUIET,
            false,
            vec![
                print_doc("one"),
                json!({"messageID": 4, "customMessage": {"a": 1}}),
                error_doc("a53de1", "boom"),
                print_doc("two\nlines"),
            ],
            None,
        )
        .await;
        r.unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "{out}");
        let after_time = |l: &str| l.split_once(' ').unwrap().1.to_owned();
        assert_eq!(after_time(lines[0]), "print  one");
        assert_eq!(
            after_time(lines[1]),
            "error  [a53de1] [Obj] Lua Error: boom"
        );
        assert_eq!(after_time(lines[2]), "print  two");
        assert_eq!(lines[3], "lines");
        // HH:MM:SS.mmm
        let t = lines[0].split(' ').next().unwrap();
        assert_eq!(t.len(), 12, "{t}");
        assert_eq!(&t[2..3], ":");
        assert_eq!(&t[8..9], ".");
    }

    #[tokio::test]
    async fn console_json_is_one_object_per_line_with_a_utc_time() {
        let (r, out) = console_run(
            QUIET,
            true,
            vec![print_doc("hi"), error_doc("-1", "bad")],
            None,
        )
        .await;
        r.unwrap();
        let docs: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0]["type"], "print");
        assert_eq!(docs[0]["message"], "hi");
        assert!(docs[0]["time"].as_str().unwrap().ends_with('Z'));
        assert!(docs[0].get("guid").is_none());
        assert_eq!(docs[1]["type"], "error");
        assert_eq!(docs[1]["guid"], "-1");
        assert_eq!(docs[1]["error"], "bad");
        assert_eq!(docs[1]["message"], "[Obj] Lua Error: bad");
    }

    #[tokio::test]
    async fn console_with_no_output_succeeds_silently() {
        let (r, out) = console_run(QUIET, false, vec![], None).await;
        r.unwrap();
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn console_lua_errors_do_not_fail_the_command() {
        let (r, out) = console_run(QUIET, false, vec![error_doc("a", "x")], None).await;
        r.unwrap();
        assert!(out.contains("error"));
    }

    #[tokio::test]
    async fn console_skips_a_broken_message_and_keeps_going() {
        let (r, out) = console_run(
            QUIET,
            false,
            vec![json!({"messageID": 2, "message": 5}), print_doc("after")],
            None,
        )
        .await;
        r.unwrap();
        assert!(out.contains("print  after"), "{out}");
    }

    #[tokio::test]
    async fn console_follow_runs_until_stopped() {
        let started = std::time::Instant::now();
        let (r, out) = console_run(
            ConsoleMode::Follow,
            false,
            vec![print_doc("early")],
            Some(Duration::from_millis(600)),
        )
        .await;
        r.unwrap();
        assert!(out.contains("print  early"));
        // Longer than the quiet window of a one-shot run.
        assert!(started.elapsed() >= Duration::from_millis(550));
    }
}
