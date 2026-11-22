//! The plan/apply pairs for the cloud commands that change something:
//! `cloud_upload_*`, `cloud_sync_push_*` and `cloud_sync_pull_*` (see
//! [`super::plan_apply`]). A plan is the CLI function's dry-run JSON; an
//! apply opens the backend once, recomputes the plan on it, verifies the
//! fingerprint and then runs the same function for real. There is no prune:
//! nothing is deleted in the cloud and no local file is recycled.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use serde_json::{Value, json};
use ttskit_cloud::{BackupStore, CloudBackend, ProjectConfig};

use super::cloud_tools::BackendFactory;
use super::plan_apply::{self, Pair, Plan, error, sha256_hex};
use super::resolve_in_root;
use crate::cloud::{self, CommandError, RunMode};

pub(super) const UPLOAD: Pair = Pair {
    plan: "cloud_upload_plan",
    apply: "cloud_upload_apply",
};
pub(super) const DELETE: Pair = Pair {
    plan: "cloud_delete_plan",
    apply: "cloud_delete_apply",
};
pub(super) const SYNC_PUSH: Pair = Pair {
    plan: "cloud_sync_push_plan",
    apply: "cloud_sync_push_apply",
};
pub(super) const SYNC_PULL: Pair = Pair {
    plan: "cloud_sync_pull_plan",
    apply: "cloud_sync_pull_apply",
};

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct CloudUploadArgs {
    /// The local file to upload, relative to the server's root or absolute (must be inside the root).
    file: String,
    /// Target cloud folder (separator `/`). Default: the cloud root.
    folder: Option<String>,
    /// Name in the cloud. Default: the file name.
    name: Option<String>,
    /// The project config; its directory gets the manifest (must be inside the root). Default: ttskit.toml in the root if it exists.
    config: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct CloudUploadApplyArgs {
    #[serde(flatten)]
    plan: CloudUploadArgs,
    /// The fingerprint returned by cloud_upload_plan for these same arguments.
    fingerprint: Option<String>,
    /// Must be true: uploading (and replacing a cloud file of the same name) is only done with it.
    confirm: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct CloudSyncArgs {
    /// The project config with the [cloud] folder and [local] directory (must be inside the root). Default: ttskit.toml in the root.
    config: Option<String>,
    /// Also act on the differences the plain sync leaves alone, as `--prune` does. Push: replace changed cloud files and DELETE cloud files that are not local (files matching [local] ignore are left alone). Pull: move local files that are not in the cloud folder to the recycle bin. Default false. Give the same value to the plan and the apply tool: a plan made without it cannot be applied with it.
    prune: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct CloudSyncApplyArgs {
    #[serde(flatten)]
    plan: CloudSyncArgs,
    /// The fingerprint returned by the matching plan tool for these same arguments.
    fingerprint: Option<String>,
    /// Must be true: the sync is only done with it.
    confirm: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct CloudDeleteArgs {
    /// The file to delete: folder path and name (`Decks/back.png`), display name, or cloud name. Give either `name` or `folder`.
    name: Option<String>,
    /// Delete this cloud folder with all its files and subfolders (separator `/`). Give either `name` or `folder`.
    folder: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub(super) struct CloudDeleteApplyArgs {
    #[serde(flatten)]
    plan: CloudDeleteArgs,
    /// The fingerprint returned by cloud_delete_plan for these same arguments.
    fingerprint: Option<String>,
    /// Must be true: the deletion is only done with it.
    confirm: Option<bool>,
}

// ---- shared pieces ----

/// The index backup store: the server's setting, else `TTSKIT_BACKUP_DIR` (as
/// for the CLI), else the CLI's default directory. Assistants cannot choose it.
pub(super) fn backup_store(setting: &Option<PathBuf>) -> Result<BackupStore, String> {
    let dir = setting.clone().or_else(|| {
        std::env::var_os("TTSKIT_BACKUP_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    cloud::backup_dir(dir)
        .map(BackupStore::new)
        .map_err(|e| e.to_string())
}

/// Makes the [`ttskit_cloud::LocalDir`] a pull writes to; the argument is the
/// directory. The real one moves pruned files to the recycle bin; tests
/// replace it.
pub(super) type LocalDirFactory =
    std::sync::Arc<dyn Fn(PathBuf) -> Box<dyn ttskit_cloud::LocalDir> + Send + Sync>;

/// The real local directory.
pub(super) fn fs_dir() -> LocalDirFactory {
    std::sync::Arc::new(|dir| Box::new(ttskit_cloud::FsDir::new(dir)))
}

/// The warning of a plan with prune.
fn prune_warning(prune: bool, what: &str) -> Option<String> {
    prune.then(|| format!("This plan has prune: applying it {what}."))
}

/// What a CLI function printed and how it ended.
type Ran = (Option<Value>, Option<CommandError>);

fn run_cli(f: impl FnOnce(&mut Vec<u8>) -> Result<(), CommandError>) -> Ran {
    let mut out = Vec::new();
    let result = f(&mut out);
    (serde_json::from_slice(&out).ok(), result.err())
}

fn never_ask(_prompt: &str) -> Result<bool, CommandError> {
    Err(std::io::Error::other("a confirmation was asked for although it was given").into())
}

fn is_conflicts(e: &CommandError) -> bool {
    matches!(
        e,
        CommandError::PushConflicts(_) | CommandError::PullConflicts(_)
    )
}

/// The plan of a dry run. A sync plan with conflicts is a normal plan; the
/// CLI refuses to run it, and so does apply.
fn make_plan(ran: Ran, hash: impl FnOnce(&Value) -> Value) -> Result<Plan, Value> {
    let (doc, err) = ran;
    let Some(mut doc) = doc else {
        return Err(error(err.map_or_else(
            || "the command printed no plan".to_owned(),
            |e| e.to_string(),
        )));
    };
    match err {
        Some(e) if is_conflicts(&e) => doc["blocked"] = json!(e.to_string()),
        Some(e) => return Err(error(e.to_string())),
        None => {}
    }
    let hashed = hash(&doc);
    Ok(Plan { body: doc, hashed })
}

/// The outcome of the real run: the CLI JSON plus `applied` (whether the
/// change was done). Anything that went wrong is an error result that still
/// carries the JSON.
fn finish(ran: Ran) -> Result<Value, Value> {
    match ran {
        (None, err) => Err(error(err.map_or_else(
            || "the command printed no result".to_owned(),
            |e| e.to_string(),
        ))),
        (Some(mut doc), err) => {
            doc["applied"] = json!(doc["done"] == json!(true));
            match err {
                None => Ok(doc),
                Some(e) => {
                    doc["error"] = json!(e.to_string());
                    Err(doc)
                }
            }
        }
    }
}

const PUSH_PRUNE: &str = "replaces changed cloud files and DELETES cloud files that are not local (the cloud index is backed up first)";
const PULL_PRUNE: &str = "moves local files that are not in the cloud folder to the recycle bin";

/// Adds the warning to the plan body (not to what is hashed).
fn set_warning(plan: &mut Plan, warning: Option<String>) {
    if let Some(w) = warning {
        plan.body["warning"] = json!(w);
    }
}

/// `explicit`, else `<root>/ttskit.toml` if it exists.
fn optional_config(root: &Path, explicit: Option<&str>) -> Result<Option<PathBuf>, String> {
    match explicit {
        Some(path) => resolve_in_root(root, Path::new(path)).map(Some),
        None => super::save_tools::existing_default(root, ProjectConfig::FILE_NAME),
    }
}

/// The directory holding the project config.
fn config_dir(config: &Path) -> PathBuf {
    match config.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_owned(),
        _ => PathBuf::from("."),
    }
}

/// The local directory of a project config, which must be inside the root.
fn local_dir(root: &Path, project: &ProjectConfig, config_dir: &Path) -> Result<PathBuf, String> {
    let dir = project.resolve_local_dir(config_dir);
    // The config parser refuses `..` and absolute paths; a symlink could
    // still lead out.
    if dir.exists() {
        resolve_in_root(root, &dir)
    } else {
        Ok(dir)
    }
}

// ---- cloud upload ----

struct Upload {
    name: String,
    data: Vec<u8>,
    folder: String,
    /// The manifest file and the local path to record in it.
    manifest: Option<(PathBuf, Option<String>)>,
}

fn upload_prepare(root: &Path, args: &CloudUploadArgs) -> Result<Upload, String> {
    let file = resolve_in_root(root, Path::new(&args.file))?;
    let (name, data) =
        cloud::read_upload_file(&file, args.name.as_deref()).map_err(|e| e.to_string())?;
    let manifest = match optional_config(root, args.config.as_deref())? {
        Some(config) => {
            let project = ProjectConfig::load(&config).map_err(|e| e.to_string())?;
            let dir_of_config = config_dir(&config);
            let dir = local_dir(root, &project, &dir_of_config)?;
            Some((
                dir_of_config.join(ttskit_cloud::Manifest::FILE_NAME),
                cloud::local_path_in(&dir, &file),
            ))
        }
        None => None,
    };
    Ok(Upload {
        name,
        data,
        folder: args.folder.clone().unwrap_or_default(),
        manifest,
    })
}

fn upload_run(
    upload: &Upload,
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    dry_run: bool,
) -> Ran {
    run_cli(|out| {
        let req = cloud::UploadRequest {
            name: &upload.name,
            data: &upload.data,
            folder: &upload.folder,
            mode: RunMode {
                dry_run,
                yes: !dry_run,
                json: true,
            },
            manifest: upload
                .manifest
                .as_ref()
                .map(|(file, local_path)| cloud::ManifestTarget {
                    file: file.clone(),
                    local_path: local_path.clone(),
                }),
        };
        cloud::upload_file(
            backend,
            store,
            &req,
            SystemTime::now(),
            &cloud::format_date(&jiff::Zoned::now()),
            &mut never_ask,
            out,
        )
    })
}

/// What an upload plan depends on: the plan fields (not the outcome fields)
/// and the content of the file.
fn upload_hashed(upload: &Upload, doc: &Value) -> Value {
    let keep = [
        "name",
        "cloud_name",
        "folder",
        "size",
        "sha1",
        "new_folders",
        "write",
        "share",
        "add_entry",
        "warnings",
    ];
    let plan: serde_json::Map<String, Value> = keep
        .iter()
        .map(|k| ((*k).to_owned(), doc[*k].clone()))
        .collect();
    json!({
        "plan": plan,
        "file_sha256": sha256_hex(&upload.data),
        "manifest": upload.manifest.as_ref().map(|(f, p)| json!([f.display().to_string(), p])),
    })
}

pub(super) fn upload_plan(
    root: &Path,
    factory: &BackendFactory,
    backup_dir: &Option<PathBuf>,
    args: &CloudUploadArgs,
) -> Result<Value, Value> {
    let upload = upload_prepare(root, args).map_err(error)?;
    let store = backup_store(backup_dir).map_err(error)?;
    let mut backend = factory().map_err(error)?;
    let ran = upload_run(&upload, backend.as_mut(), &store, true);
    let plan = make_plan(ran, |doc| upload_hashed(&upload, doc))?;
    Ok(plan.result(UPLOAD))
}

pub(super) fn upload_apply(
    root: &Path,
    factory: &BackendFactory,
    backup_dir: &Option<PathBuf>,
    args: &CloudUploadApplyArgs,
) -> Result<Value, Value> {
    let given =
        plan_apply::require_confirmation(args.fingerprint.as_deref(), args.confirm, UPLOAD)?;
    let upload = upload_prepare(root, &args.plan).map_err(error)?;
    let store = backup_store(backup_dir).map_err(error)?;
    let mut backend = factory().map_err(error)?;
    let ran = upload_run(&upload, backend.as_mut(), &store, true);
    let plan = make_plan(ran, |doc| upload_hashed(&upload, doc))?;
    plan_apply::verify(&given, &plan, UPLOAD)?;
    finish(upload_run(&upload, backend.as_mut(), &store, false))
}

// ---- cloud delete ----

const DELETE_WARNING: &str = "This deletes cloud files and cannot be undone. The index backup taken first (restorable with `ttskit cloud restore`, which is not an MCP tool) holds only the index files CloudInfo.bson and CloudFolder.bson, not the content of the deleted files. Saves that use a deleted file's download URL stop loading it.";

fn delete_target(args: &CloudDeleteArgs) -> Result<ttskit_cloud::DeleteTarget<'_>, String> {
    match (args.name.as_deref(), args.folder.as_deref()) {
        (Some(name), None) => Ok(ttskit_cloud::DeleteTarget::File(name)),
        (None, Some(folder)) => Ok(ttskit_cloud::DeleteTarget::Folder(folder)),
        _ => Err("give exactly one of name (a file) and folder".to_owned()),
    }
}

fn delete_run(
    target: ttskit_cloud::DeleteTarget<'_>,
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    yes: bool,
) -> Ran {
    run_cli(|out| cloud::delete_files(backend, store, target, yes, true, SystemTime::now(), out))
}

/// What a delete plan depends on: the files and folders it lists.
fn delete_hashed(doc: &Value) -> Value {
    json!({
        "target": doc["target"],
        "folder": doc["folder"],
        "files": doc["files"],
        "folders": doc["folders"],
    })
}

fn delete_plan_of(ran: Ran) -> Result<Plan, Value> {
    let mut plan = make_plan(ran, delete_hashed)?;
    plan.body["warning"] = json!(DELETE_WARNING);
    Ok(plan)
}

pub(super) fn delete_plan(
    factory: &BackendFactory,
    backup_dir: &Option<PathBuf>,
    args: &CloudDeleteArgs,
) -> Result<Value, Value> {
    let target = delete_target(args).map_err(error)?;
    let store = backup_store(backup_dir).map_err(error)?;
    let mut backend = factory().map_err(error)?;
    let ran = delete_run(target, backend.as_mut(), &store, false);
    Ok(delete_plan_of(ran)?.result(DELETE))
}

pub(super) fn delete_apply(
    factory: &BackendFactory,
    backup_dir: &Option<PathBuf>,
    args: &CloudDeleteApplyArgs,
) -> Result<Value, Value> {
    let given =
        plan_apply::require_confirmation(args.fingerprint.as_deref(), args.confirm, DELETE)?;
    let target = delete_target(&args.plan).map_err(error)?;
    let store = backup_store(backup_dir).map_err(error)?;
    let mut backend = factory().map_err(error)?;
    let ran = delete_run(target, backend.as_mut(), &store, false);
    let plan = delete_plan_of(ran)?;
    plan_apply::verify(&given, &plan, DELETE)?;
    finish(delete_run(target, backend.as_mut(), &store, true))
}

// ---- cloud sync ----

struct Project {
    project: ProjectConfig,
    config_dir: PathBuf,
    dir: PathBuf,
}

fn project_prepare(root: &Path, args: &CloudSyncArgs) -> Result<Project, String> {
    let config = resolve_in_root(
        root,
        Path::new(args.config.as_deref().unwrap_or(ProjectConfig::FILE_NAME)),
    )?;
    let project = ProjectConfig::load(&config).map_err(|e| e.to_string())?;
    let config_dir = config_dir(&config);
    let dir = local_dir(root, &project, &config_dir)?;
    Ok(Project {
        project,
        config_dir,
        dir,
    })
}

/// The steps with `action` of a sync plan document.
fn steps<'a>(doc: &'a Value, action: &'a str) -> impl Iterator<Item = &'a Value> {
    doc["plan"]["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(move |s| s["action"] == action)
}

// push

struct Push {
    project: Project,
    local: Vec<ttskit_cloud::LocalFile>,
    ignore: ttskit_cloud::IgnoreSet,
    ignored: usize,
}

fn push_prepare(root: &Path, args: &CloudSyncArgs) -> Result<Push, String> {
    let project = project_prepare(root, args)?;
    let ignore = project.project.ignore_set();
    let scan =
        ttskit_cloud::scan_local_ignoring(&project.dir, &ignore).map_err(|e| e.to_string())?;
    Ok(Push {
        project,
        local: scan.files,
        ignore,
        ignored: scan.ignored,
    })
}

fn push_run(
    push: &Push,
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    dry_run: bool,
    prune: bool,
) -> Ran {
    let manifest = push
        .project
        .config_dir
        .join(ttskit_cloud::Manifest::FILE_NAME);
    run_cli(|out| {
        cloud::sync_push(
            backend,
            store,
            &push.local,
            &|rel| std::fs::read(push.project.dir.join(rel)),
            &push.project.project.cloud_folder,
            prune,
            &push.ignore,
            push.ignored,
            Some(&manifest),
            RunMode {
                dry_run,
                yes: !dry_run,
                json: true,
            },
            SystemTime::now(),
            &cloud::format_date(&jiff::Zoned::now()),
            &mut never_ask,
            out,
        )
    })
}

/// The plan without the quota and the outcome fields, and the content of the
/// files it uploads.
fn push_hashed(push: &Push, doc: &Value) -> Value {
    let files: serde_json::Map<String, Value> = steps(doc, "upload")
        .filter_map(|s| s["path"].as_str())
        .map(|path| {
            let hash = match std::fs::read(push.project.dir.join(path)) {
                Ok(bytes) => json!(sha256_hex(&bytes)),
                Err(_) => json!("unreadable"),
            };
            (path.to_owned(), hash)
        })
        .collect();
    json!({ "prune": doc["plan"]["prune"], "plan": doc["plan"], "files": files })
}

pub(super) fn push_plan(
    root: &Path,
    factory: &BackendFactory,
    backup_dir: &Option<PathBuf>,
    args: &CloudSyncArgs,
) -> Result<Value, Value> {
    let push = push_prepare(root, args).map_err(error)?;
    let store = backup_store(backup_dir).map_err(error)?;
    let mut backend = factory().map_err(error)?;
    let prune = args.prune.unwrap_or(false);
    let ran = push_run(&push, backend.as_mut(), &store, true, prune);
    let mut plan = make_plan(ran, |doc| push_hashed(&push, doc))?;
    set_warning(&mut plan, prune_warning(prune, PUSH_PRUNE));
    Ok(plan.result(SYNC_PUSH))
}

pub(super) fn push_apply(
    root: &Path,
    factory: &BackendFactory,
    backup_dir: &Option<PathBuf>,
    args: &CloudSyncApplyArgs,
) -> Result<Value, Value> {
    let given =
        plan_apply::require_confirmation(args.fingerprint.as_deref(), args.confirm, SYNC_PUSH)?;
    let push = push_prepare(root, &args.plan).map_err(error)?;
    let store = backup_store(backup_dir).map_err(error)?;
    let mut backend = factory().map_err(error)?;
    let prune = args.plan.prune.unwrap_or(false);
    let ran = push_run(&push, backend.as_mut(), &store, true, prune);
    let mut plan = make_plan(ran, |doc| push_hashed(&push, doc))?;
    set_warning(&mut plan, prune_warning(prune, PUSH_PRUNE));
    plan_apply::verify(&given, &plan, SYNC_PUSH)?;
    // A plan with conflicts is refused by the command itself.
    finish(push_run(&push, backend.as_mut(), &store, false, prune))
}

// pull

struct Pull {
    project: Project,
    local: Vec<ttskit_cloud::LocalFile>,
    ignore: ttskit_cloud::IgnoreSet,
    ignored: usize,
}

fn pull_prepare(root: &Path, args: &CloudSyncArgs) -> Result<Pull, String> {
    let project = project_prepare(root, args)?;
    let ignore = project.project.ignore_set();
    // A directory that does not exist yet is an empty one, as for the CLI.
    let (local, ignored) = if project.dir.exists() {
        let scan =
            ttskit_cloud::scan_local_ignoring(&project.dir, &ignore).map_err(|e| e.to_string())?;
        (scan.files, scan.ignored)
    } else {
        (Vec::new(), 0)
    };
    Ok(Pull {
        project,
        local,
        ignore,
        ignored,
    })
}

fn pull_run(
    pull: &Pull,
    backend: &dyn CloudBackend,
    local_dir: &LocalDirFactory,
    dry_run: bool,
    prune: bool,
) -> Ran {
    let mut dir = local_dir(pull.project.dir.clone());
    run_cli(|out| {
        cloud::sync_pull(
            backend,
            dir.as_mut(),
            &pull.local,
            &pull.project.project.cloud_folder,
            prune,
            &pull.ignore,
            pull.ignored,
            RunMode {
                dry_run,
                yes: !dry_run,
                json: true,
            },
            &mut never_ask,
            out,
        )
    })
}

fn pull_hashed(doc: &Value) -> Value {
    json!({ "prune": doc["plan"]["prune"], "plan": doc["plan"] })
}

pub(super) fn pull_plan(
    root: &Path,
    factory: &BackendFactory,
    local_dir: &LocalDirFactory,
    args: &CloudSyncArgs,
) -> Result<Value, Value> {
    let pull = pull_prepare(root, args).map_err(error)?;
    let backend = factory().map_err(error)?;
    let prune = args.prune.unwrap_or(false);
    let ran = pull_run(&pull, backend.as_ref(), local_dir, true, prune);
    let mut plan = make_plan(ran, pull_hashed)?;
    set_warning(&mut plan, prune_warning(prune, PULL_PRUNE));
    Ok(plan.result(SYNC_PULL))
}

pub(super) fn pull_apply(
    root: &Path,
    factory: &BackendFactory,
    local_dir: &LocalDirFactory,
    args: &CloudSyncApplyArgs,
) -> Result<Value, Value> {
    let given =
        plan_apply::require_confirmation(args.fingerprint.as_deref(), args.confirm, SYNC_PULL)?;
    let pull = pull_prepare(root, &args.plan).map_err(error)?;
    let backend = factory().map_err(error)?;
    let prune = args.plan.prune.unwrap_or(false);
    let ran = pull_run(&pull, backend.as_ref(), local_dir, true, prune);
    let mut plan = make_plan(ran, pull_hashed)?;
    set_warning(&mut plan, prune_warning(prune, PULL_PRUNE));
    plan_apply::verify(&given, &plan, SYNC_PULL)?;
    finish(pull_run(&pull, backend.as_ref(), local_dir, false, prune))
}
