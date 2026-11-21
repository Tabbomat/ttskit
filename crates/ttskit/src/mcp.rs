//! `ttskit mcp`: a Model Context Protocol server on stdio that exposes the
//! External Editor API to AI assistants (unofficial).
//!
//! Stdout carries only the protocol. Every tool call binds the editor port
//! for its own duration and releases it afterwards; a mutex serializes the
//! calls so that two of them never compete for the port.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::transport::IntoTransport;
use rmcp::{RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use ttskit_editor::{ConsoleEvent, EditorError, FieldChange, Listener, ScriptPlan};

use crate::editor::{
    CliError, Endpoints, desired_from_tree, editor_err, event_json, exec_json, plan_json, runtime,
    write_plan,
};

/// Default quiet window of `execute_lua` and `get_console`, in milliseconds.
const DEFAULT_QUIET_MS: u64 = 1000;
/// Upper bound for the total listening time of `get_console`.
const MAX_CONSOLE_MS: u64 = 60_000;

const INSTRUCTIONS: &str = "Unofficial client for the Tabletop Simulator External Editor API \
(not affiliated with Berserk Games). The game must be running with its External Editor ports \
39999 (game) and 39998 (this server) available. The game sends its output (print, errors) \
without being asked, but this server only listens while a tool call runs: messages the game \
sends between tool calls are lost.";

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct GetScriptsArgs {
    /// GUIDs of the objects whose full script and UI are wanted ("-1" is Global). Omit for a summary of all objects.
    guids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ExecuteLuaArgs {
    /// The Lua code to run.
    code: String,
    /// GUID of the object to run in; default "-1" (Global).
    guid: Option<String>,
    /// Stop collecting output after this many milliseconds without a message (default 1000).
    quiet_ms: Option<u64>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SendCustomMessageArgs {
    /// The message: a JSON object.
    message: Map<String, Value>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SaveAndPlayArgs {
    /// The directory written by `ttskit save unpack`, relative to the server's root or absolute (must be inside the root).
    dir: String,
    /// Library directories for `require("Name")` and `<Include src="Name"/>`, as in `save pack --include` (must be inside the root).
    include: Option<Vec<String>>,
    /// The fingerprint from a previous call's plan; without it nothing is sent.
    confirm: Option<String>,
    /// Allow deleting scripts or UI that exist in the game but not in the source.
    allow_delete: Option<bool>,
    /// Keep collecting console output after sending until this many milliseconds without a message (default 1000).
    quiet_ms: Option<u64>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct GetConsoleArgs {
    /// Stop after this many milliseconds without a message (default 1000).
    quiet_ms: Option<u64>,
    /// Stop after this many milliseconds in total (default: the server's --timeout; at most 60000).
    max_ms: Option<u64>,
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("a JSON value serializes")
}

fn success(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(pretty(value))])
}

fn failure(value: &Value) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(pretty(value))])
}

fn failure_of(e: &CliError) -> CallToolResult {
    failure(&json!({ "error": e.message, "gameUnreachable": e.game_unreachable }))
}

/// Resolve `--root` to an absolute path; it must be an existing directory.
fn resolve_root(root: Option<&Path>) -> Result<PathBuf, CliError> {
    let root = root.unwrap_or_else(|| Path::new("."));
    let canonical = std::fs::canonicalize(root)
        .map_err(|e| CliError::from(format!("cannot use root {}: {e}", root.display())))?;
    if !canonical.is_dir() {
        return Err(format!("root {} is not a directory", root.display()).into());
    }
    Ok(strip_verbatim(canonical))
}

/// Windows canonical paths look like `\\?\C:\dir`; the plain form is what a
/// person (or an assistant) expects to see.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

#[derive(Clone)]
struct EditorServer {
    ep: Endpoints,
    root: PathBuf,
    /// Serializes tool calls: they all use the one editor port.
    lock: Arc<tokio::sync::Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

impl EditorServer {
    fn new(ep: Endpoints, root: PathBuf) -> Self {
        Self {
            ep,
            root,
            lock: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    async fn fetch(&self) -> Result<ttskit_editor::LoadedGame, EditorError> {
        let ep = self.ep;
        let listener = Listener::bind(ep.editor).await?;
        ttskit_editor::fetch_game(&listener, ep.game, ep.timeout).await
    }

    async fn status(&self) -> Value {
        let ep = self.ep;
        let fetched = self.fetch().await;
        let mut doc = json!({
            "gameReachable": fetched.is_ok(),
            "savePath": null,
            "objectsWithScripts": null,
            "gamePort": ep.game.port(),
            "editorPort": ep.editor.port(),
            "root": self.root.display().to_string(),
            "version": env!("CARGO_PKG_VERSION"),
        });
        match fetched {
            Ok(game) => {
                doc["savePath"] = json!(game.save_path);
                doc["objectsWithScripts"] = json!(game.script_states.len());
            }
            Err(e) => doc["error"] = json!(editor_err(e).message),
        }
        doc
    }

    async fn scripts(&self, guids: Option<Vec<String>>) -> Result<Value, CliError> {
        let game = self.fetch().await.map_err(editor_err)?;
        let text = |s: &Option<String>| s.as_deref().unwrap_or_default().to_owned();
        let Some(guids) = guids.filter(|g| !g.is_empty()) else {
            let objects: Vec<Value> = game
                .script_states
                .iter()
                .map(|s| {
                    json!({
                        "guid": s.guid,
                        "name": s.name,
                        "scriptBytes": s.script.as_deref().map_or(0, str::len),
                        "uiBytes": s.ui.as_deref().map_or(0, str::len),
                    })
                })
                .collect();
            return Ok(json!({ "savePath": game.save_path, "objects": objects }));
        };
        let objects: Vec<Value> = game
            .script_states
            .iter()
            .filter(|s| guids.contains(&s.guid))
            .map(|s| {
                json!({
                    "guid": s.guid,
                    "name": s.name,
                    "script": text(&s.script),
                    "ui": text(&s.ui),
                })
            })
            .collect();
        let missing: Vec<&String> = guids
            .iter()
            .filter(|g| !game.script_states.iter().any(|s| &s.guid == *g))
            .collect();
        Ok(json!({
            "savePath": game.save_path,
            "objects": objects,
            "missing": missing,
        }))
    }

    async fn console(&self, quiet: Duration, max: Duration) -> Result<Value, CliError> {
        let listener = Listener::bind(self.ep.editor).await.map_err(editor_err)?;
        let events = collect_console(&listener, self.ep.timeout, quiet, max).await?;
        Ok(json!({ "events": events }))
    }

    async fn save_and_play_call(&self, args: SaveAndPlayArgs) -> Result<CallToolResult, CliError> {
        let ep = self.ep;
        let dir = resolve_in_root(&self.root, Path::new(&args.dir))?;
        let includes = args
            .include
            .unwrap_or_default()
            .iter()
            .map(|p| resolve_in_root(&self.root, Path::new(p)))
            .collect::<Result<Vec<_>, _>>()?;
        let desired = desired_from_tree(&dir, includes)?;
        // One listener for the whole call, so that the reload's output is caught.
        let listener = Listener::bind(ep.editor).await.map_err(editor_err)?;
        let live = ttskit_editor::fetch_scripts(&listener, ep.game, ep.timeout)
            .await
            .map_err(editor_err)?;
        let plan = ttskit_editor::plan(&live, &desired).map_err(|e| e.to_string())?;
        if plan.is_empty() {
            return Ok(success(&json!({
                "changes": [],
                "untouched": plan.untouched,
                "sent": false,
                "message": "Nothing to do; the game is not reloaded.",
            })));
        }
        let print = fingerprint(&plan);
        let mut doc = plan_doc(&plan, false);
        let deletes = plan.deletes();
        let Some(confirm) = args.confirm else {
            let mut next = format!(
                "Review the diff. Save & Play reloads the whole game and discards unsaved changes in it. To send, call save_and_play again with confirm set to the fingerprint {print}"
            );
            if deletes {
                next.push_str(
                    " and allow_delete: true, since the source has no script or UI for objects that have one in the game (marked `delete`)",
                );
            }
            doc["next"] = json!(next);
            return Ok(success(&doc));
        };
        let refuse = |mut doc: Value, error: &str| {
            doc["error"] = json!(error);
            doc["gameUnreachable"] = json!(false);
            Ok(failure(&doc))
        };
        if confirm.trim() != print {
            return refuse(
                doc,
                "the plan changed since the fingerprint was issued (or the fingerprint is wrong); nothing was sent. Review the new plan and confirm its fingerprint",
            );
        }
        if deletes && args.allow_delete != Some(true) {
            return refuse(
                doc,
                "the source has no script or UI for objects that have one in the game, so Save & Play would delete them (marked `delete` in the diff); nothing was sent. Add the missing files, or pass allow_delete: true if that is intended",
            );
        }
        ttskit_editor::send(ep.game, &plan.message(), ep.timeout)
            .await
            .map_err(editor_err)?;
        // The game answers Save & Play with "saved" (ID 6), then "loaded"
        // (ID 1) when the reload is done, then the output of `onLoad`.
        let mut console = Vec::new();
        let reloaded = wait_for_reload(&listener, ep.timeout, &mut console).await?;
        let quiet = Duration::from_millis(args.quiet_ms.unwrap_or(DEFAULT_QUIET_MS));
        console.extend(collect_console(&listener, ep.timeout, quiet, ep.timeout).await?);
        doc["sent"] = json!(true);
        doc["reloaded"] = json!(reloaded);
        if !reloaded {
            doc["note"] = json!(format!(
                "the game did not report a reload within {} s; console output may be incomplete (call get_console)",
                ep.timeout.as_secs_f64()
            ));
        }
        doc["console"] = json!(console);
        Ok(success(&doc))
    }
}

/// Wait up to `limit` for the game's "loaded" message (ID 1) and collect the
/// console events that arrive before it. False if none came in time.
async fn wait_for_reload(
    listener: &Listener,
    limit: Duration,
    events: &mut Vec<Value>,
) -> Result<bool, CliError> {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        match tokio::time::timeout_at(deadline, listener.next_message(limit)).await {
            Err(_) => return Ok(false),
            Ok(Ok(ttskit_editor::Incoming::LoadGame { .. })) => return Ok(true),
            Ok(Ok(message)) => {
                if let Some(event) = ConsoleEvent::from_incoming(message) {
                    events.push(event_json(&jiff::Timestamp::now(), &event));
                }
            }
            Ok(Err(
                EditorError::Codec(_)
                | EditorError::Timeout(_)
                | EditorError::TooLarge
                | EditorError::NotUtf8,
            )) => {}
            Ok(Err(e)) => return Err(editor_err(e)),
        }
    }
}

/// Collect console events on `listener` until `quiet` of silence or `max` in
/// total. `read_limit` bounds the reading of one message.
async fn collect_console(
    listener: &Listener,
    read_limit: Duration,
    quiet: Duration,
    max: Duration,
) -> Result<Vec<Value>, CliError> {
    let deadline = tokio::time::Instant::now() + max;
    let mut events = Vec::new();
    loop {
        let until = (tokio::time::Instant::now() + quiet).min(deadline);
        match tokio::time::timeout_at(until, listener.next_message(read_limit)).await {
            Err(_) => return Ok(events),
            Ok(Ok(message)) => {
                if let Some(event) = ConsoleEvent::from_incoming(message) {
                    events.push(event_json(&jiff::Timestamp::now(), &event));
                }
            }
            // One bad message must not end the listening.
            Ok(Err(
                EditorError::Codec(_)
                | EditorError::Timeout(_)
                | EditorError::TooLarge
                | EditorError::NotUtf8,
            )) => {}
            Ok(Err(e)) => return Err(editor_err(e)),
        }
    }
}

/// Resolve a path the assistant gave: it must stay inside `root` (a canonical
/// path), also through symlinks, and must exist.
fn resolve_in_root(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(format!(
            "path {} contains `..`; use a path inside the project root {}",
            path.display(),
            root.display()
        ));
    }
    let joined = root.join(path); // an absolute path replaces the root
    let canonical = std::fs::canonicalize(&joined)
        .map(strip_verbatim)
        .map_err(|e| format!("cannot use path {}: {e}", joined.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "path {} is outside the project root {}; start `ttskit mcp --root DIR` to allow it",
            path.display(),
            root.display()
        ));
    }
    Ok(canonical)
}

fn field_json(change: &FieldChange) -> Value {
    match change {
        FieldChange::Unchanged => json!(["unchanged", null, null]),
        FieldChange::Add { new } => json!(["add", null, new]),
        FieldChange::Modify { old, new } => json!(["modify", old, new]),
        FieldChange::Delete { old } => json!(["delete", old, null]),
    }
}

/// Identifies a plan: the same changes against the same live text give the
/// same value, so a confirmation is only valid for the plan that was reviewed.
fn fingerprint(plan: &ScriptPlan) -> String {
    let doc: Vec<Value> = plan
        .changes
        .iter()
        .map(|c| json!([c.guid, c.listed, field_json(&c.script), field_json(&c.ui)]))
        .collect();
    let digest = Sha256::digest(Value::Array(doc).to_string().as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn plan_doc(plan: &ScriptPlan, sent: bool) -> Value {
    let mut doc = plan_json(plan, sent);
    let mut diff = Vec::new();
    write_plan(plan, &mut diff).expect("writing to a Vec cannot fail");
    doc["fingerprint"] = json!(fingerprint(plan));
    doc["deletes"] = json!(plan.deletes());
    doc["diff"] = json!(String::from_utf8_lossy(&diff));
    doc
}

#[tool_router]
impl EditorServer {
    #[tool(
        description = "Report whether the game answers, the loaded save, the ports in use and the project root. Always succeeds; check gameReachable."
    )]
    async fn get_status(&self) -> CallToolResult {
        let _guard = self.lock.lock().await;
        success(&self.status().await)
    }

    #[tool(
        description = "Read scripts and UI of the loaded game. Without guids: a summary (guid, name, byte sizes) of all objects. With guids: the full script and UI of those objects, and the guids the game does not list. The game lists only objects that have a script or UI; Global is GUID \"-1\"."
    )]
    async fn get_scripts(&self, Parameters(args): Parameters<GetScriptsArgs>) -> CallToolResult {
        let _guard = self.lock.lock().await;
        match self.scripts(args.guids).await {
            Ok(doc) => success(&doc),
            Err(e) => failure_of(&e),
        }
    }

    #[tool(
        description = "Run Lua code in the game. Runs in Global by default, or in the object with the given guid, which must already have a script. The game was not observed to answer `return`, so use print() for output. Output printed by other scripts while the call runs is included. A Lua error makes the result an error."
    )]
    async fn execute_lua(&self, Parameters(args): Parameters<ExecuteLuaArgs>) -> CallToolResult {
        let _guard = self.lock.lock().await;
        let ep = self.ep;
        let guid = args
            .guid
            .unwrap_or_else(|| ttskit_editor::GLOBAL_GUID.to_owned());
        let quiet = Duration::from_millis(args.quiet_ms.unwrap_or(DEFAULT_QUIET_MS));
        let outcome = async {
            let listener = Listener::bind(ep.editor).await?;
            ttskit_editor::execute_lua(&listener, ep.game, &guid, &args.code, ep.timeout, quiet)
                .await
        }
        .await;
        match outcome {
            Ok(outcome) if outcome.errors.is_empty() => success(&exec_json(&guid, &outcome)),
            Ok(outcome) => failure(&exec_json(&guid, &outcome)),
            Err(e) => failure_of(&editor_err(e)),
        }
    }

    #[tool(
        description = "Send the scripts and UI of an unpacked save directory to the running game with Save & Play, which reloads the whole game and discards unsaved changes in it. Only objects whose script or UI differ from the game are sent, each with both fields. The first call returns the diff and a fingerprint and sends nothing; call again with confirm set to that fingerprint to send. Deleting a script or UI the source lacks also needs allow_delete. The result includes the game's console output from the reload (reloaded: false if the game did not report one). Paths must be inside the server's root."
    )]
    async fn save_and_play(&self, Parameters(args): Parameters<SaveAndPlayArgs>) -> CallToolResult {
        let _guard = self.lock.lock().await;
        match self.save_and_play_call(args).await {
            Ok(result) => result,
            Err(e) => failure_of(&e),
        }
    }

    #[tool(
        description = "Send a custom message (a JSON object) to the game's onExternalMessage(table) handler. The game does not acknowledge it; success only means it was sent."
    )]
    async fn send_custom_message(
        &self,
        Parameters(args): Parameters<SendCustomMessageArgs>,
    ) -> CallToolResult {
        let _guard = self.lock.lock().await;
        let ep = self.ep;
        match ttskit_editor::send_custom(ep.game, Value::Object(args.message), ep.timeout).await {
            Ok(()) => success(&json!({ "sent": true })),
            Err(e) => failure_of(&editor_err(e)),
        }
    }

    #[tool(
        description = "Collect the game's print output and Lua errors. Listens until quiet_ms of silence or max_ms in total. Only messages that arrive during the call are returned; messages sent between calls are lost."
    )]
    async fn get_console(&self, Parameters(args): Parameters<GetConsoleArgs>) -> CallToolResult {
        let _guard = self.lock.lock().await;
        let quiet = Duration::from_millis(args.quiet_ms.unwrap_or(DEFAULT_QUIET_MS));
        let max = args
            .max_ms
            .map_or(self.ep.timeout, Duration::from_millis)
            .min(Duration::from_millis(MAX_CONSOLE_MS));
        match self.console(quiet, max).await {
            Ok(doc) => success(&doc),
            Err(e) => failure_of(&e),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for EditorServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new(
                "ttskit",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serve the editor tools on `transport` until the client disconnects.
async fn serve_on<T, E, A>(server: EditorServer, transport: T) -> Result<(), CliError>
where
    T: IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let running = server
        .serve(transport)
        .await
        .map_err(|e| CliError::from(format!("cannot start the MCP server: {e}")))?;
    running
        .waiting()
        .await
        .map_err(|e| CliError::from(format!("the MCP server stopped abnormally: {e}")))?;
    Ok(())
}

/// `ttskit mcp`: serve on stdin and stdout until stdin closes.
pub fn run(root: Option<&Path>, ep: Endpoints) -> Result<(), CliError> {
    let root = resolve_root(root)?;
    let server = EditorServer::new(ep, root);
    runtime()?.block_on(serve_on(server, rmcp::transport::stdio()))
}

#[cfg(test)]
mod tests;

#[allow(dead_code)] // wired into the server in the next change
mod buffer;
