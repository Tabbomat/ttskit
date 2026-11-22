//! `ttskit mcp`: a Model Context Protocol server on stdio that exposes the
//! External Editor API to AI assistants (unofficial).
//!
//! Stdout carries only the protocol. The server holds the editor port for
//! its whole life ([`hub`]): every message the game sends is read, events
//! (console output, errors, custom messages, saves and loads) are buffered
//! with sequence numbers, and the tool call that waits for replies gets them
//! too. A mutex serializes the tool calls that send requests, since the game
//! ties no reply to its request.

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
use ttskit_editor::{EditorError, FieldChange, MessageSource, ScriptPlan};

use self::buffer::{EventKind, Query};
use self::change_tools::{
    ImageBuildApplyArgs, ImageBuildArgs, RewriteUrlsApplyArgs, RewriteUrlsArgs,
};
use self::cloud_tools::{CloudListArgs, CloudStatusArgs};
use self::hub::{Hub, Replies};
use self::image_tools::ImageCheckArgs;
use self::save_tools::{
    ManifestShowArgs, SaveCheckUrlsArgs, SaveDiffArgs, SaveLintArgs, SaveScanArgs,
};

use crate::editor::{
    CliError, Endpoints, desired_from_tree, editor_err, exec_json, plan_json, runtime, write_plan,
};

/// Default quiet window of `execute_lua` and `get_console`, in milliseconds.
const DEFAULT_QUIET_MS: u64 = 1000;
/// Upper bound for the total waiting time of `get_console`.
const MAX_CONSOLE_MS: u64 = 60_000;
/// Default and upper bound of `get_console`'s `limit`.
const DEFAULT_CONSOLE_LIMIT: usize = 100;
const MAX_CONSOLE_LIMIT: usize = 1000;

const INSTRUCTIONS: &str = "Unofficial client for the Tabletop Simulator External Editor API \
(not affiliated with Berserk Games). The game must be running and listening on its External \
Editor port 39999; this server holds port 39998 for its whole life (if it is in use, \
the editor tools fail and say so; get_status reports editorAvailable false). Everything the game sends without being asked (print output, \
Lua errors, custom messages from sendExternalMessage, and the game being saved or loaded) is \
buffered with increasing sequence numbers (seq); read it with get_console and pass the returned \
nextSince as `since` next time to get only newer events. execute_lua and save_and_play report \
lastSeq for the same purpose. The save_scan, save_lint, save_diff, save_check_urls and \
manifest_show tools are read-only, work on files inside the server's root and need no game \
(only save_check_urls uses the network); their results are the JSON of the matching ttskit \
commands. The cloud_list and cloud_status tools (only in builds with Steam support) are read-only and open Steam for each call, so Steam must be running and logged in; if it is not, they fail with the reason. image_check (only in builds with image support) is local and read-only too. Tools that change files come in pairs: save_rewrite_urls_plan and image_build_plan (only with image support) change nothing and return a fingerprint; the matching *_apply tool needs the same arguments, that fingerprint and confirm: true, and refuses if anything changed since the plan. Nothing is deleted.";

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
    /// Only events with a seq greater than this (use nextSince of the previous result). Omit for the newest events in the buffer.
    since: Option<u64>,
    /// Only these kinds of events (default: all).
    kinds: Option<Vec<EventKind>>,
    /// At most this many events (default 100, at most 1000). With `since` the oldest matching ones come first and `more` says whether others follow; without it the newest ones.
    limit: Option<usize>,
    /// If no event matches yet, wait up to this many milliseconds for one (default 0: return at once; at most 60000).
    wait_ms: Option<u64>,
    /// After waiting brought a match, keep collecting until this many milliseconds pass without another (default 1000).
    quiet_ms: Option<u64>,
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
    /// Holds the editor port and buffers what the game sends; the error says
    /// why the port could not be bound. The save tools do not need it.
    hub: Result<Arc<Hub>, String>,
    /// Makes the HTTP client of `save_check_urls`.
    fetcher: save_tools::FetcherFactory,
    /// Opens the cloud backend per tool call; `None` in builds without Steam
    /// support, which then do not list the cloud tools.
    backend: Option<cloud_tools::BackendFactory>,
    /// Serializes all use of the cloud backend, held across a whole call.
    cloud_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serializes the tool calls that send requests: the game ties no reply
    /// to its request, so two of them must not wait at the same time.
    lock: Arc<tokio::sync::Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

impl EditorServer {
    /// Try to bind the editor port. The server starts either way: if the port
    /// is in use, the editor tools fail with the reason and the save tools work.
    async fn start(ep: Endpoints, root: PathBuf, max_events: usize) -> Self {
        let hub = Hub::start(ep.editor, ep.timeout, max_events)
            .await
            .map_err(|e| editor_err(e).message);
        Self {
            ep,
            root,
            hub,
            fetcher: save_tools::http_fetcher(),
            backend: cloud_tools::production_backend(),
            cloud_lock: Arc::default(),
            tool_router: Self::router(cloud_tools::production_backend().is_some()),
            lock: Arc::default(),
        }
    }

    /// All tools of this build: the cloud tools need a backend factory, the
    /// image tool needs the `image` feature; without them they are not listed.
    fn router(cloud: bool) -> ToolRouter<Self> {
        let router = cloud_tools::with_cloud_tools(Self::tool_router(), cloud);
        image_tools::with_image_tools(router, image_tools::available())
    }

    /// Use another cloud backend factory (tests); this lists the cloud tools.
    #[cfg(test)]
    fn with_backend(mut self, backend: cloud_tools::BackendFactory) -> Self {
        self.backend = Some(backend);
        self.tool_router = Self::router(true);
        self
    }

    /// Run `work` with the backend factory on a blocking thread, holding the
    /// cloud lock for the whole call. `work` opens the backend itself (so it
    /// can validate its arguments before connecting) and must do all its
    /// backend work inside, since the Steam backend is not `Send`. Failures
    /// are normal tool errors.
    async fn with_cloud<F>(&self, work: F) -> CallToolResult
    where
        F: FnOnce(&cloud_tools::BackendFactory) -> Result<Value, String> + Send + 'static,
    {
        let Some(factory) = self.backend.clone() else {
            return failure(&json!({ "error": "this build has no Steam support" }));
        };
        let guard = self.cloud_lock.clone().lock_owned().await;
        save_tools::run(move || {
            let _guard = guard;
            work(&factory)
        })
        .await
    }

    /// Use another HTTP client factory for `save_check_urls` (tests).
    #[cfg(test)]
    fn with_fetcher(mut self, fetcher: save_tools::FetcherFactory) -> Self {
        self.fetcher = fetcher;
        self
    }

    /// The hub, or the error every editor tool returns when the editor port
    /// could not be bound.
    fn hub(&self) -> Result<&Arc<Hub>, CliError> {
        self.hub.as_ref().map_err(|why| {
            CliError::from(format!(
                "the editor port {} is unavailable, so the editor tools do not work (the save tools still do): {why}",
                self.ep.editor.port()
            ))
        })
    }

    /// Start receiving replies; an error while the listener is down.
    fn replies(&self) -> Result<Replies<'_>, CliError> {
        let hub = self.hub()?;
        match hub.not_listening() {
            Some(error) => Err(error.into()),
            None => Ok(hub.replies()),
        }
    }

    async fn fetch(&self) -> Result<ttskit_editor::LoadedGame, CliError> {
        let ep = self.ep;
        let replies = self.replies()?;
        replies.expect_scripts_reply();
        ttskit_editor::fetch_game(&replies, ep.game, ep.timeout)
            .await
            .map_err(editor_err)
    }

    async fn status(&self) -> Value {
        let ep = self.ep;
        let fetched = self.fetch().await;
        let mut doc = json!({
            "editorAvailable": self.hub.is_ok(),
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
            Err(e) => doc["error"] = json!(e.message),
        }
        match &self.hub {
            Ok(hub) => {
                if let (Value::Object(doc), Value::Object(hub)) = (&mut doc, hub.status()) {
                    doc.extend(hub);
                }
            }
            Err(why) => doc["editorError"] = json!(why),
        }
        doc
    }

    async fn scripts(&self, guids: Option<Vec<String>>) -> Result<Value, CliError> {
        let game = self.fetch().await?;
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

    async fn console(&self, args: GetConsoleArgs) -> Result<Value, CliError> {
        let hub = self.hub()?;
        let last = hub.last_seq();
        if let Some(since) = args.since.filter(|s| *s > last) {
            return Err(format!(
                "since {since} is ahead of the newest event ({last}); the server may have restarted. Omit since, or pass 0 for everything buffered"
            )
            .into());
        }
        if args.kinds.as_ref().is_some_and(Vec::is_empty) {
            return Err("kinds is empty, so nothing could match; omit it for all kinds".into());
        }
        let limit = args.limit.unwrap_or(DEFAULT_CONSOLE_LIMIT);
        if !(1..=MAX_CONSOLE_LIMIT).contains(&limit) {
            return Err(format!("limit must be between 1 and {MAX_CONSOLE_LIMIT}").into());
        }
        let q = Query {
            since: args.since,
            kinds: args.kinds,
            limit,
        };
        let cap = Duration::from_millis(MAX_CONSOLE_MS);
        let wait = Duration::from_millis(args.wait_ms.unwrap_or(0)).min(cap);
        let quiet = Duration::from_millis(args.quiet_ms.unwrap_or(DEFAULT_QUIET_MS));
        let mut head = hub.watch_head();
        let mut page = hub.query(&q);
        if page.events.is_empty() && !wait.is_zero() {
            let start = tokio::time::Instant::now();
            let first_by = start + wait;
            while page.events.is_empty() {
                match tokio::time::timeout_at(first_by, head.changed()).await {
                    Ok(Ok(())) => page = hub.query(&q),
                    Ok(Err(_)) | Err(_) => break,
                }
            }
            // After the first match, collect until a quiet period brings no new one.
            let end_by = start + cap;
            while !page.events.is_empty() && page.events.len() < limit {
                let until = (tokio::time::Instant::now() + quiet).min(end_by);
                let seen = page.events.len();
                while let Ok(Ok(())) = tokio::time::timeout_at(until, head.changed()).await {
                    page = hub.query(&q);
                    if page.events.len() > seen {
                        break;
                    }
                }
                if page.events.len() == seen {
                    break;
                }
            }
        }
        let events: Vec<Value> = page.events.iter().map(|e| e.to_json()).collect();
        Ok(json!({
            "events": events,
            "nextSince": page.next_since,
            "more": page.more,
            "omitted": page.omitted,
            "dropped": page.dropped,
        }))
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
        // The project config in the root, if any; else the default lookup.
        let config = Some(self.root.join("ttskit.toml")).filter(|p| p.exists());
        let desired = desired_from_tree(&dir, includes, config.as_deref())?;
        // Replies for the whole call, so that the reload is noticed.
        let hub = self.hub()?;
        let replies = self.replies()?;
        replies.expect_scripts_reply();
        let live = ttskit_editor::fetch_scripts(&replies, ep.game, ep.timeout)
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
        let before = hub.last_seq();
        ttskit_editor::send(ep.game, &plan.message(), ep.timeout)
            .await
            .map_err(editor_err)?;
        // The game answers Save & Play with "saved" (ID 6), then "loaded"
        // (ID 1) when the reload is done, then the output of `onLoad`.
        let reloaded = wait_for_reload(&replies, ep.timeout).await?;
        let quiet = Duration::from_millis(args.quiet_ms.unwrap_or(DEFAULT_QUIET_MS));
        wait_quiet(&replies, ep.timeout, quiet, ep.timeout).await?;
        let page = hub.query(&Query {
            since: Some(before),
            kinds: Some(vec![EventKind::Print, EventKind::Error]),
            limit: usize::MAX,
        });
        let console: Vec<Value> = page.events.iter().map(|e| e.to_json()).collect();
        doc["sent"] = json!(true);
        doc["reloaded"] = json!(reloaded);
        if !reloaded {
            doc["note"] = json!(format!(
                "the game did not report a reload within {} s; console output may be incomplete (call get_console)",
                ep.timeout.as_secs_f64()
            ));
        }
        if page.dropped > 0 {
            doc["dropped"] = json!(page.dropped);
        }
        doc["console"] = json!(console);
        doc["lastSeq"] = json!(page.next_since);
        Ok(success(&doc))
    }
}

/// Wait up to `limit` for the game's "loaded" message (ID 1). False if none
/// came in time. The hub buffers the messages before it.
async fn wait_for_reload(source: &impl MessageSource, limit: Duration) -> Result<bool, CliError> {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        match tokio::time::timeout_at(deadline, source.next_message(limit)).await {
            Err(_) => return Ok(false),
            Ok(Ok(ttskit_editor::Incoming::LoadGame { .. })) => return Ok(true),
            Ok(Ok(_)) => {}
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

/// Wait until `quiet` passes without a message from the game, or `max` in
/// total. `read_limit` bounds the reading of one message.
async fn wait_quiet(
    source: &impl MessageSource,
    read_limit: Duration,
    quiet: Duration,
    max: Duration,
) -> Result<(), CliError> {
    let deadline = tokio::time::Instant::now() + max;
    loop {
        let until = (tokio::time::Instant::now() + quiet).min(deadline);
        match tokio::time::timeout_at(until, source.next_message(read_limit)).await {
            Err(_) => return Ok(()),
            Ok(Ok(_)) => {}
            // One bad message must not end the waiting.
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

/// Resolve a path to write: the file need not exist, but its directory must,
/// and it must be inside `root` (also through symlinks, for the directory and
/// for an existing file). The file name must be a plain name.
fn resolve_output_in_root(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let name = match path.components().next_back() {
        Some(Component::Normal(name)) => name.to_owned(),
        _ => {
            return Err(format!(
                "path {} must name a file inside the project root {}",
                path.display(),
                root.display()
            ));
        }
    };
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    // Checks `..`, existence of the directory and containment.
    let dir = resolve_in_root(root, parent)?;
    let target = dir.join(name);
    if target.exists() {
        // An existing file (or a symlink to one) must also be inside.
        return resolve_in_root(root, &target);
    }
    Ok(target)
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
        let replies = match self.replies() {
            Ok(r) => r,
            Err(e) => return failure_of(&e),
        };
        let last_seq = || self.hub().map_or(0, |h| h.last_seq());
        let outcome =
            ttskit_editor::execute_lua(&replies, ep.game, &guid, &args.code, ep.timeout, quiet)
                .await;
        drop(replies);
        match outcome {
            Ok(outcome) => {
                let mut doc = exec_json(&guid, &outcome);
                doc["lastSeq"] = json!(last_seq());
                if outcome.errors.is_empty() {
                    success(&doc)
                } else {
                    failure(&doc)
                }
            }
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
        if let Err(e) = self.hub() {
            return failure_of(&e);
        }
        match ttskit_editor::send_custom(ep.game, Value::Object(args.message), ep.timeout).await {
            Ok(()) => success(&json!({ "sent": true })),
            Err(e) => failure_of(&editor_err(e)),
        }
    }

    #[tool(
        description = "Read the events the server buffered since it started: the game's print output (type print), Lua errors (error, with the guid of the failing object), messages from sendExternalMessage (custom), and the game being saved (saved) or loaded (loaded, also after save_and_play). Every event has an increasing seq. Pass the result's nextSince as `since` next time to get only newer events; `dropped` counts events after `since` that the bounded buffer no longer holds. Returns at once unless wait_ms is given."
    )]
    async fn get_console(&self, Parameters(args): Parameters<GetConsoleArgs>) -> CallToolResult {
        // Reads the buffer only, so it does not wait for other tool calls.
        match self.console(args).await {
            Ok(doc) => success(&doc),
            Err(e) => failure_of(&e),
        }
    }

    #[tool(
        description = "List the asset URLs (images, models, audio, ...) of a save file, grouped by host class, with every place that uses them. Local, read-only, needs no game; the path must be inside the server's root. Same JSON as `ttskit cloud scan --json`."
    )]
    async fn save_scan(&self, Parameters(args): Parameters<SaveScanArgs>) -> CallToolResult {
        let root = self.root.clone();
        save_tools::run(move || save_tools::scan(&root, args)).await
    }

    #[tool(
        description = "Check a save file for common mistakes (duplicate GUIDs, positions out of bounds, ...) with the rules of `ttskit save lint`. Findings are a normal result (see the summary); only an unreadable save, a bad config or an unknown rule id is an error. Local, read-only, needs no game; paths must be inside the server's root. Without `config`, ttskit.toml in the root is used if it exists."
    )]
    async fn save_lint(&self, Parameters(args): Parameters<SaveLintArgs>) -> CallToolResult {
        let root = self.root.clone();
        save_tools::run(move || save_tools::lint(&root, args)).await
    }

    #[tool(
        description = "Compare two save files semantically and list the objects added, removed and changed (same JSON as `ttskit save diff --json`). Equal saves give an empty change list. Local, read-only, needs no game; paths must be inside the server's root."
    )]
    async fn save_diff(&self, Parameters(args): Parameters<SaveDiffArgs>) -> CallToolResult {
        let root = self.root.clone();
        save_tools::run(move || save_tools::diff(&root, args)).await
    }

    #[tool(
        description = "Request every asset URL of a save file over the network (HEAD, or ranged GET with verify_content) and report which ones are ok, redirected, dead, unreachable or skipped (same JSON as `ttskit save check-urls --json`). Dead or unreachable URLs are a normal result, not an error. Needs internet access and can take a while; the save path must be inside the server's root."
    )]
    async fn save_check_urls(
        &self,
        Parameters(args): Parameters<SaveCheckUrlsArgs>,
    ) -> CallToolResult {
        let root = self.root.clone();
        let fetcher = self.fetcher.clone();
        save_tools::run(move || save_tools::check_urls(&root, &fetcher, args)).await
    }

    #[tool(
        description = "Show the asset manifest (ttskit.manifest.json) that maps uploaded files to URLs, as `ttskit cloud manifest --json` does. A missing default manifest gives the empty one. Local, read-only; the path must be inside the server's root."
    )]
    async fn manifest_show(
        &self,
        Parameters(args): Parameters<ManifestShowArgs>,
    ) -> CallToolResult {
        let root = self.root.clone();
        save_tools::run(move || save_tools::manifest_show(&root, args)).await
    }

    #[tool(
        description = "Step 1 of rewriting asset URLs in a save: show what `ttskit save rewrite-urls` would replace (its dry-run JSON) and return a fingerprint. Changes nothing. Give `map` (old/new URL file), `from_manifest` and/or `normalize_steam_hosts`, and `out` or `in_place` for where the result goes; the result says whether the output exists and would be overwritten. Paths must be inside the server's root. Then call save_rewrite_urls_apply."
    )]
    async fn save_rewrite_urls_plan(
        &self,
        Parameters(args): Parameters<RewriteUrlsArgs>,
    ) -> CallToolResult {
        let root = self.root.clone();
        change_tools::run(move || change_tools::rewrite_urls_plan(&root, &args)).await
    }

    #[tool(
        description = "Step 2 of rewriting asset URLs: with the same arguments as save_rewrite_urls_plan plus its fingerprint and confirm: true, write the new save (to `out`, overwriting an existing file, or in place). Refused, with nothing written, if confirm is not true, the fingerprint is missing or wrong, the inputs changed since the plan (the result then holds the fresh plan), or there is no `out` or `in_place`."
    )]
    async fn save_rewrite_urls_apply(
        &self,
        Parameters(args): Parameters<RewriteUrlsApplyArgs>,
    ) -> CallToolResult {
        let root = self.root.clone();
        change_tools::run(move || change_tools::rewrite_urls_apply(&root, &args)).await
    }

    #[tool(
        description = "Step 1 of building images from the project config's [[image.pipeline]] rules: show what `ttskit image build` would build (its dry-run JSON) and return a fingerprint. Changes nothing. Default config: ttskit.toml in the root. Then call image_build_apply. Only in builds with the `image` feature."
    )]
    async fn image_build_plan(
        &self,
        Parameters(args): Parameters<ImageBuildArgs>,
    ) -> CallToolResult {
        let root = self.root.clone();
        change_tools::run(move || change_tools::image_build_plan(&root, &args)).await
    }

    #[tool(
        description = "Step 2 of building images: with the same arguments as image_build_plan plus its fingerprint and confirm: true, build the outputs and update the cache file. Never deletes outputs. Refused, with nothing built, if confirm is not true, the fingerprint is missing or wrong, or the plan changed since (the result then holds the fresh plan). If some images fail the result is an error with ok: false; the images that were built stay. Only in builds with the `image` feature."
    )]
    async fn image_build_apply(
        &self,
        Parameters(args): Parameters<ImageBuildApplyArgs>,
    ) -> CallToolResult {
        let root = self.root.clone();
        change_tools::run(move || change_tools::image_build_apply(&root, &args)).await
    }

    #[tool(
        description = "Report images that are larger than the game can use well (same JSON as `ttskit image check --json`): for each image its format, size and findings such as side-too-long, plus an `errors` list of files that could not be read. Findings are a normal result. Local, read-only, needs no game; inputs (files or directories) must be inside the server's root. Only in builds with the `image` feature."
    )]
    async fn image_check(&self, Parameters(args): Parameters<ImageCheckArgs>) -> CallToolResult {
        let root = self.root.clone();
        save_tools::run(move || image_tools::check(&root, args)).await
    }

    #[tool(
        description = "List the files in the Steam Cloud of the running Steam account, grouped by folder, with sizes and the quota (same JSON as `ttskit cloud list --json`). Read-only. Opens Steam for this call: Steam must be running and logged in, otherwise the call fails with the reason. Optional `folder` limits the listing."
    )]
    async fn cloud_list(&self, Parameters(args): Parameters<CloudListArgs>) -> CallToolResult {
        self.with_cloud(move |factory| cloud_tools::list(factory, args))
            .await
    }

    #[tool(
        description = "Compare a local directory with a Steam Cloud folder (same JSON as `ttskit cloud status --json`): which files are in sync, changed, only local or only in the cloud. Not being in sync is a normal result, see the JSON. Read-only. The directory and config must be inside the server's root; ignore patterns come from the project config (default ttskit.toml in the root). Opens Steam for this call; it fails with the reason if Steam is unavailable."
    )]
    async fn cloud_status(&self, Parameters(args): Parameters<CloudStatusArgs>) -> CallToolResult {
        let root = self.root.clone();
        self.with_cloud(move |factory| cloud_tools::status(&root, factory, args))
            .await
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

/// `ttskit mcp`: hold the editor port (the editor tools are unavailable if it
/// cannot be bound) and serve on stdin and stdout until stdin closes. `max_events` bounds the event buffer.
pub fn run(root: Option<&Path>, ep: Endpoints, max_events: usize) -> Result<(), CliError> {
    let root = resolve_root(root)?;
    runtime()?.block_on(async {
        let server = EditorServer::start(ep, root, max_events).await;
        if let Err(why) = &server.hub {
            eprintln!("ttskit mcp: the editor tools are unavailable: {why}");
        }
        serve_on(server, rmcp::transport::stdio()).await
    })
}

#[cfg(test)]
mod tests;

mod buffer;
mod change_tools;
mod cloud_tools;
mod hub;
mod image_tools;
mod plan_apply;
mod save_tools;
