//! The save tools of the MCP server: read-only, local files inside the
//! project root (`save_check_urls` also uses the network). They run the same
//! code as the CLI commands with `--json`, so each feature has one JSON schema.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::CallToolResult;
use serde::Deserialize;
use serde_json::{Value, json};
use ttskit_core::{CheckConfig, Fetch};

use super::{failure, resolve_in_root, success};
use crate::cloud::ManifestFormat;
use crate::{check_urls, cloud, diff, lint};

/// Makes the HTTP client of `save_check_urls`; the argument is the timeout of
/// one request. Tests replace it with a fake.
pub(super) type FetcherFactory =
    Arc<dyn Fn(Duration) -> Result<Box<dyn Fetch + Send>, String> + Send + Sync>;

/// The real HTTP client.
pub(super) fn http_fetcher() -> FetcherFactory {
    Arc::new(|timeout| {
        let client = crate::http::HttpClient::new(timeout)?;
        Ok(Box::new(client) as Box<dyn Fetch + Send>)
    })
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct SaveScanArgs {
    /// The save file (JSON), relative to the server's root or absolute (must be inside the root).
    save: String,
    /// Accepted for symmetry with the CLI; the JSON result always lists every occurrence.
    occurrences: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct SaveLintArgs {
    /// The save file (JSON), relative to the server's root or absolute (must be inside the root).
    save: String,
    /// Project config with a [lint] table (must be inside the root). Default: ttskit.toml in the root if it exists.
    config: Option<String>,
    /// Rule ids to switch off.
    allow: Option<Vec<String>>,
    /// Rule ids to set to warning.
    warn: Option<Vec<String>>,
    /// Rule ids to set to error.
    deny: Option<Vec<String>>,
    /// Only findings at or above this severity: "info", "warning" (default) or "error".
    level: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct SaveDiffArgs {
    /// The old save file (JSON), relative to the server's root or absolute (must be inside the root).
    old: String,
    /// The new save file (JSON), same rules as `old`.
    new: String,
    /// Treat transform numbers closer than this as equal (a finite number >= 0; default: exact text).
    tolerance: Option<f64>,
    /// Include the full line diff of changed scripts and XML UI.
    scripts: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct SaveCheckUrlsArgs {
    /// The save file (JSON), relative to the server's root or absolute (must be inside the root).
    save: String,
    /// Also download the start of each file and report HTML pages as dead.
    verify_content: Option<bool>,
    /// Only check URLs on these hosts or their subdomains.
    host: Option<Vec<String>>,
    /// Skip URLs on these hosts or their subdomains.
    skip_host: Option<Vec<String>>,
    /// Most requests in flight at once (1 to 256, default 16).
    concurrency: Option<u32>,
    /// Most requests in flight at once to one host (1 to 64, default 4).
    per_host: Option<u32>,
    /// Timeout of one request in seconds (1 to 600, default 10).
    timeout: Option<u32>,
    /// Extra attempts after a timeout, network error, 429 or 5xx (0 to 10, default 3).
    retries: Option<u32>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct ManifestShowArgs {
    /// The manifest file, relative to the server's root or absolute (must be inside the root). Default: ttskit.manifest.json in the root.
    manifest: Option<String>,
}

/// Run blocking work off the async threads, so that the editor hub stays
/// responsive, and turn its outcome into a tool result.
pub(super) async fn run<F>(work: F) -> CallToolResult
where
    F: FnOnce() -> Result<Value, String> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(doc)) => success(&doc),
        Ok(Err(error)) => failure(&json!({ "error": error })),
        Err(e) => failure(&json!({ "error": format!("the tool failed: {e}") })),
    }
}

/// Run a CLI command that writes JSON to `out` and parse what it wrote.
fn json_of(command: impl FnOnce(&mut Vec<u8>) -> Result<(), String>) -> Result<Value, String> {
    let mut out = Vec::new();
    command(&mut out)?;
    serde_json::from_slice(&out).map_err(|e| format!("the command wrote invalid JSON: {e}"))
}

/// `name` inside the root (checked) if that file exists there, else `None`.
fn existing_default(root: &Path, name: &str) -> Result<Option<PathBuf>, String> {
    if root.join(name).exists() {
        resolve_in_root(root, Path::new(name)).map(Some)
    } else {
        Ok(None)
    }
}

pub(super) fn scan(root: &Path, args: SaveScanArgs) -> Result<Value, String> {
    let save = resolve_in_root(root, Path::new(&args.save))?;
    json_of(|out| {
        cloud::scan(&save, true, args.occurrences.unwrap_or(false), out).map_err(|e| e.to_string())
    })
}

pub(super) fn lint(root: &Path, args: SaveLintArgs) -> Result<Value, String> {
    let save = resolve_in_root(root, Path::new(&args.save))?;
    let config = match &args.config {
        Some(path) => Some(resolve_in_root(root, Path::new(path))?),
        None => existing_default(root, "ttskit.toml")?,
    };
    let level = lint::parse_level(args.level.as_deref().unwrap_or("warning"))?;
    let none = Vec::new();
    let options = lint::Options {
        save: &save,
        config: config.as_deref(),
        allow: args.allow.as_ref().unwrap_or(&none),
        warn: args.warn.as_ref().unwrap_or(&none),
        deny: args.deny.as_ref().unwrap_or(&none),
        level,
        json: true,
    };
    json_of(|out| lint::lint(&options, out).map(|_| ()))
}

pub(super) fn diff(root: &Path, args: SaveDiffArgs) -> Result<Value, String> {
    let old = resolve_in_root(root, Path::new(&args.old))?;
    let new = resolve_in_root(root, Path::new(&args.new))?;
    let tolerance = args.tolerance.unwrap_or(0.0);
    diff::parse_tolerance(&tolerance.to_string())?;
    let options = diff::Options {
        old: &old,
        new: &new,
        tolerance,
        scripts: args.scripts.unwrap_or(false),
        verbose: false,
        json: true,
    };
    json_of(|out| diff::diff(&options, out).map(|_| ()))
}

fn in_range(
    name: &str,
    value: Option<u32>,
    default: u32,
    range: (u32, u32),
) -> Result<u32, String> {
    let value = value.unwrap_or(default);
    if (range.0..=range.1).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "{name} must be between {} and {}",
            range.0, range.1
        ))
    }
}

pub(super) fn check_urls(
    root: &Path,
    fetcher: &FetcherFactory,
    args: SaveCheckUrlsArgs,
) -> Result<Value, String> {
    let save = resolve_in_root(root, Path::new(&args.save))?;
    // The same limits and defaults as `save check-urls`.
    let concurrency = in_range("concurrency", args.concurrency, 16, (1, 256))?;
    let per_host = in_range("per_host", args.per_host, 4, (1, 64))?;
    let timeout = in_range("timeout", args.timeout, 10, (1, 600))?;
    let retries = in_range("retries", args.retries, 3, (0, 10))?;
    let config = CheckConfig {
        concurrency: concurrency as usize,
        per_host: per_host as usize,
        retries,
        verify_content: args.verify_content.unwrap_or(false),
        only_hosts: args.host.unwrap_or_default(),
        skip_hosts: args.skip_host.unwrap_or_default(),
        ..CheckConfig::default()
    };
    let client = fetcher(Duration::from_secs(u64::from(timeout)))?;
    json_of(|out| check_urls::check_urls(&save, &*client, &config, true, out).map(|_| ()))
}

pub(super) fn manifest_show(root: &Path, args: ManifestShowArgs) -> Result<Value, String> {
    let path = match &args.manifest {
        Some(path) => resolve_in_root(root, Path::new(path))?,
        None => match existing_default(root, "ttskit.manifest.json")? {
            Some(path) => path,
            // Missing: the CLI shows the empty manifest.
            None => root.join("ttskit.manifest.json"),
        },
    };
    json_of(|out| cloud::show_manifest(&path, ManifestFormat::Json, out).map_err(|e| e.to_string()))
}
