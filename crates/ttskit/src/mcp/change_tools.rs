//! The plan/apply pairs for the local commands that change files:
//! `save_rewrite_urls_*` and `image_build_*` (see [`super::plan_apply`]).
//! A plan is the CLI's dry-run JSON; an apply runs the CLI function for real
//! and returns its JSON.

use std::path::{Path, PathBuf};

use rmcp::model::CallToolResult;
use serde::Deserialize;
use serde_json::{Value, json};

use super::plan_apply::{self, Pair, Plan, error, sha256_hex};
use super::save_tools::json_of;
use super::{failure, resolve_in_root, resolve_output_in_root, success};
use crate::{image, rewrite_urls};

pub(super) const REWRITE: Pair = Pair {
    plan: "save_rewrite_urls_plan",
    apply: "save_rewrite_urls_apply",
};

pub(super) const IMAGE_BUILD: Pair = Pair {
    plan: "image_build_plan",
    apply: "image_build_apply",
};

/// Run blocking work and turn its outcome into a tool result; an `Err` body
/// is a tool error.
pub(super) async fn run<F>(work: F) -> CallToolResult
where
    F: FnOnce() -> Result<Value, Value> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(doc)) => success(&doc),
        Ok(Err(doc)) => failure(&doc),
        Err(e) => failure(&error(format!("the tool failed: {e}"))),
    }
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct RewriteUrlsArgs {
    /// The save file (JSON), relative to the server's root or absolute (must be inside the root).
    save: String,
    /// Mapping file with one `old URL<TAB>new URL` per line (inside the root).
    map: Option<String>,
    /// Project manifest (ttskit.manifest.json) whose URL for the same SHA1 replaces a Steam Cloud URL (inside the root).
    from_manifest: Option<String>,
    /// Also rewrite the legacy Steam Cloud host (cloud-3.steamusercontent.com) to the current one.
    normalize_steam_hosts: Option<bool>,
    /// Also replace URLs inside LuaScript, LuaScriptState and XmlUI strings.
    in_scripts: Option<bool>,
    /// Write the new save here (inside the root; its directory must exist). An existing file is overwritten when the plan is applied. Apply needs `out` or `in_place`.
    out: Option<String>,
    /// Replace the input save itself when the plan is applied. Use instead of `out`.
    in_place: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct RewriteUrlsApplyArgs {
    #[serde(flatten)]
    plan: RewriteUrlsArgs,
    /// The fingerprint returned by save_rewrite_urls_plan for these same arguments.
    fingerprint: Option<String>,
    /// Must be true: writing the save (and overwriting an existing output) is only done with it.
    confirm: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct ImageBuildArgs {
    /// The project config with `[[image.pipeline]]` rules, relative to the server's root or absolute (must be inside the root). Default: ttskit.toml in the root.
    config: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct ImageBuildApplyArgs {
    #[serde(flatten)]
    plan: ImageBuildArgs,
    /// The fingerprint returned by image_build_plan for these same arguments.
    fingerprint: Option<String>,
    /// Must be true: building writes the output images and the cache file.
    confirm: Option<bool>,
}

// ---- save_rewrite_urls ----

struct RewritePaths {
    save: PathBuf,
    map: Option<PathBuf>,
    manifest: Option<PathBuf>,
    /// Where the apply writes; `None` if the arguments name no output.
    out: Option<PathBuf>,
}

fn rewrite_paths(root: &Path, args: &RewriteUrlsArgs) -> Result<RewritePaths, String> {
    let existing = |p: &Option<String>| {
        p.as_ref()
            .map(|p| resolve_in_root(root, Path::new(p)))
            .transpose()
    };
    let save = resolve_in_root(root, Path::new(&args.save))?;
    let in_place = args.in_place.unwrap_or(false);
    if in_place && args.out.is_some() {
        return Err("use either out or in_place, not both".to_owned());
    }
    let out = match &args.out {
        Some(out) => Some(resolve_output_in_root(root, Path::new(out))?),
        None => in_place.then(|| save.clone()),
    };
    Ok(RewritePaths {
        map: existing(&args.map)?,
        manifest: existing(&args.from_manifest)?,
        save,
        out,
    })
}

fn file_hash(path: Option<&PathBuf>) -> Value {
    match path.map(std::fs::read) {
        None => Value::Null,
        // An unreadable file is reported by the command itself.
        Some(Err(_)) => json!("unreadable"),
        Some(Ok(bytes)) => json!(sha256_hex(&bytes)),
    }
}

/// The CLI's dry run, with the output and what the plan depends on.
fn rewrite_plan(paths: &RewritePaths, args: &RewriteUrlsArgs) -> Result<Plan, String> {
    let in_place = args.in_place.unwrap_or(false);
    let options = rewrite_urls::Options {
        save: &paths.save,
        map: paths.map.as_deref(),
        manifest: paths.manifest.as_deref(),
        normalize_steam_hosts: args.normalize_steam_hosts.unwrap_or(false),
        in_scripts: args.in_scripts.unwrap_or(false),
        out: if in_place { None } else { paths.out.as_deref() },
        in_place,
        yes: false,
        dry_run: true,
        json: true,
    };
    let mut doc = json_of(|out| rewrite_urls::rewrite_urls(&options, out))?;
    let exists = paths.out.as_ref().is_some_and(|p| p.exists());
    doc["output_exists"] = json!(exists);
    doc["would_overwrite"] = json!(exists);
    doc["will_write"] = json!(paths.out.is_some());
    // The plan text does not show the file contents; the hash does.
    let hashed = json!({
        "plan": doc,
        "inputs": {
            "save": file_hash(Some(&paths.save)),
            "map": file_hash(paths.map.as_ref()),
            "manifest": file_hash(paths.manifest.as_ref()),
        },
    });
    Ok(Plan { body: doc, hashed })
}

pub(super) fn rewrite_urls_plan(root: &Path, args: &RewriteUrlsArgs) -> Result<Value, Value> {
    let paths = rewrite_paths(root, args).map_err(error)?;
    let plan = rewrite_plan(&paths, args).map_err(error)?;
    Ok(plan.result(REWRITE))
}

pub(super) fn rewrite_urls_apply(root: &Path, args: &RewriteUrlsApplyArgs) -> Result<Value, Value> {
    let given =
        plan_apply::require_confirmation(args.fingerprint.as_deref(), args.confirm, REWRITE)?;
    let plan_args = &args.plan;
    let paths = rewrite_paths(root, plan_args).map_err(error)?;
    let Some(destination) = &paths.out else {
        return Err(error(
            "nothing to write: give out (a new file) or in_place: true; nothing was done",
        ));
    };
    let plan = rewrite_plan(&paths, plan_args).map_err(error)?;
    plan_apply::verify(&given, &plan, REWRITE)?;
    let in_place = plan_args.in_place.unwrap_or(false);
    let options = rewrite_urls::Options {
        save: &paths.save,
        map: paths.map.as_deref(),
        manifest: paths.manifest.as_deref(),
        normalize_steam_hosts: plan_args.normalize_steam_hosts.unwrap_or(false),
        in_scripts: plan_args.in_scripts.unwrap_or(false),
        out: if in_place { None } else { Some(destination) },
        in_place,
        // Confirmed by `confirm: true` and the fingerprint.
        yes: true,
        dry_run: false,
        json: true,
    };
    let result = json_of(|out| rewrite_urls::rewrite_urls(&options, out)).map_err(error)?;
    Ok(plan_apply::applied(result))
}

// ---- image_build ----

fn image_config(root: &Path, args: &ImageBuildArgs) -> Result<PathBuf, String> {
    let path = args.config.as_deref().unwrap_or("ttskit.toml");
    resolve_in_root(root, Path::new(path))
}

fn image_plan(config: &Path) -> Result<Plan, String> {
    let options = image::BuildArgs {
        config: config.to_owned(),
        dry_run: true,
        prune: false,
        yes: false,
        json: true,
    };
    let doc = json_of(|out| image::build(&options, out).map(|_| ()))?;
    Ok(Plan {
        hashed: doc.clone(),
        body: doc,
    })
}

pub(super) fn image_build_plan(root: &Path, args: &ImageBuildArgs) -> Result<Value, Value> {
    let config = image_config(root, args).map_err(error)?;
    Ok(image_plan(&config).map_err(error)?.result(IMAGE_BUILD))
}

pub(super) fn image_build_apply(root: &Path, args: &ImageBuildApplyArgs) -> Result<Value, Value> {
    let given =
        plan_apply::require_confirmation(args.fingerprint.as_deref(), args.confirm, IMAGE_BUILD)?;
    let config = image_config(root, &args.plan).map_err(error)?;
    let plan = image_plan(&config).map_err(error)?;
    plan_apply::verify(&given, &plan, IMAGE_BUILD)?;
    // Never prunes: deleting outputs is not offered.
    let options = image::BuildArgs {
        config,
        dry_run: false,
        prune: false,
        yes: true,
        json: true,
    };
    let mut ok = true;
    let mut result = json_of(|out| {
        ok = image::build(&options, out)?;
        Ok(())
    })
    .map_err(error)?;
    result["ok"] = json!(ok);
    let result = plan_apply::applied(result);
    // Some images failed: the files that were built stay, and the JSON says
    // which jobs failed, but the call is reported as an error.
    if ok { Ok(result) } else { Err(result) }
}
