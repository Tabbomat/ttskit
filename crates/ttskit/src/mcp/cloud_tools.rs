//! The Steam Cloud tools of the MCP server. Steam is opened per tool call,
//! never at server start; a failure to open it is a normal tool error. The
//! backend comes from a [`BackendFactory`], so tests use an in-memory cloud
//! and builds without Steam support do not list these tools at all.
//!
//! Every use of a backend goes through [`super::EditorServer::with_cloud`]:
//! one lock across the whole call (Steam must not be initialised twice at the
//! same time) and `spawn_blocking`. The factory is called on the blocking
//! thread that then uses the backend, since the Steam backend is not `Send`.

use std::path::Path;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use serde::Deserialize;
use serde_json::Value;
use ttskit_cloud::CloudBackend;

use super::resolve_in_root;
use super::save_tools::{existing_default, json_of};
use crate::cloud;

/// Opens the cloud backend (for Steam: connects to the running client).
pub(super) type BackendFactory =
    Arc<dyn Fn() -> Result<Box<dyn CloudBackend>, String> + Send + Sync>;

/// The names of the tools that need a backend.
const CLOUD_TOOLS: [&str; 8] = [
    "cloud_list",
    "cloud_status",
    "cloud_upload_plan",
    "cloud_upload_apply",
    "cloud_sync_push_plan",
    "cloud_sync_push_apply",
    "cloud_sync_pull_plan",
    "cloud_sync_pull_apply",
];

/// The factory of this build: Steam if compiled in, else none.
pub(super) fn production_backend() -> Option<BackendFactory> {
    cfg!(feature = "steam").then(|| Arc::new(cloud::connect) as BackendFactory)
}

/// Drops the cloud tools from `router` unless a backend factory exists, so
/// that `tools/list` omits them.
pub(super) fn with_cloud_tools<S>(mut router: ToolRouter<S>, available: bool) -> ToolRouter<S>
where
    S: Send + Sync + 'static,
{
    if !available {
        for name in CLOUD_TOOLS {
            router.remove_route(name);
        }
    }
    router
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct CloudListArgs {
    /// Only this cloud folder and its subfolders (separator `/`). Default: everything.
    folder: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct CloudStatusArgs {
    /// The local directory to compare, relative to the server's root or absolute (must be inside the root).
    dir: String,
    /// Cloud folder to compare with (separator `/`). Default: the cloud root.
    folder: Option<String>,
    /// The project config; its [local] ignore patterns apply, relative to `dir` (must be inside the root). Default: ttskit.toml in the root if it exists.
    config: Option<String>,
}

pub(super) fn list(factory: &BackendFactory, args: CloudListArgs) -> Result<Value, String> {
    let backend = factory()?;
    json_of(|out| {
        cloud::list(backend.as_ref(), args.folder.as_deref(), true, out).map_err(|e| e.to_string())
    })
}

pub(super) fn status(
    root: &Path,
    factory: &BackendFactory,
    args: CloudStatusArgs,
) -> Result<Value, String> {
    let dir = resolve_in_root(root, Path::new(&args.dir))?;
    let config = match &args.config {
        Some(path) => Some(resolve_in_root(root, Path::new(path))?),
        None => existing_default(root, ttskit_cloud::ProjectConfig::FILE_NAME)?,
    };
    let ignore = match config {
        Some(config) => ttskit_cloud::ProjectConfig::load(&config)
            .map_err(|e| e.to_string())?
            .ignore_set(),
        None => ttskit_cloud::IgnoreSet::default(),
    };
    // Scan the local side before connecting to Steam.
    let scan = ttskit_cloud::scan_local_ignoring(&dir, &ignore).map_err(|e| e.to_string())?;
    let backend = factory()?;
    json_of(|out| {
        // Out of sync is a normal result; the JSON says it.
        cloud::status(
            backend.as_ref(),
            &scan.files,
            args.folder.as_deref().unwrap_or_default(),
            &ignore,
            scan.ignored,
            &args.dir,
            true,
            out,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
}
