//! `ttskit mcp`: a Model Context Protocol server on stdio that exposes the
//! External Editor API to AI assistants (unofficial).
//!
//! Stdout carries only the protocol. Every tool call binds the editor port
//! for its own duration and releases it afterwards; a mutex serializes the
//! calls so that two of them never compete for the port.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::transport::IntoTransport;
use rmcp::{RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use ttskit_editor::{ConsoleEvent, EditorError, Listener};

use crate::editor::{CliError, Endpoints, editor_err, event_json, exec_json, runtime};

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
    /// GUIDs of the objects whose full script and UI are wanted ("-1" is
    /// Global). Omit for a summary of all objects.
    guids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ExecuteLuaArgs {
    /// The Lua code to run.
    code: String,
    /// GUID of the object to run in; default "-1" (Global).
    guid: Option<String>,
    /// Stop collecting output after this many milliseconds without a message
    /// (default 1000).
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
struct GetConsoleArgs {
    /// Stop after this many milliseconds without a message (default 1000).
    quiet_ms: Option<u64>,
    /// Stop after this many milliseconds in total (default: the server's
    /// --timeout; at most 60000).
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
        let ep = self.ep;
        let listener = Listener::bind(ep.editor).await.map_err(editor_err)?;
        let deadline = tokio::time::Instant::now() + max;
        let mut events = Vec::new();
        loop {
            let until = (tokio::time::Instant::now() + quiet).min(deadline);
            let next = listener.next_message(ep.timeout);
            match tokio::time::timeout_at(until, next).await {
                Err(_) => break,
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
        Ok(json!({ "events": events }))
    }
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
