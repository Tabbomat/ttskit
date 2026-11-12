//! The `cloud` subcommands.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::json;
use ttskit_cloud::{
    Action, Backup, BackupError, BackupStore, CloudBackend, CloudError, CloudIndex, DeleteError,
    DeletePlan, DeleteTarget, FileState, IndexEntry, IndexError, LocalDir, LocalFile, Manifest,
    ManifestEntry, PullError, PushError, Reason, RestoreAction, StatusError, TsvError, UgcUrl,
    UploadError, UploadPlan, compare, delete, find_entries, is_game_internal, normalize_folder,
    plan_delete, plan_pull, plan_push, plan_restore, plan_upload, pull, push, restore, upload,
};

/// Errors of the `cloud` subcommands.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// The cloud backend failed.
    #[error(transparent)]
    Cloud(#[from] CloudError),
    /// The folder index in the cloud cannot be read.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// The requested folder does not exist.
    #[error("no cloud folder \"{0}\"")]
    FolderNotFound(String),
    /// A backup or restore failed.
    #[error(transparent)]
    Backup(#[from] BackupError),
    /// No default backup directory can be determined.
    #[error("cannot determine the local data directory; use --backup-dir")]
    NoBackupDir,
    /// Writing the output failed.
    #[error("cannot write output: {0}")]
    Io(#[from] io::Error),
    /// Planning or running an upload failed.
    #[error(transparent)]
    Upload(#[from] UploadError),
    /// Planning or running a delete failed.
    #[error(transparent)]
    Delete(DeleteError),
    /// Planning or running a push failed.
    #[error(transparent)]
    Push(PushError),
    /// The push plan has conflicts.
    #[error("cannot push: {0} conflict(s) in the plan; rename the files involved")]
    PushConflicts(usize),
    /// Planning or running a pull failed.
    #[error(transparent)]
    Pull(PullError),
    /// The pull plan has conflicts.
    #[error("cannot pull: {0} conflict(s) in the plan; rename the files involved")]
    PullConflicts(usize),
    /// The local directory cannot be scanned.
    #[error(transparent)]
    Status(#[from] StatusError),
    /// The file to upload cannot be read.
    #[error("cannot read {}: {source}", path.display())]
    ReadFile {
        /// The file.
        path: PathBuf,
        /// Why reading failed.
        source: io::Error,
    },
    /// The save file is not valid.
    #[error("{} is not a valid save: {source}", path.display())]
    InvalidSave {
        /// The save file.
        path: PathBuf,
        /// Why parsing failed.
        source: ttskit_core::SaveError,
    },
    /// The file name is not usable as a cloud name.
    #[error("cannot use the file name of {} as a cloud name; give one with --name", path.display())]
    NameNotUtf8 {
        /// The file.
        path: PathBuf,
    },
    /// No file matches the query.
    #[error("no cloud file \"{0}\"")]
    NotFound(String),
    /// The file exists but the index does not know it.
    #[error("{0} is not in the cloud index; use cloud upload")]
    NotIndexed(String),
    /// Several index entries match the query.
    #[error("{query} matches several files; give the folder path or the cloud name:\n{matches}")]
    Ambiguous {
        /// What the user asked for.
        query: String,
        /// One line per match.
        matches: String,
    },
    /// The entry has no URL and sharing was not requested.
    #[error("{0} has no URL yet; run again with --share to share it")]
    NoUrl(String),
    /// A confirmation is needed but cannot be asked for.
    #[error("confirmation needed: run again with --yes (or --dry-run)")]
    ConfirmationNeeded,
    /// The cloud work is done but the manifest could not be updated.
    #[error("the {what} succeeded, but the manifest {} could not be updated: {reason}", path.display())]
    ManifestUpdate {
        /// What succeeded, e.g. "push".
        what: &'static str,
        /// The manifest file.
        path: PathBuf,
        /// Why the update failed.
        reason: String,
    },
    /// The manifest cannot be read.
    #[error("cannot read the manifest {}: {reason}", path.display())]
    ManifestRead {
        /// The manifest file.
        path: PathBuf,
        /// Why reading failed.
        reason: String,
    },
    /// The manifest cannot be written; nothing else was changed.
    #[error("cannot write the manifest {}: {reason}", path.display())]
    ManifestWrite {
        /// The manifest file.
        path: PathBuf,
        /// Why writing failed.
        reason: String,
    },
    /// The TSV text has problems, one per line.
    #[error(transparent)]
    Tsv(#[from] TsvError),
}

impl From<DeleteError> for CommandError {
    /// Uses the CLI's own errors where they exist, so messages stay the same
    /// across commands.
    fn from(e: DeleteError) -> Self {
        match e {
            DeleteError::NotFound { query } => CommandError::NotFound(query),
            DeleteError::FolderNotFound { folder } => CommandError::FolderNotFound(folder),
            DeleteError::Ambiguous { query, matches } => CommandError::Ambiguous {
                query,
                matches: matches
                    .iter()
                    .map(|(path, key)| format!("  {path}  {key}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            },
            DeleteError::Cloud(e) => CommandError::Cloud(e),
            DeleteError::Index(e) => CommandError::Index(e),
            DeleteError::Backup(e) => CommandError::Backup(e),
            other => CommandError::Delete(other),
        }
    }
}

impl From<PushError> for CommandError {
    /// Uses the CLI's own errors where they exist, so messages stay the same
    /// across commands.
    fn from(e: PushError) -> Self {
        match e {
            PushError::Cloud(e) => CommandError::Cloud(e),
            PushError::Index(e) => CommandError::Index(e),
            PushError::Backup(e) => CommandError::Backup(e),
            other => CommandError::Push(other),
        }
    }
}

impl From<PullError> for CommandError {
    /// Uses the CLI's own errors where they exist, so messages stay the same
    /// across commands.
    fn from(e: PullError) -> Self {
        match e {
            PullError::Cloud(e) => CommandError::Cloud(e),
            PullError::Index(e) => CommandError::Index(e),
            other => CommandError::Pull(other),
        }
    }
}

/// Asks the user a yes/no question; gets the prompt text.
pub type Confirm<'a> = &'a mut dyn FnMut(&str) -> Result<bool, CommandError>;

/// How a changing command runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunMode {
    /// Only print the plan.
    pub dry_run: bool,
    /// Do not ask for confirmation.
    pub yes: bool,
    /// Print one JSON object instead of text.
    pub json: bool,
}

/// The directory holding the cloud index backups: `explicit` if given,
/// otherwise `<local app data>/ttskit/cloud-backups`.
pub fn backup_dir(explicit: Option<PathBuf>) -> Result<PathBuf, CommandError> {
    match explicit {
        Some(dir) => Ok(dir),
        None => dirs::data_local_dir()
            .map(|d| d.join("ttskit").join("cloud-backups"))
            .ok_or(CommandError::NoBackupDir),
    }
}

/// Lists the backups in `store`, newest first.
pub fn list_backups(store: &BackupStore, out: &mut dyn io::Write) -> Result<(), CommandError> {
    let backups = store.list()?;
    let dir = store.root().display();
    let mut buf: Vec<u8> = Vec::new();
    if backups.is_empty() {
        writeln!(buf, "No backups in {dir}")?;
    } else {
        writeln!(buf, "Backups in {dir}:")?;
        let width = backups.iter().map(|b| b.name.len()).max().unwrap_or(0);
        for backup in &backups {
            let files: Vec<String> = backup
                .files
                .iter()
                .map(|f| match f.size {
                    Some(size) => format!("{} {size} bytes", f.name),
                    None => format!("{} absent", f.name),
                })
                .collect();
            writeln!(buf, "  {:<width$}  {}", backup.name, files.join(", "))?;
        }
    }
    out.write_all(&buf)?;
    Ok(())
}

/// Plans restoring `backup` and prints the plan. With `yes` the restore is
/// carried out too (after backing up the current index); without it nothing
/// is changed.
pub fn restore_backup(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    backup: &Backup,
    yes: bool,
    now: SystemTime,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let plan = plan_restore(backend, backup)?;
    let mut buf: Vec<u8> = Vec::new();
    writeln!(buf, "Restore backup {}:", backup.name)?;
    for step in &plan.steps {
        let (action, detail) = match step.action {
            RestoreAction::Write { size } => ("write", format!(" ({size} bytes)")),
            RestoreAction::Delete => ("delete", String::new()),
            RestoreAction::Unchanged => ("unchanged", String::new()),
        };
        writeln!(buf, "  {action:<9}  {}{detail}", step.file)?;
    }
    if plan.is_noop() {
        writeln!(
            buf,
            "Nothing to do: the cloud index already matches this backup."
        )?;
    } else if !yes {
        writeln!(
            buf,
            "Dry run: nothing was changed. Run again with --yes to restore."
        )?;
    }
    out.write_all(&buf)?;
    if plan.is_noop() || !yes {
        return Ok(());
    }

    let outcome = restore(backend, store, &plan, now)?;
    if let Some(pre) = &outcome.pre_restore_backup {
        writeln!(out, "Backed up the current cloud index to {}.", pre.name)?;
    }
    writeln!(out, "Restored backup {}.", backup.name)?;
    Ok(())
}

/// Reads the file to upload and works out its cloud display name: `name` if
/// given, otherwise the file's own name.
pub fn read_upload_file(
    path: &Path,
    name: Option<&str>,
) -> Result<(String, Vec<u8>), CommandError> {
    let data = std::fs::read(path).map_err(|source| CommandError::ReadFile {
        path: path.to_owned(),
        source,
    })?;
    let name = match name {
        Some(name) => name.to_owned(),
        None => path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| CommandError::NameNotUtf8 {
                path: path.to_owned(),
            })?
            .to_owned(),
    };
    Ok((name, data))
}

/// Formats a time the way the game writes dates in the index:
/// `MM/dd/yyyy HH:mm:ss`, 24 hours.
pub fn format_date(time: &jiff::Zoned) -> String {
    time.strftime("%m/%d/%Y %H:%M:%S").to_string()
}

/// The `/`-separated path of an entry, e.g. `Decks/back.png`.
fn display_path(folder: &str, name: &str) -> String {
    let folder = normalize_folder(folder);
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

fn slashes(folder: &str) -> String {
    folder.replace('\\', "/")
}

enum Decision {
    DryRun,
    Cancelled,
    Go,
}

/// Applies `--dry-run`, `--yes` or asks `prompt`. Prints the outcome line
/// unless the output is JSON.
fn decide(
    mode: RunMode,
    prompt: &str,
    confirm: Confirm<'_>,
    out: &mut dyn io::Write,
) -> Result<Decision, CommandError> {
    if mode.dry_run {
        if !mode.json {
            writeln!(out, "Dry run: nothing was changed.")?;
        }
        return Ok(Decision::DryRun);
    }
    if mode.yes || confirm(prompt)? {
        return Ok(Decision::Go);
    }
    if !mode.json {
        writeln!(out, "Cancelled: nothing was changed.")?;
    }
    Ok(Decision::Cancelled)
}

fn write_pretty_json(
    value: &serde_json::Value,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let mut text = serde_json::to_string_pretty(value).map_err(io::Error::from)?;
    text.push('\n');
    out.write_all(text.as_bytes())?;
    Ok(())
}

/// Loads the manifest at `path`, lets `update` change it and saves it. Every
/// failure becomes [`CommandError::ManifestUpdate`] for the finished `what`.
fn update_manifest(
    path: &Path,
    what: &'static str,
    update: impl FnOnce(&mut Manifest) -> Result<(), String>,
) -> Result<(), CommandError> {
    let fail = |reason: String| CommandError::ManifestUpdate {
        what,
        path: path.to_owned(),
        reason,
    };
    let mut manifest = Manifest::load(path).map_err(|e| fail(e.to_string()))?;
    update(&mut manifest).map_err(&fail)?;
    manifest.save(path).map_err(|e| fail(e.to_string()))
}

/// The path of `file` relative to `dir`, `/` separated, or `None` when the
/// file is not below `dir` (or one of them cannot be resolved or is not
/// UTF-8). Both paths are canonicalized, so they must exist.
pub fn local_path_in(dir: &Path, file: &Path) -> Option<String> {
    let dir = std::fs::canonicalize(dir).ok()?;
    let file = std::fs::canonicalize(file).ok()?;
    let rel = file.strip_prefix(&dir).ok()?;
    let parts: Vec<&str> = rel
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Where `cloud upload` records the file in the project manifest.
pub struct ManifestTarget {
    /// The manifest file.
    pub file: PathBuf,
    /// The local path of the uploaded file relative to the project's local
    /// directory, `/` separated; `None` if it is outside.
    pub local_path: Option<String>,
}

/// What to upload and where.
pub struct UploadRequest<'a> {
    /// The display name in the cloud.
    pub name: &'a str,
    /// The content.
    pub data: &'a [u8],
    /// The target folder, `/` or `\` separated; empty for the root.
    pub folder: &'a str,
    /// Dry run, confirmation and output format.
    pub mode: RunMode,
    /// The manifest to record a completed upload in, if any.
    pub manifest: Option<ManifestTarget>,
}

fn write_upload_plan(plan: &UploadPlan, out: &mut Vec<u8>) -> io::Result<()> {
    if plan.display_folder.is_empty() {
        writeln!(
            out,
            "Upload {} ({} bytes) to the root folder:",
            plan.name, plan.size
        )?;
    } else {
        writeln!(
            out,
            "Upload {} ({} bytes) to {}:",
            plan.name, plan.size, plan.display_folder
        )?;
    }
    for folder in &plan.new_folders {
        writeln!(out, "  {:<13}  {}", "create folder", slashes(folder))?;
    }
    if plan.write {
        writeln!(out, "  {:<13}  {}", "write", plan.key)?;
    }
    if plan.share {
        writeln!(out, "  {:<13}  {}", "share", plan.key)?;
    }
    if plan.add_entry {
        writeln!(
            out,
            "  {:<13}  {}",
            "add entry",
            display_path(&plan.folder, &plan.name)
        )?;
    }
    for warning in &plan.warnings {
        writeln!(out, "warning: {warning}")?;
    }
    Ok(())
}

/// Uploads `req.data` to the cloud, shares it and records it in the index.
///
/// The plan is printed (and flushed) before anything is asked or changed. With
/// `--json` only one JSON object is printed, at the end. The URL is the last
/// line of the text output. A completed upload (also "already uploaded") is
/// recorded in `req.manifest`; if that fails, the output is printed first and
/// the error returned afterwards.
pub fn upload_file(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    req: &UploadRequest<'_>,
    now: SystemTime,
    date: &str,
    confirm: Confirm<'_>,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let mode = req.mode;
    let plan = plan_upload(backend, req.name, req.data, req.folder)?;
    let noop = plan.is_noop();

    if !mode.json {
        let mut buf = Vec::new();
        if noop {
            let url = plan.existing_url.as_deref().unwrap_or_default();
            writeln!(buf, "Already uploaded: {url}")?;
        } else {
            write_upload_plan(&plan, &mut buf)?;
        }
        out.write_all(&buf)?;
        out.flush()?;
    }

    let mut done = false;
    let mut url = None;
    let mut backup = None;
    let mut print_url = false;
    if noop {
        done = true;
        url.clone_from(&plan.existing_url);
    } else {
        match decide(mode, "Upload?", confirm, out)? {
            Decision::Go => {
                let outcome = upload(backend, store, &plan, now, date)?;
                done = true;
                backup = outcome.backup.map(|b| b.name);
                url = Some(outcome.url);
                if !mode.json {
                    if let Some(name) = &backup {
                        writeln!(out, "Backed up the cloud index to {name}.")?;
                    }
                    print_url = true;
                }
            }
            Decision::DryRun | Decision::Cancelled => {}
        }
    }

    let mut manifest_file = None;
    let mut manifest_error = None;
    // A noop counts as done even for a dry run, which must write nothing.
    if let (true, Some(target)) = (done && !mode.dry_run, &req.manifest) {
        let entry = ManifestEntry {
            path: target.local_path.clone(),
            cloud_path: display_path(&plan.display_folder, &plan.name),
            sha1: plan.sha1.to_ascii_uppercase(),
            size: plan.size,
            url: url.clone().unwrap_or_default(),
        };
        match update_manifest(&target.file, "upload", |m| {
            m.upsert(entry);
            Ok(())
        }) {
            Ok(()) => {
                manifest_file = Some(target.file.display().to_string());
                if !mode.json {
                    writeln!(out, "Updated manifest {}.", target.file.display())?;
                }
            }
            Err(e) => manifest_error = Some(e),
        }
    }
    if print_url {
        writeln!(out, "{}", url.as_deref().unwrap_or_default())?;
    }

    if mode.json {
        let new_folders: Vec<String> = plan.new_folders.iter().map(|f| slashes(f)).collect();
        let warnings: Vec<String> = plan.warnings.iter().map(ToString::to_string).collect();
        write_pretty_json(
            &json!({
                "name": plan.name,
                "cloud_name": plan.key,
                "folder": plan.display_folder,
                "size": plan.size,
                "sha1": plan.sha1,
                "new_folders": new_folders,
                "write": plan.write,
                "share": plan.share,
                "add_entry": plan.add_entry,
                "warnings": warnings,
                "dry_run": mode.dry_run,
                "done": done,
                "url": url,
                "backup": backup,
                "manifest": manifest_file,
            }),
            out,
        )?;
    }
    out.flush()?;
    manifest_error.map_or(Ok(()), Err)
}

/// Makes the cloud folder `folder` match the scanned local files: uploads new
/// files and, with `prune`, replaces changed files and deletes cloud files
/// that are not local.
///
/// The plan is printed (and flushed) before anything is asked or changed. A
/// plan with conflicts is printed and then refused, even for a dry run. With
/// `--json` only one JSON object is printed, at the end (before the error for
/// a plan with conflicts).
///
/// With a `manifest` path, a push that completed (also when there was nothing
/// to do, but never for a dry run, a cancelled push or a failure) refreshes
/// that manifest from the local files and the new cloud index. If that fails,
/// the output is printed first and the error returned afterwards.
#[allow(clippy::too_many_arguments)]
pub fn sync_push(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    local: &[LocalFile],
    read_local: &dyn Fn(&str) -> io::Result<Vec<u8>>,
    folder: &str,
    prune: bool,
    manifest: Option<&Path>,
    mode: RunMode,
    now: SystemTime,
    date: &str,
    confirm: Confirm<'_>,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let plan = plan_push(backend, local, folder, prune)?;
    let conflicts = plan.sync.count(Action::Conflict);
    let noop = plan.is_noop();
    let needs_prune = plan
        .sync
        .steps
        .iter()
        .filter(|s| s.reason == Reason::NeedsPrune)
        .count();

    if !mode.json {
        out.write_all(plan.sync.render_text().as_bytes())?;
        out.flush()?;
    }

    let plan_json = |done: bool,
                     uploaded: &[String],
                     deleted: &[String],
                     backup: &Option<String>,
                     manifest: &Option<String>| {
        json!({
            "plan": plan.sync.to_json(),
            "dry_run": mode.dry_run,
            "done": done,
            "uploaded": uploaded,
            "deleted": deleted,
            "backup": backup,
            "manifest": manifest,
        })
    };

    if conflicts > 0 {
        if mode.json {
            write_pretty_json(&plan_json(false, &[], &[], &None, &None), out)?;
        }
        out.flush()?;
        return Err(CommandError::PushConflicts(conflicts));
    }

    let mut done = false;
    let mut uploaded = Vec::new();
    let mut deleted = Vec::new();
    let mut backup = None;
    if noop {
        done = true;
    } else if let Decision::Go = decide(mode, "Push?", confirm, out)? {
        let mut write_error: Option<io::Error> = None;
        let result = push(backend, store, &plan, read_local, now, date, &mut |step| {
            if mode.json || write_error.is_some() {
                return;
            }
            let label = match step.action {
                Action::Upload => "uploaded",
                Action::Rename => "renamed",
                Action::Delete => "deleted",
                _ => return,
            };
            if let Err(e) = writeln!(out, "  {label:<10} {}", step.path) {
                write_error = Some(e);
            }
        });
        if let Some(e) = write_error {
            return Err(e.into());
        }
        let outcome = result?;
        done = true;
        backup = outcome.backup.map(|b| b.name);
        uploaded = outcome.uploaded;
        deleted = outcome.deleted;
        if !mode.json {
            if let Some(name) = &backup {
                writeln!(out, "Backed up the cloud index to {name}.")?;
            }
            writeln!(
                out,
                "Uploaded {}, deleted {}.",
                count(uploaded.len(), "file"),
                count(deleted.len(), "file")
            )?;
        }
    }

    let mut manifest_file = None;
    let mut manifest_error = None;
    // A noop counts as done even for a dry run, which must write nothing.
    if let (true, Some(path)) = (done && !mode.dry_run, manifest) {
        let refreshed = update_manifest(path, "push", |m| {
            let index = CloudIndex::load(backend).map_err(|e| e.to_string())?;
            m.refresh_for_local(local, folder, &index);
            Ok(())
        });
        match refreshed {
            Ok(()) => {
                manifest_file = Some(path.display().to_string());
                if !mode.json {
                    writeln!(out, "Updated manifest {}.", path.display())?;
                }
            }
            Err(e) => manifest_error = Some(e),
        }
    }

    if mode.json {
        write_pretty_json(
            &plan_json(done, &uploaded, &deleted, &backup, &manifest_file),
            out,
        )?;
    } else if needs_prune > 0 {
        writeln!(
            out,
            "{} not pushed; run again with --prune to replace {}.",
            if needs_prune == 1 {
                "1 changed file was".to_owned()
            } else {
                format!("{needs_prune} changed files were")
            },
            if needs_prune == 1 { "it" } else { "them" }
        )?;
    }
    out.flush()?;
    manifest_error.map_or(Ok(()), Err)
}

/// Makes the local directory match the cloud folder `folder`: downloads new
/// and changed files and, with `prune`, moves local files that are not in the
/// cloud folder to the recycle bin.
///
/// The plan is printed (and flushed) before anything is asked or changed. A
/// plan with conflicts is printed and then refused, even for a dry run. With
/// `--json` only one JSON object is printed, at the end (before the error for
/// a plan with conflicts).
#[allow(clippy::too_many_arguments)]
pub fn sync_pull(
    backend: &dyn CloudBackend,
    dir: &mut dyn LocalDir,
    local: &[LocalFile],
    folder: &str,
    prune: bool,
    mode: RunMode,
    confirm: Confirm<'_>,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let plan = plan_pull(backend, local, folder, prune)?;
    let conflicts = plan.sync.count(Action::Conflict);
    let noop = plan.is_noop();
    let kept = plan
        .sync
        .steps
        .iter()
        .filter(|s| s.reason == Reason::NotPruned)
        .count();

    if !mode.json {
        out.write_all(plan.sync.render_text().as_bytes())?;
        out.flush()?;
    }

    let plan_json = |done: bool, downloaded: &[String], deleted: &[String]| {
        json!({
            "plan": plan.sync.to_json(),
            "dry_run": mode.dry_run,
            "done": done,
            "downloaded": downloaded,
            "deleted": deleted,
        })
    };

    if conflicts > 0 {
        if mode.json {
            write_pretty_json(&plan_json(false, &[], &[]), out)?;
        }
        out.flush()?;
        return Err(CommandError::PullConflicts(conflicts));
    }

    let mut done = false;
    let mut downloaded = Vec::new();
    let mut deleted = Vec::new();
    if noop {
        done = true;
    } else if let Decision::Go = decide(mode, "Pull?", confirm, out)? {
        let mut write_error: Option<io::Error> = None;
        let result = pull(backend, dir, &plan, &mut |step| {
            if mode.json || write_error.is_some() {
                return;
            }
            let label = match step.action {
                Action::Download => "downloaded",
                Action::Rename => "renamed",
                Action::Delete => "trashed",
                _ => return,
            };
            if let Err(e) = writeln!(out, "  {label:<10} {}", step.path) {
                write_error = Some(e);
            }
        });
        if let Some(e) = write_error {
            return Err(e.into());
        }
        let outcome = result?;
        done = true;
        downloaded = outcome.downloaded;
        deleted = outcome.deleted;
        if !mode.json {
            writeln!(
                out,
                "Downloaded {}, moved {} to the recycle bin.",
                count(downloaded.len(), "file"),
                count(deleted.len(), "file")
            )?;
        }
    }

    if mode.json {
        write_pretty_json(&plan_json(done, &downloaded, &deleted), out)?;
    } else if kept > 0 {
        writeln!(
            out,
            "{} kept; run again with --prune to move {} to the recycle bin.",
            if kept == 1 {
                "1 local file was".to_owned()
            } else {
                format!("{kept} local files were")
            },
            if kept == 1 { "it" } else { "them" }
        )?;
    }
    out.flush()?;
    Ok(())
}

/// What to print the URL of.
pub struct UrlRequest<'a> {
    /// A path, display name or cloud name, see [`find_entries`].
    pub query: &'a str,
    /// Share the file if it has no URL yet.
    pub share: bool,
    /// Dry run, confirmation and output format.
    pub mode: RunMode,
}

fn missing_entry_error(backend: &dyn CloudBackend, query: &str) -> CommandError {
    match backend.list() {
        Ok(files) if files.iter().any(|f| f.name.eq_ignore_ascii_case(query)) => {
            CommandError::NotIndexed(query.to_owned())
        }
        Ok(_) => CommandError::NotFound(query.to_owned()),
        Err(e) => e.into(),
    }
}

/// Prints the download URL of a cloud file; with `share` it first shares a
/// file that has no URL yet and records the URL in the index.
pub fn show_url(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    req: &UrlRequest<'_>,
    now: SystemTime,
    confirm: Confirm<'_>,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let mode = req.mode;
    let index = CloudIndex::load(backend)?;
    let entry: IndexEntry = match find_entries(&index, req.query).as_slice() {
        [] => return Err(missing_entry_error(backend, req.query)),
        [one] => (*one).clone(),
        several => {
            let matches = several
                .iter()
                .map(|e| format!("  {}  {}", display_path(&e.folder, &e.name), e.key))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(CommandError::Ambiguous {
                query: req.query.to_owned(),
                matches,
            });
        }
    };
    let path = display_path(&entry.folder, &entry.name);
    let folder = normalize_folder(&entry.folder);

    if let Some(url) = &entry.url {
        if mode.json {
            write_pretty_json(
                &json!({
                    "name": entry.name,
                    "cloud_name": entry.key,
                    "folder": folder,
                    "url": url,
                    "shared": false,
                }),
                out,
            )?;
        } else {
            writeln!(out, "{url}")?;
        }
        return Ok(());
    }
    if !req.share {
        return Err(CommandError::NoUrl(path));
    }

    if !mode.json {
        writeln!(
            out,
            "Share {path} ({}) and record its URL in the index.",
            entry.key
        )?;
        out.flush()?;
    }
    let mut url = None;
    let mut backup = None;
    if let Decision::Go = decide(mode, "Share?", confirm, out)? {
        let handle = backend.share(&entry.key)?;
        let shared = UgcUrl {
            handle,
            sha1: entry.sha1().to_owned(),
        }
        .to_string();
        let mut index = CloudIndex::load(backend)?;
        if let Some(e) = index
            .entries
            .iter_mut()
            .find(|e| e.key.eq_ignore_ascii_case(&entry.key))
        {
            e.url = Some(shared.clone());
        }
        backup = index.save(backend, store, now)?.backup.map(|b| b.name);
        if !mode.json {
            if let Some(name) = &backup {
                writeln!(out, "Backed up the cloud index to {name}.")?;
            }
            writeln!(out, "{shared}")?;
        }
        url = Some(shared);
    }
    if mode.json {
        write_pretty_json(
            &json!({
                "name": entry.name,
                "cloud_name": entry.key,
                "folder": folder,
                "url": url,
                "shared": url.is_some(),
                "dry_run": mode.dry_run,
                "backup": backup,
            }),
            out,
        )?;
    }
    out.flush()?;
    Ok(())
}

/// Compares the scanned local files with the cloud folder `folder` and prints
/// the differences, or JSON when `json` is set. `dir_label` names the local
/// directory in the output. Returns whether everything is in sync.
pub fn status(
    backend: &dyn CloudBackend,
    local: &[LocalFile],
    folder: &str,
    dir_label: &str,
    json: bool,
    out: &mut dyn io::Write,
) -> Result<bool, CommandError> {
    let index = CloudIndex::load(backend)?;
    let cloud_files = backend.list()?;
    let report = compare(&index, &cloud_files, folder, local);
    let in_sync = report.is_in_sync();
    let mut buf: Vec<u8> = Vec::new();

    if json {
        let counts: serde_json::Map<String, serde_json::Value> = FileState::ALL
            .iter()
            .map(|s| (s.as_str().to_owned(), json!(report.count(*s))))
            .collect();
        let files: Vec<serde_json::Value> = report
            .rows
            .iter()
            .map(|r| {
                json!({
                    "path": r.path,
                    "state": r.state.as_str(),
                    "local_size": r.local_size,
                    "local_sha1": r.local_sha1,
                    "cloud_name": r.cloud_name,
                    "url": r.url,
                })
            })
            .collect();
        let mut text = serde_json::to_string_pretty(&json!({
            "dir": dir_label,
            "folder": report.folder,
            "folder_exists": report.folder_exists,
            "in_sync": in_sync,
            "counts": counts,
            "files": files,
        }))
        .map_err(io::Error::from)?;
        text.push('\n');
        buf.extend_from_slice(text.as_bytes());
    } else {
        let target = if report.folder.is_empty() {
            "the root folder".to_owned()
        } else {
            format!("cloud folder {}", report.folder)
        };
        let note = if report.folder_exists {
            ""
        } else {
            " (does not exist yet)"
        };
        writeln!(buf, "Compare {dir_label} with {target}{note}:")?;
        for row in report
            .rows
            .iter()
            .filter(|r| r.state != FileState::Identical)
        {
            let detail = if row.state == FileState::Broken {
                "  (cloud file is gone)"
            } else {
                ""
            };
            writeln!(buf, "  {:<9}  {}{detail}", row.state.as_str(), row.path)?;
        }
        if in_sync {
            writeln!(
                buf,
                "In sync: {} identical.",
                count(report.count(FileState::Identical), "file")
            )?;
        } else {
            let parts: Vec<String> = FileState::ALL
                .iter()
                .filter(|s| report.count(**s) > 0)
                .map(|s| format!("{} {}", report.count(*s), s.as_str()))
                .collect();
            writeln!(buf, "{}", parts.join(", "))?;
        }
    }
    out.write_all(&buf)?;
    out.flush()?;
    Ok(in_sync)
}

fn write_delete_plan(plan: &DeletePlan, out: &mut Vec<u8>) -> io::Result<()> {
    match &plan.folder {
        Some(folder) => writeln!(
            out,
            "Delete folder {folder} ({}, {}):",
            count(plan.files.len(), "file"),
            count(plan.folders.len(), "folder")
        )?,
        None => {
            for file in &plan.files {
                writeln!(out, "Delete {}:", display_path(&file.folder, &file.name))?;
            }
        }
    }
    for file in &plan.files {
        let path = display_path(&file.folder, &file.name);
        if !file.indexed {
            writeln!(out, "  {:<13}  {path}  (not in index)", "delete file")?;
        } else if file.in_cloud {
            writeln!(out, "  {:<13}  {path}  ({})", "delete file", file.key)?;
        } else {
            writeln!(
                out,
                "  {:<13}  {path}  ({}, file already missing)",
                "remove entry", file.key
            )?;
        }
    }
    for folder in &plan.folders {
        writeln!(out, "  {:<13}  {folder}", "remove folder")?;
    }
    let with_url = plan.files.iter().filter(|f| f.url.is_some()).count();
    match with_url {
        0 => {}
        1 => writeln!(
            out,
            "warning: 1 file has a download URL; saves that use it will no longer load it"
        )?,
        n => writeln!(
            out,
            "warning: {n} files have a download URL; saves that use it will no longer load them"
        )?,
    }
    Ok(())
}

/// Deletes a file or a folder with everything below it, and removes it from
/// the index.
///
/// The plan is printed (and flushed) first. Without `yes` that is all: nothing
/// is changed and no backup is taken. With `--json` only one JSON object is
/// printed, at the end.
pub fn delete_files(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    target: DeleteTarget<'_>,
    yes: bool,
    json: bool,
    now: SystemTime,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let plan = plan_delete(backend, target)?;
    if !json {
        let mut buf = Vec::new();
        write_delete_plan(&plan, &mut buf)?;
        out.write_all(&buf)?;
        out.flush()?;
    }

    let mut deleted = Vec::new();
    let mut backup = None;
    if yes {
        let outcome = delete(backend, store, &plan, now)?;
        if !json {
            writeln!(out, "Backed up the cloud index to {}.", outcome.backup.name)?;
            writeln!(out, "Deleted {}.", count(outcome.deleted.len(), "file"))?;
        }
        deleted = outcome.deleted;
        backup = Some(outcome.backup.name);
    } else if !json {
        writeln!(
            out,
            "Dry run: nothing was changed. Run again with --yes to delete."
        )?;
    }

    if json {
        let files: Vec<serde_json::Value> = plan
            .files
            .iter()
            .map(|f| {
                json!({
                    "name": f.name,
                    "cloud_name": f.key,
                    "folder": f.folder,
                    "size": f.size,
                    "url": f.url,
                    "indexed": f.indexed,
                    "in_cloud": f.in_cloud,
                })
            })
            .collect();
        write_pretty_json(
            &json!({
                "target": if plan.folder.is_some() { "folder" } else { "file" },
                "folder": plan.folder,
                "files": files,
                "folders": plan.folders,
                "dry_run": !yes,
                "done": yes,
                "deleted": deleted,
                "backup": backup,
            }),
            out,
        )?;
    }
    out.flush()?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Missing,
    Unindexed,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Missing => "missing",
            Status::Unindexed => "unindexed",
        }
    }
}

/// A file as shown by `cloud list`.
struct FileRow {
    name: String,
    cloud_name: String,
    /// Normalized folder; `None` for files that are not in the index.
    folder: Option<String>,
    size: Option<u64>,
    url: Option<String>,
    date: Option<String>,
    status: Status,
}

fn lower_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_ascii_lowercase()
        .cmp(&b.to_ascii_lowercase())
        .then_with(|| a.cmp(b))
}

fn total_size(files: &[FileRow]) -> u64 {
    files
        .iter()
        .fold(0, |sum, f| sum.saturating_add(f.size.unwrap_or(0)))
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Lists the cloud files and folders as a tree, or as JSON when `json` is set.
/// `folder` limits the output to that folder and its subfolders.
pub fn list(
    backend: &dyn CloudBackend,
    folder: Option<&str>,
    json: bool,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let steam_files = backend.list()?;
    let index = CloudIndex::load(backend)?;
    let all_folders = index.all_folders();

    // Lowercase filter path; `None` shows everything.
    let filter = match folder.map(normalize_folder) {
        Some(f) if !f.is_empty() => {
            let f = f.to_ascii_lowercase();
            if !all_folders.iter().any(|a| a.eq_ignore_ascii_case(&f)) {
                return Err(CommandError::FolderNotFound(
                    folder.unwrap_or_default().to_owned(),
                ));
            }
            Some(f)
        }
        _ => None,
    };
    let in_filter = |path: &str| match &filter {
        None => true,
        Some(f) => {
            let p = path.to_ascii_lowercase();
            p == *f
                || p.strip_prefix(f.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        }
    };

    let folders: Vec<String> = all_folders.into_iter().filter(|f| in_filter(f)).collect();

    let steam: HashMap<String, u64> = steam_files
        .iter()
        .map(|f| (f.name.to_ascii_lowercase(), f.size))
        .collect();
    let mut files: Vec<FileRow> = Vec::new();
    let mut referenced: HashSet<String> = HashSet::new();
    for entry in &index.entries {
        let key = entry.key.to_ascii_lowercase();
        let found = steam.get(&key).copied();
        referenced.insert(key);
        let entry_folder = normalize_folder(&entry.folder);
        if !in_filter(&entry_folder) {
            continue;
        }
        files.push(FileRow {
            name: entry.name.clone(),
            cloud_name: entry.key.clone(),
            folder: Some(entry_folder),
            size: found.or(entry.size),
            url: entry.url.clone(),
            date: entry.date.clone(),
            status: if found.is_some() {
                Status::Ok
            } else {
                Status::Missing
            },
        });
    }
    files.sort_by(|a, b| {
        let key = |f: &FileRow| {
            (
                f.folder.as_deref().unwrap_or("").to_ascii_lowercase(),
                f.name.to_ascii_lowercase(),
            )
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| a.cloud_name.cmp(&b.cloud_name))
    });
    if filter.is_none() {
        let mut unindexed: Vec<FileRow> = steam_files
            .iter()
            .filter(|f| {
                !referenced.contains(&f.name.to_ascii_lowercase()) && !is_game_internal(&f.name)
            })
            .map(|f| FileRow {
                name: f.name.clone(),
                cloud_name: f.name.clone(),
                folder: None,
                size: Some(f.size),
                url: None,
                date: None,
                status: Status::Unindexed,
            })
            .collect();
        unindexed.sort_by(|a, b| lower_cmp(&a.name, &b.name));
        files.extend(unindexed);
    }

    // Build everything first so that nothing is written on an error.
    let mut buf: Vec<u8> = Vec::new();
    if json {
        write_json(&folders, &files, &mut buf)?;
    } else {
        write_tree(&folders, &files, filter.as_deref(), &mut buf)?;
    }
    out.write_all(&buf)?;
    Ok(())
}

fn write_json(
    folders: &[String],
    files: &[FileRow],
    out: &mut Vec<u8>,
) -> Result<(), CommandError> {
    let rows: Vec<_> = files
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "cloud_name": f.cloud_name,
                "folder": f.folder,
                "size": f.size,
                "url": f.url,
                "date": f.date,
                "status": f.status.as_str(),
            })
        })
        .collect();
    let value = json!({
        "folders": folders,
        "files": rows,
        "total_size": total_size(files),
    });
    let text = serde_json::to_string_pretty(&value).map_err(io::Error::from)?;
    out.extend_from_slice(text.as_bytes());
    out.push(b'\n');
    Ok(())
}

/// One line of the tree. Lines with a `size` take part in column alignment.
struct Line {
    label: String,
    size: Option<String>,
    missing: bool,
}

impl Line {
    fn text(label: String) -> Self {
        Line {
            label,
            size: None,
            missing: false,
        }
    }
}

fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

fn last_segment(path: &str) -> &str {
    path.rfind('/').map_or(path, |i| &path[i + 1..])
}

fn emit_children(
    path: &str,
    depth: usize,
    folders: &[String],
    files: &[FileRow],
    lines: &mut Vec<Line>,
) {
    let indent = "  ".repeat(depth);
    let mut subs: Vec<&String> = folders.iter().filter(|f| parent_of(f) == path).collect();
    subs.sort_by(|a, b| lower_cmp(last_segment(a), last_segment(b)));
    for sub in subs {
        lines.push(Line::text(format!("{indent}{}/", last_segment(sub))));
        emit_children(sub, depth + 1, folders, files, lines);
    }
    // `files` is sorted by (folder, name, cloud name) already.
    for f in files.iter().filter(|f| f.folder.as_deref() == Some(path)) {
        lines.push(Line {
            label: format!("{indent}{}", f.name),
            size: Some(f.size.map_or_else(|| "-".to_owned(), |s| s.to_string())),
            missing: f.status == Status::Missing,
        });
    }
}

fn write_tree(
    folders: &[String],
    files: &[FileRow],
    filter: Option<&str>,
    out: &mut Vec<u8>,
) -> Result<(), CommandError> {
    let mut lines: Vec<Line> = Vec::new();
    match filter {
        None => emit_children("", 0, folders, files, &mut lines),
        Some(f) => {
            let mut roots: Vec<&String> = folders
                .iter()
                .filter(|p| p.eq_ignore_ascii_case(f))
                .collect();
            roots.sort();
            for root in roots {
                lines.push(Line::text(format!("{root}/")));
                emit_children(root, 1, folders, files, &mut lines);
            }
        }
    }
    let unindexed: Vec<&FileRow> = files
        .iter()
        .filter(|f| f.status == Status::Unindexed)
        .collect();
    if !unindexed.is_empty() {
        lines.push(Line::text("Not in index:".to_owned()));
        for f in &unindexed {
            lines.push(Line {
                label: format!("  {}", f.name),
                size: f.size.map(|s| s.to_string()),
                missing: false,
            });
        }
    }

    let label_width = lines
        .iter()
        .filter(|l| l.size.is_some())
        .map(|l| l.label.chars().count())
        .max()
        .unwrap_or(0);
    let size_width = lines
        .iter()
        .filter_map(|l| l.size.as_ref())
        .map(String::len)
        .max()
        .unwrap_or(0);
    for line in &lines {
        match &line.size {
            Some(size) => {
                let marker = if line.missing { "  missing" } else { "" };
                writeln!(
                    out,
                    "{:<label_width$}  {size:>size_width$}{marker}",
                    line.label
                )?;
            }
            None => writeln!(out, "{}", line.label)?,
        }
    }

    let missing = files.iter().filter(|f| f.status == Status::Missing).count();
    let not_indexed = unindexed.len();
    let mut footer = format!(
        "{} in {}, {} bytes",
        count(files.len(), "file"),
        count(folders.len(), "folder"),
        total_size(files)
    );
    if missing > 0 {
        footer.push_str(&format!(", {missing} missing"));
    }
    if not_indexed > 0 {
        footer.push_str(&format!(", {not_indexed} not in index"));
    }
    writeln!(out, "{footer}")?;
    Ok(())
}

/// The output format of `cloud manifest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestFormat {
    /// The manifest file content.
    Json,
    /// The `filename<TAB>URL` mapping.
    Tsv,
}

pub fn load_manifest(path: &Path) -> Result<Manifest, CommandError> {
    Manifest::load(path).map_err(|e| CommandError::ManifestRead {
        path: path.to_owned(),
        reason: e.to_string(),
    })
}

/// Prints the manifest at `path` (an empty one if the file is missing).
/// Nothing but the manifest goes to `out`.
pub fn show_manifest(
    path: &Path,
    format: ManifestFormat,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let manifest = load_manifest(path)?;
    let text = match format {
        ManifestFormat::Json => manifest.to_json(),
        ManifestFormat::Tsv => manifest.to_tsv(),
    };
    out.write_all(text.as_bytes())?;
    out.flush()?;
    Ok(())
}

/// The asset URLs of a save, read once so that the Steam-free part of
/// `cloud scan` can run before connecting.
pub struct SaveScan {
    save: PathBuf,
    urls: Vec<ttskit_core::AssetUrl>,
}

/// Reads the save at `path` and walks its asset URLs. Local only.
pub fn read_scan(path: &Path) -> Result<SaveScan, CommandError> {
    let data = std::fs::read(path).map_err(|source| CommandError::ReadFile {
        path: path.to_owned(),
        source,
    })?;
    let save =
        ttskit_core::Save::from_slice(&data).map_err(|source| CommandError::InvalidSave {
            path: path.to_owned(),
            source,
        })?;
    Ok(SaveScan {
        save: path.to_owned(),
        urls: save.asset_urls(),
    })
}

const SCAN_CLASSES: [ttskit_core::HostClass; 3] = [
    ttskit_core::HostClass::Imgur,
    ttskit_core::HostClass::SteamUserContent,
    ttskit_core::HostClass::Other,
];

fn scan_json(scan: &SaveScan) -> serde_json::Map<String, serde_json::Value> {
    let mut hosts = serde_json::Map::new();
    for class in SCAN_CLASSES {
        let group: Vec<_> = scan
            .urls
            .iter()
            .filter(|u| u.host_class() == class)
            .map(|u| json!({"url": u.url, "labels": u.labels, "count": u.count}))
            .collect();
        hosts.insert(class.as_str().to_owned(), group.into());
    }
    let mut object = serde_json::Map::new();
    object.insert(
        "save".into(),
        scan.save.display().to_string().replace('\\', "/").into(),
    );
    object.insert("total".into(), scan.urls.len().into());
    object.insert("hosts".into(), hosts.into());
    object
}

fn write_scan_text(scan: &SaveScan, out: &mut dyn io::Write) -> Result<(), CommandError> {
    let urls = &scan.urls;
    if urls.is_empty() {
        writeln!(out, "No asset URLs found.")?;
        return Ok(());
    }
    for class in SCAN_CLASSES {
        let group: Vec<_> = urls.iter().filter(|u| u.host_class() == class).collect();
        writeln!(out, "{} ({})", class.as_str(), group.len())?;
        for u in group {
            write!(out, "  {}  [{}]", u.url, u.labels.join(", "))?;
            if u.count > 1 {
                write!(out, "  x{}", u.count)?;
            }
            writeln!(out)?;
        }
    }
    writeln!(out, "{} unique URLs", urls.len())?;
    Ok(())
}

/// Lists the asset URLs of the save at `path`, grouped by host. Local only.
pub fn scan(path: &Path, json: bool, out: &mut dyn io::Write) -> Result<(), CommandError> {
    let scan = read_scan(path)?;
    if json {
        return write_pretty_json(&scan_json(&scan).into(), out);
    }
    write_scan_text(&scan, out)
}

/// Prints the scan report of `scan`, then adopts the Steam Cloud files it
/// refers to into `manifest` (loaded from `manifest_file`) without uploading
/// anything: each steamusercontent URL that matches a cloud index entry by
/// handle and SHA1 becomes a path-less manifest entry. Nothing in the cloud is
/// changed.
///
/// URLs of other hosts are ignored. Steam Cloud URLs that match nothing are
/// reported as unmatched and not adopted. A plan with nothing to add asks
/// nothing and leaves the file alone. With `--json` only one JSON object is
/// printed, at the end.
pub fn scan_adopt(
    backend: &dyn CloudBackend,
    scan: &SaveScan,
    mut manifest: Manifest,
    manifest_file: &Path,
    mode: RunMode,
    confirm: Confirm<'_>,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let index = CloudIndex::load(backend)?;
    let indexed: Vec<(Option<UgcUrl>, &ttskit_cloud::IndexEntry)> = index
        .entries
        .iter()
        .map(|e| (e.url.as_deref().and_then(UgcUrl::parse), e))
        .collect();

    let mut rows: Vec<(&'static str, ManifestEntry)> = Vec::new();
    let mut unmatched: Vec<&str> = Vec::new();
    for u in &scan.urls {
        if u.host_class() != ttskit_core::HostClass::SteamUserContent {
            continue;
        }
        let found = UgcUrl::parse(&u.url).and_then(|parsed| {
            indexed
                .iter()
                .find(|(i, _)| {
                    i.as_ref().is_some_and(|i| {
                        i.handle == parsed.handle && i.sha1.eq_ignore_ascii_case(&parsed.sha1)
                    })
                })
                .and_then(|(_, entry)| ManifestEntry::from_index(entry, None))
        });
        let Some(entry) = found else {
            unmatched.push(&u.url);
            continue;
        };
        // Both URL forms of one file lead to the same entry.
        if rows
            .iter()
            .any(|(_, e)| e.cloud_path == entry.cloud_path && e.sha1 == entry.sha1)
        {
            continue;
        }
        let action = if manifest.contains_cloud(&entry.cloud_path, &entry.sha1) {
            "unchanged"
        } else {
            "add"
        };
        rows.push((action, entry));
    }
    let added = rows.iter().filter(|(a, _)| *a == "add").count();
    let unchanged = rows.len() - added;
    let noop = added == 0;

    if !mode.json {
        write_scan_text(scan, out)?;
        let mut buf = Vec::new();
        writeln!(buf, "Adopt into manifest {}:", manifest_file.display())?;
        for (action, e) in &rows {
            writeln!(buf, "  {action:<9}  {}", e.cloud_path)?;
        }
        for url in &unmatched {
            writeln!(buf, "  {:<9}  {url}", "unmatched")?;
        }
        writeln!(
            buf,
            "{added} add, {unchanged} unchanged, {} unmatched",
            unmatched.len()
        )?;
        if noop {
            writeln!(buf, "Nothing to do: nothing to add to the manifest.")?;
        }
        out.write_all(&buf)?;
        out.flush()?;
    }

    let mut done = false;
    let mut write_error = None;
    if noop {
        done = true;
    } else if let Decision::Go = decide(mode, "Adopt?", confirm, out)? {
        for (action, e) in &rows {
            if *action == "add" {
                manifest.upsert(e.clone());
            }
        }
        match manifest.save(manifest_file) {
            Ok(()) => {
                done = true;
                if !mode.json {
                    writeln!(out, "Updated manifest {}.", manifest_file.display())?;
                }
            }
            Err(e) => {
                write_error = Some(CommandError::ManifestWrite {
                    path: manifest_file.to_owned(),
                    reason: e.to_string(),
                });
            }
        }
    }

    if mode.json {
        let entries: Vec<serde_json::Value> = rows
            .iter()
            .map(|(action, e)| {
                json!({
                    "action": action,
                    "cloud_path": e.cloud_path,
                    "sha1": e.sha1,
                    "size": e.size,
                    "url": e.url,
                })
            })
            .collect();
        let mut object = scan_json(scan);
        object.insert(
            "adopt".into(),
            json!({
                "manifest": manifest_file.display().to_string(),
                "dry_run": mode.dry_run,
                "done": done,
                "entries": entries,
                "unmatched": unmatched,
            }),
        );
        write_pretty_json(&object.into(), out)?;
    }
    out.flush()?;
    write_error.map_or(Ok(()), Err)
}

/// Reads a `filename<TAB>URL` file. It must be UTF-8.
pub fn read_tsv_file(path: &Path) -> Result<String, CommandError> {
    let read = |source| CommandError::ReadFile {
        path: path.to_owned(),
        source,
    };
    let data = std::fs::read(path).map_err(read)?;
    String::from_utf8(data).map_err(|e| CommandError::ReadFile {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidData, e),
    })
}

/// Adopts the `filename<TAB>URL` mapping `text` into the manifest at
/// `manifest_file`, resolving the URLs against the cloud index. Nothing in the
/// cloud is changed.
///
/// Problems in the text are returned as [`CommandError::Tsv`] and nothing is
/// written. Otherwise the plan is printed; unless it is a dry run or declined,
/// the entries are upserted into the manifest. A plan with nothing to change
/// asks nothing and leaves the file alone. With `--json` only one JSON object
/// is printed, at the end.
pub fn import_tsv(
    backend: &dyn CloudBackend,
    text: &str,
    manifest_file: &Path,
    mode: RunMode,
    confirm: Confirm<'_>,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let index = CloudIndex::load(backend)?;
    let imported = Manifest::import_tsv(text, &index)?;
    let mut manifest = load_manifest(manifest_file)?;

    let rows: Vec<(&'static str, &ManifestEntry)> = imported
        .iter()
        .map(|entry| {
            let existing: Vec<&ManifestEntry> = manifest
                .entries
                .iter()
                .filter(|e| e.path == entry.path)
                .collect();
            let action = match existing.as_slice() {
                [] => "add",
                [one] if *one == entry => "unchanged",
                _ => "update",
            };
            (action, entry)
        })
        .collect();
    let counts = |action: &str| rows.iter().filter(|(a, _)| *a == action).count();
    let (added, updated, unchanged) = (counts("add"), counts("update"), counts("unchanged"));
    let noop = added + updated == 0;

    if !mode.json {
        let mut buf = Vec::new();
        writeln!(buf, "Import into manifest {}:", manifest_file.display())?;
        for (action, e) in &rows {
            writeln!(
                buf,
                "  {action:<9}  {}  ({})",
                e.path.as_deref().unwrap_or_default(),
                e.cloud_path
            )?;
        }
        writeln!(buf, "{added} add, {updated} update, {unchanged} unchanged")?;
        if noop {
            writeln!(
                buf,
                "Nothing to do: the manifest already has these entries."
            )?;
        }
        out.write_all(&buf)?;
        out.flush()?;
    }

    let mut done = false;
    let mut write_error = None;
    if noop {
        done = true;
    } else if let Decision::Go = decide(mode, "Import?", confirm, out)? {
        for (_, e) in &rows {
            manifest.upsert((*e).clone());
        }
        match manifest.save(manifest_file) {
            Ok(()) => {
                done = true;
                if !mode.json {
                    writeln!(out, "Updated manifest {}.", manifest_file.display())?;
                }
            }
            Err(e) => {
                write_error = Some(CommandError::ManifestWrite {
                    path: manifest_file.to_owned(),
                    reason: e.to_string(),
                });
            }
        }
    }

    if mode.json {
        let entries: Vec<serde_json::Value> = rows
            .iter()
            .map(|(action, e)| {
                json!({
                    "action": action,
                    "path": e.path,
                    "cloud_path": e.cloud_path,
                    "sha1": e.sha1,
                    "size": e.size,
                    "url": e.url,
                })
            })
            .collect();
        write_pretty_json(
            &json!({
                "manifest": manifest_file.display().to_string(),
                "dry_run": mode.dry_run,
                "done": done,
                "entries": entries,
            }),
            out,
        )?;
    }
    out.flush()?;
    write_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use ttskit_cloud::{CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, IndexEntry, MemoryCloud, Op};

    use super::*;

    fn entry(key: &str, name: &str, size: Option<u64>, folder: &str) -> IndexEntry {
        IndexEntry::new(key, name, None, size, None, folder)
    }

    fn seed(cloud: MemoryCloud, index: &CloudIndex) -> MemoryCloud {
        cloud
            .with_file(CLOUD_INFO_FILE, index.encode_info().unwrap())
            .with_file(CLOUD_FOLDER_FILE, index.encode_folders().unwrap())
    }

    /// The example from the feature description.
    fn example() -> MemoryCloud {
        let mut index = CloudIndex::empty();
        index.folders = vec!["Decks/Old".into()];
        index.entries = vec![
            entry("AA_back.png", "back.png", Some(2048), "Decks"),
            entry("BB_front.png", "front.png", Some(12345), "Decks"),
            entry("CC_red.png", "red.png", Some(100), "Tokens/Red"),
            entry("DD_readme.txt", "readme.txt", Some(10), ""),
        ];
        seed(
            MemoryCloud::new()
                .with_file("AA_back.png", vec![0; 2048])
                .with_file("BB_front.png", vec![0; 12345])
                .with_file("DD_readme.txt", vec![0; 10])
                .with_file("stray.png", vec![0; 512]),
            &index,
        )
    }

    fn try_run(
        cloud: &MemoryCloud,
        folder: Option<&str>,
        json: bool,
    ) -> (Result<(), CommandError>, String) {
        let mut out = Vec::new();
        let result = list(cloud, folder, json, &mut out);
        (result, String::from_utf8(out).unwrap())
    }

    fn run(cloud: &MemoryCloud, folder: Option<&str>, json: bool) -> String {
        let (result, text) = try_run(cloud, folder, json);
        result.unwrap();
        text
    }

    fn json_of(cloud: &MemoryCloud, folder: Option<&str>) -> serde_json::Value {
        let text = run(cloud, folder, true);
        assert!(text.ends_with("}\n"));
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn example_tree() {
        let text = run(&example(), None, false);
        println!("{text}");
        assert_eq!(
            text,
            [
                "Decks/",
                "  Old/",
                "  back.png    2048",
                "  front.png  12345",
                "Tokens/",
                "  Red/",
                "    red.png    100  missing",
                "readme.txt      10",
                "Not in index:",
                "  stray.png    512",
                "5 files in 4 folders, 15015 bytes, 1 missing, 1 not in index",
                "",
            ]
            .join("\n")
        );
    }

    #[test]
    fn subfolders_come_before_files_and_sort_case_insensitively() {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_b.txt", "b.txt", Some(1), "x"),
            entry("AA_B.txt", "B.txt", Some(1), "x"),
            entry("AA_a.txt", "a.txt", Some(1), "x/z"),
            entry("AA_c.txt", "c.txt", Some(1), "x/Y"),
            entry("AA_top.txt", "top.txt", Some(1), ""),
        ];
        let text = run(&seed(MemoryCloud::new(), &index), None, false);
        let names: Vec<&str> = text
            .lines()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(
            &names[..8],
            [
                "x/", "Y/", "c.txt", "z/", "a.txt", "B.txt", "b.txt", "top.txt"
            ]
        );
        assert!(text.contains("\n    c.txt"));
        assert!(text.contains("\n  B.txt"));
    }

    #[test]
    fn empty_folder_is_shown() {
        let mut index = CloudIndex::empty();
        index.folders = vec!["Empty".into()];
        let text = run(&seed(MemoryCloud::new(), &index), None, false);
        assert_eq!(text, "Empty/\n0 files in 1 folder, 0 bytes\n");
    }

    #[test]
    fn missing_entry_marker_and_counts() {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_a.png", "a.png", Some(7), ""),
            entry("BB_b.png", "b.png", None, ""),
        ];
        let cloud = seed(MemoryCloud::new(), &index);
        assert_eq!(
            run(&cloud, None, false),
            "a.png  7  missing\nb.png  -  missing\n2 files in 0 folders, 7 bytes, 2 missing\n"
        );
    }

    #[test]
    fn steam_file_is_matched_case_insensitively_and_uses_steam_size() {
        let mut index = CloudIndex::empty();
        index.entries = vec![entry("abcd_a.png", "a.png", Some(7), "")];
        let cloud = seed(
            MemoryCloud::new().with_file("ABCD_A.PNG", vec![0; 3]),
            &index,
        );
        assert_eq!(
            run(&cloud, None, false),
            "a.png  3\n1 file in 0 folders, 3 bytes\n"
        );
    }

    #[test]
    fn same_folder_and_name_with_different_sha1_are_both_shown() {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_a.png", "a.png", Some(1), ""),
            entry("BB_a.png", "a.png", Some(2), ""),
        ];
        let cloud = seed(
            MemoryCloud::new()
                .with_file("AA_a.png", vec![0])
                .with_file("BB_a.png", vec![0; 2]),
            &index,
        );
        assert_eq!(
            run(&cloud, None, false),
            "a.png  1\na.png  2\n2 files in 0 folders, 3 bytes\n"
        );
    }

    #[test]
    fn game_internal_files_are_hidden() {
        let cloud = MemoryCloud::new()
            .with_file("WorkshopUpload", vec![0; 5])
            .with_file("WorkshopImageUpload.png", vec![0; 5]);
        let cloud = seed(cloud, &CloudIndex::empty());
        assert_eq!(run(&cloud, None, false), "0 files in 0 folders, 0 bytes\n");
    }

    #[test]
    fn without_index_all_files_are_not_in_index() {
        let cloud = MemoryCloud::new()
            .with_file("b.png", vec![0; 2])
            .with_file("A.png", vec![0; 10]);
        assert_eq!(
            run(&cloud, None, false),
            "Not in index:\n  A.png  10\n  b.png   2\n2 files in 0 folders, 12 bytes, 2 not in index\n"
        );
    }

    #[test]
    fn empty_cloud_prints_only_the_footer() {
        assert_eq!(
            run(&MemoryCloud::new(), None, false),
            "0 files in 0 folders, 0 bytes\n"
        );
    }

    #[test]
    fn folder_filter_is_recursive_case_insensitive_and_slash_tolerant() {
        for spelling in ["decks", "/DECKS/", "Decks", "//decks//"] {
            let text = run(&example(), Some(spelling), false);
            assert_eq!(
                text,
                [
                    "Decks/",
                    "  Old/",
                    "  back.png    2048",
                    "  front.png  12345",
                    "2 files in 2 folders, 14393 bytes",
                    "",
                ]
                .join("\n"),
                "{spelling}"
            );
        }
    }

    /// The game separates folder levels with `\`.
    fn backslash_example() -> MemoryCloud {
        let mut index = CloudIndex::empty();
        index.folders = vec!["Game".into(), "Game\\dice".into(), "Game\\token".into()];
        index.entries = vec![
            entry("AA_die.obj", "die.obj", Some(3), "Game\\dice"),
            entry("BB_vp.jpg", "vp.jpg", Some(5), "Game\\token"),
        ];
        seed(
            MemoryCloud::new()
                .with_file("AA_die.obj", vec![0; 3])
                .with_file("BB_vp.jpg", vec![0; 5]),
            &index,
        )
    }

    #[test]
    fn backslash_folders_are_nested() {
        assert_eq!(
            run(&backslash_example(), None, false),
            [
                "Game/",
                "  dice/",
                "    die.obj  3",
                "  token/",
                "    vp.jpg   5",
                "2 files in 3 folders, 8 bytes",
                "",
            ]
            .join("\n")
        );
        let value = json_of(&backslash_example(), None);
        assert_eq!(value["folders"], json!(["Game", "Game/dice", "Game/token"]));
        assert_eq!(value["files"][0]["folder"], "Game/dice");
    }

    #[test]
    fn folder_filter_accepts_either_separator() {
        let expected = "Game/dice/\n  die.obj  3\n1 file in 1 folder, 3 bytes\n";
        for spelling in ["game/dice", "Game\\dice", "\\game\\DICE\\"] {
            assert_eq!(
                run(&backslash_example(), Some(spelling), false),
                expected,
                "{spelling}"
            );
        }
        let text = run(&backslash_example(), Some("game"), false);
        assert!(text.starts_with("Game/\n  dice/\n"));
        assert!(text.ends_with("2 files in 3 folders, 8 bytes\n"));
    }

    #[test]
    fn folder_filter_on_nested_folder_shows_full_path() {
        let text = run(&example(), Some("tokens/red"), false);
        assert_eq!(
            text,
            "Tokens/Red/\n  red.png  100  missing\n1 file in 1 folder, 100 bytes, 1 missing\n"
        );
    }

    #[test]
    fn folder_filter_does_not_match_name_prefixes() {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_a.png", "a.png", Some(1), "Deck"),
            entry("BB_b.png", "b.png", Some(1), "Decks"),
        ];
        let cloud = seed(MemoryCloud::new(), &index);
        let text = run(&cloud, Some("Deck"), false);
        assert!(text.starts_with("Deck/\n  a.png"));
        assert!(!text.contains("b.png"));
    }

    #[test]
    fn unknown_folder_is_an_error_and_writes_nothing() {
        for json in [false, true] {
            let (result, text) = try_run(&example(), Some("/Nope/"), json);
            let err = result.unwrap_err();
            assert!(matches!(err, CommandError::FolderNotFound(_)));
            assert_eq!(err.to_string(), "no cloud folder \"/Nope/\"");
            assert!(text.is_empty());
        }
    }

    #[test]
    fn slash_folder_means_everything() {
        let cloud = example();
        assert_eq!(run(&cloud, Some("/"), false), run(&cloud, None, false));
        assert_eq!(run(&cloud, Some(""), true), run(&cloud, None, true));
    }

    #[test]
    fn json_shape_and_order() {
        let mut index = CloudIndex::empty();
        index.folders = vec!["Decks/Old".into()];
        index.entries = vec![
            IndexEntry::new(
                "BB_b.png",
                "b.png",
                Some("http://example.invalid/b".into()),
                Some(5),
                Some("1/2/2020".into()),
                "Decks",
            ),
            entry("AA_a.png", "a.png", None, "Decks"),
            entry("CC_r.png", "r.png", Some(9), ""),
        ];
        let cloud = seed(
            MemoryCloud::new()
                .with_file("BB_b.png", vec![0; 4])
                .with_file("zed.txt", vec![0; 2])
                .with_file("Alpha.txt", vec![0; 1]),
            &index,
        );
        assert_eq!(
            json_of(&cloud, None),
            json!({
                "folders": ["Decks", "Decks/Old"],
                "files": [
                    {"name": "r.png", "cloud_name": "CC_r.png", "folder": "", "size": 9,
                     "url": null, "date": null, "status": "missing"},
                    {"name": "a.png", "cloud_name": "AA_a.png", "folder": "Decks", "size": null,
                     "url": null, "date": null, "status": "missing"},
                    {"name": "b.png", "cloud_name": "BB_b.png", "folder": "Decks", "size": 4,
                     "url": "http://example.invalid/b", "date": "1/2/2020", "status": "ok"},
                    {"name": "Alpha.txt", "cloud_name": "Alpha.txt", "folder": null, "size": 1,
                     "url": null, "date": null, "status": "unindexed"},
                    {"name": "zed.txt", "cloud_name": "zed.txt", "folder": null, "size": 2,
                     "url": null, "date": null, "status": "unindexed"},
                ],
                "total_size": 16,
            })
        );
    }

    #[test]
    fn json_with_folder_filter() {
        let value = json_of(&example(), Some("decks"));
        assert_eq!(value["folders"], json!(["Decks", "Decks/Old"]));
        let names: Vec<_> = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["back.png", "front.png"]);
        assert_eq!(value["total_size"], 14393);
    }

    #[test]
    fn empty_cloud_json() {
        assert_eq!(
            json_of(&MemoryCloud::new(), None),
            json!({"folders": [], "files": [], "total_size": 0})
        );
    }

    #[test]
    fn malformed_index_is_an_error_and_writes_nothing() {
        let cloud = MemoryCloud::new()
            .with_file(CLOUD_INFO_FILE, b"not bson".to_vec())
            .with_file("a.png", vec![0]);
        for json in [false, true] {
            let (result, text) = try_run(&cloud, None, json);
            assert!(matches!(result.unwrap_err(), CommandError::Index(_)));
            assert!(text.is_empty());
        }
    }

    #[test]
    fn singular_forms() {
        let mut index = CloudIndex::empty();
        index.entries = vec![entry("AA_a.png", "a.png", Some(1), "d")];
        let cloud = seed(MemoryCloud::new().with_file("AA_a.png", vec![0]), &index);
        let text = run(&cloud, None, false);
        assert_eq!(text.lines().last(), Some("1 file in 1 folder, 1 bytes"));
    }

    #[test]
    fn total_saturates() {
        let row = |size| FileRow {
            name: "a".into(),
            cloud_name: "a".into(),
            folder: None,
            size: Some(size),
            url: None,
            date: None,
            status: Status::Ok,
        };
        assert_eq!(total_size(&[row(u64::MAX), row(1)]), u64::MAX);
    }

    #[test]
    fn list_failure_propagates_without_output() {
        let cloud = example();
        for json in [false, true] {
            cloud.fail_once(Op::List, None, CloudError::Backend("boom".into()));
            let (result, text) = try_run(&cloud, None, json);
            assert!(matches!(
                result.unwrap_err(),
                CommandError::Cloud(CloudError::Backend(_))
            ));
            assert!(text.is_empty());
        }
    }

    mod restore_tests {
        use std::fs;
        use std::time::{Duration, UNIX_EPOCH};

        use super::*;

        fn index_cloud(name: &str) -> MemoryCloud {
            let mut index = CloudIndex::empty();
            index.entries = vec![entry(&format!("AA_{name}"), name, Some(1), "")];
            seed(MemoryCloud::new(), &index)
        }

        fn time(secs: u64) -> SystemTime {
            UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
        }

        fn names(store: &BackupStore) -> Vec<String> {
            store.list().unwrap().into_iter().map(|b| b.name).collect()
        }

        fn restore_run(
            cloud: &mut MemoryCloud,
            store: &BackupStore,
            name: &str,
            yes: bool,
        ) -> (Result<(), CommandError>, String) {
            let mut out = Vec::new();
            let result = store
                .get(name)
                .map_err(CommandError::from)
                .and_then(|b| restore_backup(cloud, store, &b, yes, time(100), &mut out));
            (result, String::from_utf8(out).unwrap())
        }

        /// A store holding one backup of `index_cloud("a.png")`, and a cloud
        /// that differs from it.
        fn setup() -> (tempfile::TempDir, BackupStore, MemoryCloud, String) {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            let backup = store.create(&index_cloud("a.png"), time(0)).unwrap();
            // The backup lacks CloudFolder.bson: it did not exist in that cloud.
            fs::remove_file(backup.path.join(CLOUD_FOLDER_FILE)).unwrap();
            (dir, store, index_cloud("b.png"), backup.name)
        }

        fn list_text(store: &BackupStore) -> String {
            let mut out = Vec::new();
            list_backups(store, &mut out).unwrap();
            String::from_utf8(out).unwrap()
        }

        #[test]
        fn list_without_backups() {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            assert_eq!(
                list_text(&store),
                format!("No backups in {}\n", dir.path().display())
            );
        }

        #[test]
        fn list_with_missing_dir() {
            let dir = tempfile::tempdir().unwrap();
            let missing = dir.path().join("nope");
            let store = BackupStore::new(&missing);
            assert_eq!(
                list_text(&store),
                format!("No backups in {}\n", missing.display())
            );
        }

        #[test]
        fn list_shows_newest_first_with_absent_files() {
            let dir = tempfile::tempdir().unwrap();
            let older = dir.path().join("20261110T090000Z");
            fs::create_dir(&older).unwrap();
            fs::write(older.join(CLOUD_INFO_FILE), vec![0; 1200]).unwrap();
            let newer = dir.path().join("20261111T184948Z");
            fs::create_dir(&newer).unwrap();
            fs::write(newer.join(CLOUD_INFO_FILE), vec![0; 1234]).unwrap();
            fs::write(newer.join(CLOUD_FOLDER_FILE), vec![0; 56]).unwrap();
            fs::create_dir(dir.path().join("not-a-backup")).unwrap();
            let store = BackupStore::new(dir.path());
            assert_eq!(
                list_text(&store),
                format!(
                    "Backups in {}:\n\
                     \x20 20261111T184948Z  CloudInfo.bson 1234 bytes, CloudFolder.bson 56 bytes\n\
                     \x20 20261110T090000Z  CloudInfo.bson 1200 bytes, CloudFolder.bson absent\n",
                    dir.path().display()
                )
            );
        }

        #[test]
        fn dry_run_prints_the_plan_and_changes_nothing() {
            let (_dir, store, mut cloud, name) = setup();
            cloud.clear_calls();
            let (result, text) = restore_run(&mut cloud, &store, &name, false);
            result.unwrap();
            let size = store.get(&name).unwrap().files[0].size.unwrap();
            assert_eq!(
                text,
                format!(
                    "Restore backup {name}:\n\
                     \x20 write      CloudInfo.bson ({size} bytes)\n\
                     \x20 delete     CloudFolder.bson\n\
                     Dry run: nothing was changed. Run again with --yes to restore.\n"
                )
            );
            assert!(cloud.mutating_calls().is_empty());
            assert_eq!(names(&store), [name]);
        }

        #[test]
        fn yes_restores_and_backs_up_the_current_index() {
            let (_dir, store, mut cloud, name) = setup();
            let old_info = cloud.contents(CLOUD_INFO_FILE).unwrap();
            let old_folders = cloud.contents(CLOUD_FOLDER_FILE).unwrap();
            let (result, text) = restore_run(&mut cloud, &store, &name, true);
            result.unwrap();

            let backup = store.get(&name).unwrap();
            assert_eq!(
                cloud.contents(CLOUD_INFO_FILE),
                backup.read(CLOUD_INFO_FILE).unwrap()
            );
            // The backup has no CloudFolder.bson, so it is deleted.
            assert_eq!(backup.read(CLOUD_FOLDER_FILE).unwrap(), None);
            assert_eq!(cloud.contents(CLOUD_FOLDER_FILE), None);

            let all = names(&store);
            assert_eq!(all.len(), 2);
            let pre = store.get(&all[0]).unwrap();
            assert_ne!(pre.name, name);
            assert_eq!(pre.read(CLOUD_INFO_FILE).unwrap(), Some(old_info));
            assert_eq!(pre.read(CLOUD_FOLDER_FILE).unwrap(), Some(old_folders));
            assert!(text.contains("  delete     CloudFolder.bson\n"));
            assert!(text.ends_with(&format!(
                "Backed up the current cloud index to {}.\nRestored backup {name}.\n",
                pre.name
            )));
        }

        #[test]
        fn matching_cloud_is_a_noop_with_and_without_yes() {
            let (_dir, store, _, name) = setup();
            for yes in [false, true] {
                let mut cloud = index_cloud("a.png");
                cloud.delete(CLOUD_FOLDER_FILE).unwrap();
                cloud.clear_calls();
                let (result, text) = restore_run(&mut cloud, &store, &name, yes);
                result.unwrap();
                assert!(text.contains("  unchanged  CloudInfo.bson\n"));
                assert!(
                    text.ends_with("Nothing to do: the cloud index already matches this backup.\n")
                );
                assert!(!text.contains("Dry run"));
                assert!(cloud.mutating_calls().is_empty());
                assert_eq!(names(&store).as_slice(), std::slice::from_ref(&name));
            }
        }

        #[test]
        fn invalid_backup_content_is_an_error_without_writes() {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            let bad = dir.path().join("20260101T000000Z");
            fs::create_dir(&bad).unwrap();
            fs::write(bad.join(CLOUD_INFO_FILE), b"not bson").unwrap();
            let mut cloud = index_cloud("b.png");
            cloud.clear_calls();
            let (result, text) = restore_run(&mut cloud, &store, "20260101T000000Z", true);
            assert!(matches!(
                result.unwrap_err(),
                CommandError::Backup(BackupError::InvalidBackup { .. })
            ));
            assert!(text.is_empty());
            assert!(cloud.mutating_calls().is_empty());
            assert_eq!(names(&store), ["20260101T000000Z"]);
        }

        #[test]
        fn missing_and_invalid_names_are_errors() {
            let (_dir, store, mut cloud, _) = setup();
            let (result, text) = restore_run(&mut cloud, &store, "20200101T000000Z", true);
            assert!(matches!(
                result.unwrap_err(),
                CommandError::Backup(BackupError::NotFound { .. })
            ));
            assert!(text.is_empty());
            let (result, _) = restore_run(&mut cloud, &store, "nope", true);
            assert!(matches!(
                result.unwrap_err(),
                CommandError::Backup(BackupError::InvalidName { .. })
            ));
        }

        #[test]
        fn backup_dir_prefers_the_explicit_value() {
            let dir = PathBuf::from("some/where");
            assert_eq!(backup_dir(Some(dir.clone())).unwrap(), dir);
        }

        #[test]
        fn backup_dir_default_ends_with_ttskit_cloud_backups() {
            // Nothing to check on machines without a local data directory.
            if let Ok(dir) = backup_dir(None) {
                let parts: Vec<_> = dir.components().rev().take(2).collect();
                assert_eq!(parts[0].as_os_str(), "cloud-backups");
                assert_eq!(parts[1].as_os_str(), "ttskit");
            }
        }
    }

    mod upload_tests {
        use std::time::UNIX_EPOCH;

        use ttskit_cloud::{cloud_key, sha1_hex};

        use super::*;

        const DATE: &str = "10/10/2026 14:05:08";
        const YES: RunMode = RunMode {
            dry_run: false,
            yes: true,
            json: false,
        };
        const DRY: RunMode = RunMode {
            dry_run: true,
            yes: false,
            json: false,
        };
        const ASK: RunMode = RunMode {
            dry_run: false,
            yes: false,
            json: false,
        };

        fn key_of(data: &[u8], name: &str) -> String {
            cloud_key(&sha1_hex(data), name)
        }

        fn url_of(cloud: &MemoryCloud, data: &[u8], name: &str) -> String {
            format!(
                "https://steamusercontent-a.akamaihd.net/ugc/{}/{}/",
                cloud.handle(&key_of(data, name)).unwrap(),
                sha1_hex(data)
            )
        }

        struct Run {
            result: Result<(), CommandError>,
            text: String,
            prompts: Vec<String>,
        }

        fn up(
            cloud: &mut MemoryCloud,
            store: &BackupStore,
            file: (&str, &[u8], &str),
            mode: RunMode,
            answer: bool,
        ) -> Run {
            up_with(cloud, store, file, mode, answer, None)
        }

        fn up_with(
            cloud: &mut MemoryCloud,
            store: &BackupStore,
            (name, data, folder): (&str, &[u8], &str),
            mode: RunMode,
            answer: bool,
            manifest: Option<ManifestTarget>,
        ) -> Run {
            let mut prompts = Vec::new();
            let mut confirm = |p: &str| {
                prompts.push(p.to_owned());
                Ok(answer)
            };
            let mut out = Vec::new();
            let req = UploadRequest {
                name,
                data,
                folder,
                mode,
                manifest,
            };
            let result = upload_file(cloud, store, &req, UNIX_EPOCH, DATE, &mut confirm, &mut out);
            Run {
                result,
                text: String::from_utf8(out).unwrap(),
                prompts,
            }
        }

        fn setup() -> (tempfile::TempDir, BackupStore) {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            (dir, store)
        }

        fn json_mode(dry_run: bool) -> RunMode {
            RunMode {
                dry_run,
                yes: !dry_run,
                json: true,
            }
        }

        #[test]
        fn format_date_pads_and_uses_24_hours() {
            let at = |y, mo, d, h, mi, s| {
                let dt = jiff::civil::date(y, mo, d).at(h, mi, s, 0);
                format_date(&dt.to_zoned(jiff::tz::TimeZone::UTC).unwrap())
            };
            assert_eq!(at(2026, 10, 10, 14, 5, 8), "10/10/2026 14:05:08");
            assert_eq!(at(2026, 3, 4, 7, 0, 9), "03/04/2026 07:00:09");
            assert_eq!(at(2026, 12, 31, 23, 59, 59), "12/31/2026 23:59:59");
        }

        #[test]
        fn dry_run_prints_the_plan_and_changes_nothing() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let key = key_of(b"hello", "deck.png");
            let run = up(
                &mut cloud,
                &store,
                ("deck.png", b"hello", "Decks/Old"),
                DRY,
                false,
            );
            run.result.unwrap();
            assert_eq!(
                run.text,
                format!(
                    "Upload deck.png (5 bytes) to Decks/Old:\n\
                     \x20 create folder  Decks\n\
                     \x20 create folder  Decks/Old\n\
                     \x20 write          {key}\n\
                     \x20 share          {key}\n\
                     \x20 add entry      Decks/Old/deck.png\n\
                     Dry run: nothing was changed.\n"
                )
            );
            assert!(run.prompts.is_empty());
            assert!(cloud.mutating_calls().is_empty());
            assert!(store.list().unwrap().is_empty());
        }

        #[test]
        fn root_folder_wording() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = up(&mut cloud, &store, ("a.png", b"x", ""), DRY, false);
            run.result.unwrap();
            assert!(
                run.text
                    .starts_with("Upload a.png (1 bytes) to the root folder:\n")
            );
            assert!(run.text.contains("  add entry      a.png\n"));
            assert!(!run.text.contains("create folder"));
        }

        #[test]
        fn declined_confirmation_changes_nothing() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = up(&mut cloud, &store, ("a.png", b"x", "F"), ASK, false);
            run.result.unwrap();
            assert_eq!(run.prompts, ["Upload?"]);
            assert!(run.text.starts_with("Upload a.png"));
            assert!(run.text.ends_with("Cancelled: nothing was changed.\n"));
            assert!(cloud.mutating_calls().is_empty());
            assert!(store.list().unwrap().is_empty());
        }

        #[test]
        fn confirmation_error_propagates_after_the_plan() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let mut confirm = |_: &str| Err(CommandError::ConfirmationNeeded);
            let mut out = Vec::new();
            let req = UploadRequest {
                name: "a.png",
                data: b"x",
                folder: "",
                mode: ASK,
                manifest: None,
            };
            let err = upload_file(
                &mut cloud,
                &store,
                &req,
                UNIX_EPOCH,
                DATE,
                &mut confirm,
                &mut out,
            )
            .unwrap_err();
            assert!(matches!(err, CommandError::ConfirmationNeeded));
            assert!(String::from_utf8(out).unwrap().starts_with("Upload a.png"));
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn accepted_confirmation_and_yes_both_upload() {
            for (mode, answer, prompts) in [(ASK, true, 1), (YES, false, 0)] {
                let (_dir, store) = setup();
                let mut cloud = MemoryCloud::new();
                let run = up(&mut cloud, &store, ("a.png", b"hello", "F"), mode, answer);
                run.result.unwrap();
                assert_eq!(run.prompts.len(), prompts);
                assert_eq!(
                    cloud.contents(&key_of(b"hello", "a.png")).unwrap(),
                    b"hello"
                );
                let index = CloudIndex::load(&cloud).unwrap();
                assert_eq!(index.entries.len(), 1);
                assert_eq!(index.entries[0].name, "a.png");
                assert_eq!(index.entries[0].date.as_deref(), Some(DATE));
                assert_eq!(index.folders, ["F"]);
                let backups = store.list().unwrap();
                assert_eq!(backups.len(), 1);
                let lines: Vec<&str> = run.text.lines().collect();
                let url = url_of(&cloud, b"hello", "a.png");
                assert_eq!(lines[lines.len() - 1], url);
                assert_eq!(
                    lines[lines.len() - 2],
                    format!("Backed up the cloud index to {}.", backups[0].name)
                );
                assert_eq!(index.entries[0].url.as_deref(), Some(url.as_str()));
            }
        }

        #[test]
        fn noop_prints_the_existing_url_and_never_asks() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            up(&mut cloud, &store, ("a.png", b"x", "F"), YES, false)
                .result
                .unwrap();
            cloud.clear_calls();
            let run = up(&mut cloud, &store, ("a.png", b"x", "f"), ASK, false);
            run.result.unwrap();
            assert_eq!(
                run.text,
                format!("Already uploaded: {}\n", url_of(&cloud, b"x", "a.png"))
            );
            assert!(run.prompts.is_empty());
            assert!(cloud.mutating_calls().is_empty());
            assert_eq!(store.list().unwrap().len(), 1);
        }

        #[test]
        fn collision_is_an_error_and_prints_nothing() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            up(&mut cloud, &store, ("a.png", b"x", "Other"), YES, false)
                .result
                .unwrap();
            cloud.clear_calls();
            for mode in [YES, json_mode(false)] {
                let run = up(&mut cloud, &store, ("a.png", b"x", "Here"), mode, false);
                assert!(matches!(
                    run.result.unwrap_err(),
                    CommandError::Upload(UploadError::Collision { .. })
                ));
                assert!(run.text.is_empty());
            }
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn quota_error_prints_nothing() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new().with_quota(3);
            let run = up(&mut cloud, &store, ("a.png", b"hello", ""), YES, false);
            assert!(matches!(
                run.result.unwrap_err(),
                CommandError::Upload(UploadError::Cloud(CloudError::QuotaExceeded { .. }))
            ));
            assert!(run.text.is_empty());
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn same_name_warning_is_printed() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            up(&mut cloud, &store, ("a.png", b"one", "F"), YES, false)
                .result
                .unwrap();
            let run = up(&mut cloud, &store, ("a.png", b"two", "F"), DRY, false);
            run.result.unwrap();
            let key = key_of(b"one", "a.png");
            assert!(run.text.contains(&format!(
                "warning: the folder already has a file with the same name but different \
                 content ({key})\n"
            )));
            let run = up(
                &mut cloud,
                &store,
                ("a.png", b"two", "F"),
                json_mode(true),
                false,
            );
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["warnings"].as_array().unwrap().len(), 1);
        }

        #[test]
        fn json_for_a_dry_run() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = up(
                &mut cloud,
                &store,
                ("deck.png", b"hello", "Decks\\Old"),
                RunMode {
                    dry_run: true,
                    yes: false,
                    json: true,
                },
                false,
            );
            run.result.unwrap();
            assert!(run.text.ends_with("}\n"));
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&run.text).unwrap(),
                json!({
                    "name": "deck.png",
                    "cloud_name": key_of(b"hello", "deck.png"),
                    "folder": "Decks/Old",
                    "size": 5,
                    "sha1": sha1_hex(b"hello"),
                    "new_folders": ["Decks", "Decks/Old"],
                    "write": true,
                    "share": true,
                    "add_entry": true,
                    "warnings": [],
                    "dry_run": true,
                    "done": false,
                    "url": null,
                    "backup": null,
                    "manifest": null,
                })
            );
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn json_for_a_real_run_and_a_noop() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = up(
                &mut cloud,
                &store,
                ("a.png", b"hello", ""),
                json_mode(false),
                false,
            );
            run.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            let backups = store.list().unwrap();
            assert_eq!(value["done"], true);
            assert_eq!(value["dry_run"], false);
            assert_eq!(value["folder"], "");
            assert_eq!(value["url"], url_of(&cloud, b"hello", "a.png"));
            assert_eq!(value["backup"], backups[0].name);
            assert_eq!(value["new_folders"], json!([]));

            let run = up(
                &mut cloud,
                &store,
                ("a.png", b"hello", ""),
                json_mode(false),
                false,
            );
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["done"], true);
            assert_eq!(value["write"], false);
            assert_eq!(value["url"], url_of(&cloud, b"hello", "a.png"));
            assert_eq!(value["backup"], serde_json::Value::Null);
        }

        #[test]
        fn json_for_a_declined_upload() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let mode = RunMode {
                dry_run: false,
                yes: false,
                json: true,
            };
            let run = up(&mut cloud, &store, ("a.png", b"x", ""), mode, false);
            run.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["done"], false);
            assert_eq!(value["dry_run"], false);
            assert_eq!(value["url"], serde_json::Value::Null);
            assert!(cloud.mutating_calls().is_empty());
        }

        fn target(dir: &tempfile::TempDir, local_path: Option<&str>) -> Option<ManifestTarget> {
            Some(ManifestTarget {
                file: dir.path().join(Manifest::FILE_NAME),
                local_path: local_path.map(str::to_owned),
            })
        }

        #[test]
        fn upload_records_the_file_in_the_manifest() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = mdir.path().join(Manifest::FILE_NAME);
            let mut cloud = MemoryCloud::new();
            let run = up_with(
                &mut cloud,
                &store,
                ("deck.png", b"hello", "Decks\\Old"),
                YES,
                false,
                target(&mdir, Some("art/deck.png")),
            );
            run.result.unwrap();
            let url = url_of(&cloud, b"hello", "deck.png");
            let lines: Vec<&str> = run.text.lines().collect();
            assert_eq!(lines[lines.len() - 1], url);
            assert_eq!(
                lines[lines.len() - 2],
                format!("Updated manifest {}.", file.display())
            );
            assert_eq!(
                Manifest::load(&file).unwrap().entries,
                [ManifestEntry {
                    path: Some("art/deck.png".into()),
                    cloud_path: "Decks/Old/deck.png".into(),
                    sha1: sha1_hex(b"hello").to_ascii_uppercase(),
                    size: 5,
                    url,
                }]
            );
        }

        #[test]
        fn upload_outside_the_local_dir_and_in_the_root_folder() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = mdir.path().join(Manifest::FILE_NAME);
            let mut cloud = MemoryCloud::new();
            up_with(
                &mut cloud,
                &store,
                ("a.png", b"x", ""),
                YES,
                false,
                target(&mdir, None),
            )
            .result
            .unwrap();
            let manifest = Manifest::load(&file).unwrap();
            assert_eq!(manifest.entries.len(), 1);
            assert_eq!(manifest.entries[0].path, None);
            assert_eq!(manifest.entries[0].cloud_path, "a.png");

            // Uploading it again changes nothing and still reports success.
            let before = std::fs::read(&file).unwrap();
            let run = up_with(
                &mut cloud,
                &store,
                ("a.png", b"x", ""),
                ASK,
                false,
                target(&mdir, None),
            );
            run.result.unwrap();
            assert!(run.text.starts_with("Already uploaded: "));
            assert!(
                run.text
                    .ends_with(&format!("Updated manifest {}.\n", file.display()))
            );
            assert_eq!(std::fs::read(&file).unwrap(), before);
        }

        #[test]
        fn upload_replaces_the_entry_of_the_same_local_path() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = mdir.path().join(Manifest::FILE_NAME);
            let mut cloud = MemoryCloud::new();
            for data in [&b"one"[..], b"two"] {
                up_with(
                    &mut cloud,
                    &store,
                    ("a.png", data, "F"),
                    YES,
                    false,
                    target(&mdir, Some("a.png")),
                )
                .result
                .unwrap();
            }
            let manifest = Manifest::load(&file).unwrap();
            assert_eq!(manifest.entries.len(), 1);
            assert_eq!(manifest.entries[0].size, 3);
            assert_eq!(
                manifest.entries[0].sha1,
                sha1_hex(b"two").to_ascii_uppercase()
            );
        }

        #[test]
        fn dry_run_and_declined_uploads_write_no_manifest() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = mdir.path().join(Manifest::FILE_NAME);
            let mut cloud = MemoryCloud::new();
            for (mode, answer) in [(DRY, false), (ASK, false), (json_mode(true), false)] {
                let run = up_with(
                    &mut cloud,
                    &store,
                    ("a.png", b"x", ""),
                    mode,
                    answer,
                    target(&mdir, Some("a.png")),
                );
                run.result.unwrap();
                assert!(!run.text.contains("Updated manifest"));
            }
            // A noop dry run writes nothing either.
            up(&mut cloud, &store, ("a.png", b"x", ""), YES, false)
                .result
                .unwrap();
            let run = up_with(
                &mut cloud,
                &store,
                ("a.png", b"x", ""),
                json_mode(true),
                false,
                target(&mdir, Some("a.png")),
            );
            run.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert!(value["manifest"].is_null());
            assert!(!file.exists());
        }

        #[test]
        fn json_reports_the_manifest_path() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = mdir.path().join(Manifest::FILE_NAME);
            let mut cloud = MemoryCloud::new();
            let run = up_with(
                &mut cloud,
                &store,
                ("a.png", b"x", ""),
                json_mode(false),
                false,
                target(&mdir, Some("a.png")),
            );
            run.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["manifest"], file.display().to_string());
            assert!(file.exists());
        }

        #[test]
        fn manifest_failure_is_reported_after_the_upload_succeeded() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            // A directory cannot be read or replaced as a manifest file.
            let broken = Some(ManifestTarget {
                file: mdir.path().to_path_buf(),
                local_path: None,
            });
            let mut cloud = MemoryCloud::new();
            let run = up_with(&mut cloud, &store, ("a.png", b"x", ""), YES, false, broken);
            let err = run.result.unwrap_err();
            assert!(matches!(err, CommandError::ManifestUpdate { .. }));
            assert!(
                err.to_string()
                    .starts_with("the upload succeeded, but the manifest ")
            );
            assert_eq!(
                run.text.lines().last(),
                Some(url_of(&cloud, b"x", "a.png").as_str())
            );
            assert!(!run.text.contains("Updated manifest"));
            assert_eq!(CloudIndex::load(&cloud).unwrap().entries.len(), 1);
        }

        #[test]
        fn local_path_in_cases() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("proj");
            let nested = root.join("a").join("b");
            std::fs::create_dir_all(&nested).unwrap();
            let top = root.join("top.png");
            let deep = nested.join("deep.png");
            let outside = dir.path().join("outside.png");
            for f in [&top, &deep, &outside] {
                std::fs::write(f, b"x").unwrap();
            }
            assert_eq!(local_path_in(&root, &top).as_deref(), Some("top.png"));
            assert_eq!(local_path_in(&root, &deep).as_deref(), Some("a/b/deep.png"));
            assert_eq!(local_path_in(&root, &outside), None);
            // Unnormalized spellings resolve to the same file.
            let roundabout = nested.join("..").join("..").join("top.png");
            assert_eq!(
                local_path_in(&root, &roundabout).as_deref(),
                Some("top.png")
            );
            // The directory itself, and files that do not exist.
            assert_eq!(local_path_in(&root, &root), None);
            assert_eq!(local_path_in(&root, &root.join("missing.png")), None);
        }

        #[test]
        fn read_upload_file_names_and_errors() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("deck.png");
            std::fs::write(&file, b"abc").unwrap();
            let (name, data) = read_upload_file(&file, None).unwrap();
            assert_eq!((name.as_str(), data.as_slice()), ("deck.png", &b"abc"[..]));
            let (name, _) = read_upload_file(&file, Some("other.png")).unwrap();
            assert_eq!(name, "other.png");
            let missing = dir.path().join("missing.png");
            let err = read_upload_file(&missing, None).unwrap_err();
            assert!(err.to_string().starts_with("cannot read "));
            assert!(err.to_string().contains("missing.png"));
        }

        // `cloud url`

        fn url_cloud() -> MemoryCloud {
            let mut index = CloudIndex::empty();
            index.folders = vec!["Decks".into(), "Cards".into()];
            index.entries = vec![
                IndexEntry::new(
                    "AA11_a.png",
                    "a.png",
                    Some("https://example.invalid/a".into()),
                    Some(1),
                    None,
                    "Decks",
                ),
                IndexEntry::new("BB22_a.png", "a.png", None, Some(1), None, "Cards"),
                IndexEntry::new("CC33_c.png", "c.png", None, Some(1), None, ""),
            ];
            seed(
                MemoryCloud::new()
                    .with_file("AA11_a.png", "x")
                    .with_file("BB22_a.png", "y")
                    .with_file("CC33_c.png", "z")
                    .with_file("DD44_loose.png", "w"),
                &index,
            )
        }

        fn url_run(
            cloud: &mut MemoryCloud,
            store: &BackupStore,
            query: &str,
            share: bool,
            mode: RunMode,
            answer: bool,
        ) -> Run {
            let mut prompts = Vec::new();
            let mut confirm = |p: &str| {
                prompts.push(p.to_owned());
                Ok(answer)
            };
            let mut out = Vec::new();
            let req = UrlRequest { query, share, mode };
            let result = show_url(cloud, store, &req, UNIX_EPOCH, &mut confirm, &mut out);
            Run {
                result,
                text: String::from_utf8(out).unwrap(),
                prompts,
            }
        }

        #[test]
        fn url_by_path_name_and_key() {
            let (_dir, store) = setup();
            let mut cloud = url_cloud();
            for query in ["Decks/a.png", "decks\\A.PNG", "aa11_a.png"] {
                let run = url_run(&mut cloud, &store, query, false, YES, false);
                run.result.unwrap();
                assert_eq!(run.text, "https://example.invalid/a\n", "{query}");
            }
        }

        #[test]
        fn url_with_share_on_an_entry_with_a_url_changes_nothing() {
            let (_dir, store) = setup();
            let mut cloud = url_cloud();
            cloud.clear_calls();
            let run = url_run(&mut cloud, &store, "Decks/a.png", true, ASK, false);
            run.result.unwrap();
            assert_eq!(run.text, "https://example.invalid/a\n");
            assert!(run.prompts.is_empty());
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn url_json_for_an_existing_url() {
            let (_dir, store) = setup();
            let mut cloud = url_cloud();
            let run = url_run(
                &mut cloud,
                &store,
                "decks/a.png",
                false,
                json_mode(false),
                false,
            );
            run.result.unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&run.text).unwrap(),
                json!({
                    "name": "a.png",
                    "cloud_name": "AA11_a.png",
                    "folder": "Decks",
                    "url": "https://example.invalid/a",
                    "shared": false,
                })
            );
        }

        #[test]
        fn ambiguous_name_lists_both() {
            let (_dir, store) = setup();
            let mut cloud = url_cloud();
            let run = url_run(&mut cloud, &store, "a.png", false, YES, false);
            let err = run.result.unwrap_err();
            assert!(matches!(err, CommandError::Ambiguous { .. }));
            let text = err.to_string();
            assert!(text.contains("\n  Decks/a.png  AA11_a.png"));
            assert!(text.contains("\n  Cards/a.png  BB22_a.png"));
            assert!(text.contains("folder path"));
            assert!(run.text.is_empty());
        }

        #[test]
        fn unknown_and_unindexed_files() {
            let (_dir, store) = setup();
            let mut cloud = url_cloud();
            let err = url_run(&mut cloud, &store, "nope.png", false, YES, false)
                .result
                .unwrap_err();
            assert_eq!(err.to_string(), "no cloud file \"nope.png\"");
            let err = url_run(&mut cloud, &store, "dd44_LOOSE.png", false, YES, false)
                .result
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                "dd44_LOOSE.png is not in the cloud index; use cloud upload"
            );
        }

        #[test]
        fn no_url_without_share_is_an_error() {
            let (_dir, store) = setup();
            let mut cloud = url_cloud();
            cloud.clear_calls();
            let run = url_run(&mut cloud, &store, "Cards/a.png", false, YES, false);
            assert_eq!(
                run.result.unwrap_err().to_string(),
                "Cards/a.png has no URL yet; run again with --share to share it"
            );
            assert!(run.text.is_empty());
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn share_dry_run_and_decline_change_nothing() {
            let (_dir, store) = setup();
            let mut cloud = url_cloud();
            cloud.clear_calls();
            let run = url_run(&mut cloud, &store, "c.png", true, DRY, false);
            run.result.unwrap();
            assert_eq!(
                run.text,
                "Share c.png (CC33_c.png) and record its URL in the index.\n\
                 Dry run: nothing was changed.\n"
            );
            let run = url_run(&mut cloud, &store, "c.png", true, ASK, false);
            run.result.unwrap();
            assert_eq!(run.prompts, ["Share?"]);
            assert!(run.text.ends_with("Cancelled: nothing was changed.\n"));
            assert!(cloud.mutating_calls().is_empty());
            assert!(store.list().unwrap().is_empty());
        }

        #[test]
        fn share_records_the_url_and_backs_up() {
            for (mode, answer) in [(YES, false), (ASK, true)] {
                let (_dir, store) = setup();
                let mut cloud = url_cloud();
                let run = url_run(&mut cloud, &store, "Cards/a.png", true, mode, answer);
                run.result.unwrap();
                let handle = cloud.handle("BB22_a.png").unwrap();
                let url = format!("https://steamusercontent-a.akamaihd.net/ugc/{handle}/BB22/");
                let index = CloudIndex::load(&cloud).unwrap();
                assert_eq!(index.entries[1].url.as_deref(), Some(url.as_str()));
                assert_eq!(
                    index.entries[0].url.as_deref(),
                    Some("https://example.invalid/a")
                );
                let backups = store.list().unwrap();
                assert_eq!(backups.len(), 1);
                let lines: Vec<&str> = run.text.lines().collect();
                assert_eq!(
                    lines[0],
                    "Share Cards/a.png (BB22_a.png) and record its URL in the index."
                );
                assert_eq!(lines[lines.len() - 1], url);
                assert_eq!(
                    lines[lines.len() - 2],
                    format!("Backed up the cloud index to {}.", backups[0].name)
                );
            }
        }

        #[test]
        fn share_json_shapes() {
            let (_dir, store) = setup();
            let mut cloud = url_cloud();
            let dry = RunMode {
                dry_run: true,
                yes: false,
                json: true,
            };
            let run = url_run(&mut cloud, &store, "c.png", true, dry, false);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&run.text).unwrap(),
                json!({
                    "name": "c.png",
                    "cloud_name": "CC33_c.png",
                    "folder": "",
                    "url": null,
                    "shared": false,
                    "dry_run": true,
                    "backup": null,
                })
            );
            let run = url_run(&mut cloud, &store, "c.png", true, json_mode(false), false);
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            let handle = cloud.handle("CC33_c.png").unwrap();
            assert_eq!(
                value["url"],
                format!("https://steamusercontent-a.akamaihd.net/ugc/{handle}/CC33/")
            );
            assert_eq!(value["shared"], true);
            assert_eq!(value["dry_run"], false);
            assert_eq!(value["backup"], store.list().unwrap()[0].name);
        }
    }

    mod delete_tests {
        use std::time::UNIX_EPOCH;

        use super::*;

        fn entry_url(key: &str, name: &str, url: Option<&str>, folder: &str) -> IndexEntry {
            IndexEntry::new(key, name, url.map(Into::into), Some(4), None, folder)
        }

        /// `Decks/Old` holds one file, `Decks/Old/Sub` one whose file is gone;
        /// `Cards` and the root hold files of the same name.
        fn delete_cloud() -> MemoryCloud {
            let mut index = CloudIndex::empty();
            index.folders = vec![
                "Decks".into(),
                "Decks\\Old".into(),
                "Decks\\Old\\Sub".into(),
            ];
            index.entries = vec![
                entry_url("AA_back.png", "back.png", None, "Decks\\Old"),
                entry_url("CC_gone.png", "gone.png", None, "Decks\\Old\\Sub"),
                entry_url("EE_a.png", "a.png", None, "Cards"),
                entry_url("FF_a.png", "a.png", None, ""),
            ];
            seed(
                MemoryCloud::new()
                    .with_file("AA_back.png", "data")
                    .with_file("EE_a.png", "data")
                    .with_file("FF_a.png", "data")
                    .with_file("stray.png", "data"),
                &index,
            )
        }

        fn setup() -> (tempfile::TempDir, BackupStore) {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            (dir, store)
        }

        fn run(
            cloud: &mut MemoryCloud,
            store: &BackupStore,
            target: DeleteTarget<'_>,
            yes: bool,
            json: bool,
        ) -> (Result<(), CommandError>, String) {
            let mut out = Vec::new();
            let result = delete_files(cloud, store, target, yes, json, UNIX_EPOCH, &mut out);
            (result, String::from_utf8(out).unwrap())
        }

        fn file(name: &str) -> DeleteTarget<'_> {
            DeleteTarget::File(name)
        }

        fn folder(name: &str) -> DeleteTarget<'_> {
            DeleteTarget::Folder(name)
        }

        #[test]
        fn dry_run_prints_the_plan_and_changes_nothing() {
            let (_dir, store) = setup();
            let mut cloud = delete_cloud();
            cloud.clear_calls();
            let (result, text) = run(&mut cloud, &store, file("Cards/a.png"), false, false);
            result.unwrap();
            assert_eq!(
                text,
                "Delete Cards/a.png:\n\
                 \x20 delete file    Cards/a.png  (EE_a.png)\n\
                 Dry run: nothing was changed. Run again with --yes to delete.\n"
            );
            assert!(cloud.mutating_calls().is_empty());
            assert!(store.list().unwrap().is_empty());
            assert!(cloud.contents("EE_a.png").is_some());
        }

        #[test]
        fn yes_deletes_the_file_and_entry_and_reports_the_backup() {
            let (_dir, store) = setup();
            let mut cloud = delete_cloud();
            let (result, text) = run(&mut cloud, &store, file("cards/A.PNG"), true, false);
            result.unwrap();
            let backups = store.list().unwrap();
            assert_eq!(backups.len(), 1);
            assert_eq!(
                text,
                format!(
                    "Delete Cards/a.png:\n\
                     \x20 delete file    Cards/a.png  (EE_a.png)\n\
                     Backed up the cloud index to {}.\n\
                     Deleted 1 file.\n",
                    backups[0].name
                )
            );
            assert!(cloud.contents("EE_a.png").is_none());
            assert!(cloud.contents("FF_a.png").is_some());
            assert_eq!(CloudIndex::load(&cloud).unwrap().entries.len(), 3);
        }

        #[test]
        fn folder_mode_text() {
            let (_dir, store) = setup();
            let mut cloud = delete_cloud();
            let (result, text) = run(&mut cloud, &store, folder("decks/old"), false, false);
            result.unwrap();
            assert_eq!(
                text,
                "Delete folder Decks/Old (2 files, 2 folders):\n\
                 \x20 delete file    Decks/Old/back.png  (AA_back.png)\n\
                 \x20 remove entry   Decks/Old/Sub/gone.png  (CC_gone.png, file already missing)\n\
                 \x20 remove folder  Decks/Old/Sub\n\
                 \x20 remove folder  Decks/Old\n\
                 Dry run: nothing was changed. Run again with --yes to delete.\n"
            );
            let (result, text) = run(&mut cloud, &store, folder("decks/old"), true, false);
            result.unwrap();
            assert!(text.ends_with("Deleted 1 file.\n"));
            let index = CloudIndex::load(&cloud).unwrap();
            assert_eq!(index.folders, ["Decks"]);
            assert_eq!(index.entries.len(), 2);
        }

        #[test]
        fn unindexed_file() {
            let (_dir, store) = setup();
            let mut cloud = delete_cloud();
            let (result, text) = run(&mut cloud, &store, file("STRAY.png"), true, false);
            result.unwrap();
            assert!(
                text.starts_with("Delete stray.png:\n  delete file    stray.png  (not in index)\n")
            );
            assert!(cloud.contents("stray.png").is_none());
            assert_eq!(store.list().unwrap().len(), 1);
        }

        #[test]
        fn url_warning() {
            let (_dir, store) = setup();
            let mut cloud = delete_cloud();
            let mut index = CloudIndex::load(&cloud).unwrap();
            index.entries[0].url = Some("https://u/1".into());
            index.entries[1].url = Some("https://u/2".into());
            index.entries[2].url = Some("https://u/3".into());
            cloud = seed(MemoryCloud::new().with_file("EE_a.png", "x"), &index);
            let (result, text) = run(&mut cloud, &store, folder("Decks"), false, false);
            result.unwrap();
            assert!(text.contains(
                "warning: 2 files have a download URL; saves that use it will no longer load them\n"
            ));
            let (result, text) = run(&mut cloud, &store, file("EE_a.png"), false, false);
            result.unwrap();
            assert!(text.contains(
                "warning: 1 file has a download URL; saves that use it will no longer load it\n"
            ));
        }

        #[test]
        fn json_for_dry_run_and_yes() {
            let (_dir, store) = setup();
            let mut cloud = delete_cloud();
            let (result, text) = run(&mut cloud, &store, folder("Decks/Old"), false, true);
            result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["target"], "folder");
            assert_eq!(value["folder"], "Decks/Old");
            assert_eq!(value["dry_run"], true);
            assert_eq!(value["done"], false);
            assert_eq!(value["deleted"], json!([]));
            assert_eq!(value["backup"], serde_json::Value::Null);
            assert_eq!(value["folders"], json!(["Decks/Old/Sub", "Decks/Old"]));
            assert_eq!(
                value["files"][1],
                json!({
                    "name": "gone.png",
                    "cloud_name": "CC_gone.png",
                    "folder": "Decks/Old/Sub",
                    "size": 4,
                    "url": null,
                    "indexed": true,
                    "in_cloud": false,
                })
            );
            assert!(store.list().unwrap().is_empty());

            let (result, text) = run(&mut cloud, &store, file("Cards/a.png"), true, true);
            result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["target"], "file");
            assert_eq!(value["folder"], serde_json::Value::Null);
            assert_eq!(value["dry_run"], false);
            assert_eq!(value["done"], true);
            assert_eq!(value["deleted"], json!(["EE_a.png"]));
            assert_eq!(value["backup"], store.list().unwrap()[0].name);
        }

        #[test]
        fn errors_use_the_cli_messages() {
            let (_dir, store) = setup();
            let mut cloud = delete_cloud();
            cloud.clear_calls();
            let (result, text) = run(&mut cloud, &store, file("a.png"), true, false);
            let err = result.unwrap_err();
            assert!(matches!(err, CommandError::Ambiguous { .. }));
            let msg = err.to_string();
            assert!(msg.contains("\n  Cards/a.png  EE_a.png"));
            assert!(msg.contains("\n  a.png  FF_a.png"));
            assert!(msg.contains("folder path"));
            assert!(text.is_empty());

            let err = run(&mut cloud, &store, file("nope.png"), true, false)
                .0
                .unwrap_err();
            assert_eq!(err.to_string(), "no cloud file \"nope.png\"");
            let err = run(&mut cloud, &store, folder("Nope"), true, false)
                .0
                .unwrap_err();
            assert_eq!(err.to_string(), "no cloud folder \"Nope\"");
            let err = run(&mut cloud, &store, folder("/"), true, false)
                .0
                .unwrap_err();
            assert_eq!(err.to_string(), "the root folder cannot be deleted");
            let err = run(&mut cloud, &store, file("CloudInfo.bson"), true, false)
                .0
                .unwrap_err();
            assert!(err.to_string().contains("never deleted"));
            assert!(cloud.mutating_calls().is_empty());
            assert!(store.list().unwrap().is_empty());
        }
    }

    mod status_tests {
        use ttskit_cloud::sha1_hex;

        use super::*;

        fn local(path: &str, data: &str) -> LocalFile {
            LocalFile {
                path: path.to_owned(),
                size: data.len() as u64,
                sha1: sha1_hex(data.as_bytes()),
            }
        }

        fn key(data: &str, name: &str) -> String {
            format!("{}_{name}", sha1_hex(data.as_bytes()))
        }

        /// Cloud folder `Decks` with an identical, a changed, a missing and a
        /// broken file; one more file lives outside it.
        fn status_cloud() -> MemoryCloud {
            let mut index = CloudIndex::empty();
            index.folders = vec!["Decks".into(), "Decks\\Sub".into()];
            let mk = |data: &str, name: &str, folder: &str| {
                IndexEntry::new(
                    key(data, name),
                    name,
                    Some(format!("https://u/{name}")),
                    Some(1),
                    None,
                    folder,
                )
            };
            index.entries = vec![
                mk("same", "same.png", "Decks"),
                mk("old", "changed.png", "Decks"),
                mk("gone", "old.png", "Decks"),
                mk("brk", "b.png", "Decks\\Sub"),
                mk("other", "other.png", "Cards"),
            ];
            seed(
                MemoryCloud::new()
                    .with_file(&key("same", "same.png"), "x")
                    .with_file(&key("old", "changed.png"), "x")
                    .with_file(&key("gone", "old.png"), "x")
                    .with_file(&key("other", "other.png"), "x"),
                &index,
            )
        }

        fn run(
            cloud: &MemoryCloud,
            files: &[LocalFile],
            folder: &str,
            json: bool,
        ) -> (bool, String) {
            let mut out = Vec::new();
            let in_sync = status(cloud, files, folder, "assets", json, &mut out).unwrap();
            (in_sync, String::from_utf8(out).unwrap())
        }

        #[test]
        fn mixed_case_text() {
            let cloud = status_cloud();
            let files = [
                local("same.png", "same"),
                local("changed.png", "new"),
                local("fresh.png", "f"),
                local("sub/b.png", "brk"),
                local("sub/x.png", "x"),
            ];
            let (in_sync, text) = run(&cloud, &files, "decks", false);
            assert!(!in_sync);
            assert_eq!(
                text,
                "Compare assets with cloud folder Decks:\n\
                 \x20 changed    changed.png\n\
                 \x20 new        fresh.png\n\
                 \x20 missing    old.png\n\
                 \x20 broken     sub/b.png  (cloud file is gone)\n\
                 \x20 new        sub/x.png\n\
                 2 new, 1 changed, 1 missing, 1 broken, 1 identical\n"
            );
        }

        #[test]
        fn in_sync_text() {
            let files = [local("same.png", "same"), local("old.png", "gone")];
            let mut index = CloudIndex::empty();
            index.entries = vec![
                IndexEntry::new(
                    key("same", "same.png"),
                    "same.png",
                    None,
                    None,
                    None,
                    "Decks",
                ),
                IndexEntry::new(key("gone", "old.png"), "old.png", None, None, None, "Decks"),
            ];
            let cloud = seed(
                MemoryCloud::new()
                    .with_file(&key("same", "same.png"), "x")
                    .with_file(&key("gone", "old.png"), "x"),
                &index,
            );
            let (in_sync, text) = run(&cloud, &files, "Decks", false);
            assert!(in_sync);
            assert_eq!(
                text,
                "Compare assets with cloud folder Decks:\nIn sync: 2 files identical.\n"
            );
        }

        #[test]
        fn json_fields() {
            let cloud = status_cloud();
            let files = [local("same.png", "same"), local("fresh.png", "f")];
            let (in_sync, text) = run(&cloud, &files, "Decks", true);
            assert!(!in_sync);
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["dir"], "assets");
            assert_eq!(v["folder"], "Decks");
            assert_eq!(v["folder_exists"], true);
            assert_eq!(v["in_sync"], false);
            assert_eq!(
                v["counts"],
                json!({"new": 1, "changed": 0, "missing": 3, "broken": 0, "identical": 1})
            );
            let rows = v["files"].as_array().unwrap();
            assert_eq!(rows.len(), 5);
            let same = rows.iter().find(|r| r["path"] == "same.png").unwrap();
            assert_eq!(same["state"], "identical");
            assert_eq!(same["local_size"], 4);
            assert_eq!(same["local_sha1"], sha1_hex(b"same"));
            assert_eq!(same["cloud_name"], key("same", "same.png"));
            assert_eq!(same["url"], "https://u/same.png");
            let fresh = rows.iter().find(|r| r["path"] == "fresh.png").unwrap();
            assert_eq!(fresh["state"], "new");
            assert!(fresh["cloud_name"].is_null() && fresh["url"].is_null());
            let missing = rows.iter().find(|r| r["path"] == "old.png").unwrap();
            assert!(missing["local_size"].is_null() && missing["local_sha1"].is_null());
        }

        #[test]
        fn nonexistent_folder_and_root_headers() {
            let cloud = status_cloud();
            let files = [local("a.png", "a")];
            let (in_sync, text) = run(&cloud, &files, "No/Such", false);
            assert!(!in_sync);
            assert_eq!(
                text,
                "Compare assets with cloud folder No/Such (does not exist yet):\n\
                 \x20 new        a.png\n\
                 1 new\n"
            );
            let (_, text) = run(&cloud, &[], "", false);
            assert!(text.starts_with("Compare assets with the root folder:\n"));
        }
    }

    mod sync_push_tests {
        use std::time::UNIX_EPOCH;

        use ttskit_cloud::{Op, cloud_key, sha1_hex};

        use super::*;

        const DATE: &str = "10/10/2026 14:05:08";
        const YES: RunMode = RunMode {
            dry_run: false,
            yes: true,
            json: false,
        };
        const DRY: RunMode = RunMode {
            dry_run: true,
            yes: false,
            json: false,
        };
        const ASK: RunMode = RunMode {
            dry_run: false,
            yes: false,
            json: false,
        };

        struct Out {
            result: Result<(), CommandError>,
            text: String,
            prompts: Vec<String>,
        }

        fn locals(files: &[(&str, &str)]) -> Vec<LocalFile> {
            files
                .iter()
                .map(|(path, data)| LocalFile {
                    path: (*path).to_owned(),
                    size: data.len() as u64,
                    sha1: sha1_hex(data.as_bytes()),
                })
                .collect()
        }

        /// A cloud with `files` (name, content) in the folder `Game`, indexed.
        fn cloud_with(files: &[(&str, &str)]) -> MemoryCloud {
            let mut index = CloudIndex::empty();
            index.folders = vec!["Game".into()];
            let mut cloud = MemoryCloud::new();
            for (name, data) in files {
                let key = cloud_key(&sha1_hex(data.as_bytes()), name);
                cloud = cloud.with_file(&key, data.as_bytes());
                index.entries.push(IndexEntry::new(
                    key.clone(),
                    *name,
                    Some(format!("https://u/{key}")),
                    Some(data.len() as u64),
                    None,
                    "Game",
                ));
            }
            seed(cloud, &index)
        }

        fn push_run(
            cloud: &mut MemoryCloud,
            store: &BackupStore,
            files: &[(&str, &str)],
            prune: bool,
            mode: RunMode,
            answer: bool,
        ) -> Out {
            push_run_with(cloud, store, files, prune, mode, answer, None)
        }

        fn push_run_with(
            cloud: &mut MemoryCloud,
            store: &BackupStore,
            files: &[(&str, &str)],
            prune: bool,
            mode: RunMode,
            answer: bool,
            manifest: Option<&Path>,
        ) -> Out {
            let data: HashMap<String, Vec<u8>> = files
                .iter()
                .map(|(p, d)| ((*p).to_owned(), d.as_bytes().to_vec()))
                .collect();
            let read = |p: &str| {
                data.get(p)
                    .cloned()
                    .ok_or_else(|| io::Error::other("no such file"))
            };
            let mut prompts = Vec::new();
            let mut confirm = |p: &str| {
                prompts.push(p.to_owned());
                Ok(answer)
            };
            let mut out = Vec::new();
            let result = sync_push(
                cloud,
                store,
                &locals(files),
                &read,
                "Game",
                prune,
                manifest,
                mode,
                UNIX_EPOCH,
                DATE,
                &mut confirm,
                &mut out,
            );
            Out {
                result,
                text: String::from_utf8(out).unwrap(),
                prompts,
            }
        }

        fn setup() -> (tempfile::TempDir, BackupStore) {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            (dir, store)
        }

        fn json_mode(dry_run: bool) -> RunMode {
            RunMode {
                dry_run,
                yes: !dry_run,
                json: true,
            }
        }

        const NEW: [(&str, &str); 2] = [("a.png", "A"), ("b.png", "B")];

        #[test]
        fn dry_run_prints_the_plan_and_changes_nothing() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = push_run(&mut cloud, &store, &NEW, false, DRY, false);
            run.result.unwrap();
            assert_eq!(
                run.text,
                "Push to cloud folder Game:\n\
                 \x20 upload     a.png  (new)\n\
                 \x20 upload     b.png  (new)\n\
                 2 upload\n\
                 Dry run: nothing was changed.\n"
            );
            assert!(run.prompts.is_empty());
            assert!(cloud.mutating_calls().is_empty());
            assert!(store.list().unwrap().is_empty());
        }

        #[test]
        fn declined_confirmation_changes_nothing() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = push_run(&mut cloud, &store, &NEW, false, ASK, false);
            run.result.unwrap();
            assert_eq!(run.prompts, ["Push?"]);
            assert!(run.text.starts_with("Push to cloud folder Game:\n"));
            assert!(run.text.ends_with("Cancelled: nothing was changed.\n"));
            assert!(cloud.mutating_calls().is_empty());
            assert!(store.list().unwrap().is_empty());
        }

        #[test]
        fn accepted_confirmation_pushes() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = push_run(&mut cloud, &store, &NEW, false, ASK, true);
            run.result.unwrap();
            assert_eq!(run.prompts, ["Push?"]);
            let backups = store.list().unwrap();
            assert_eq!(backups.len(), 1);
            assert!(
                run.text
                    .contains("\n  uploaded   a.png\n  uploaded   b.png\n")
            );
            assert!(run.text.ends_with(&format!(
                "Backed up the cloud index to {}.\nUploaded 2 files, deleted 0 files.\n",
                backups[0].name
            )));
            assert!(
                cloud
                    .contents(&cloud_key(&sha1_hex(b"A"), "a.png"))
                    .is_some()
            );
            let index = CloudIndex::load(&cloud).unwrap();
            assert_eq!(index.entries.len(), 2);
        }

        #[test]
        fn yes_pushes_without_asking() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = push_run(&mut cloud, &store, &NEW[..1], false, YES, false);
            run.result.unwrap();
            assert!(run.prompts.is_empty());
            assert!(run.text.ends_with("Uploaded 1 file, deleted 0 files.\n"));
            assert_eq!(CloudIndex::load(&cloud).unwrap().entries.len(), 1);
        }

        #[test]
        fn confirmation_error_propagates_after_the_plan() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let mut confirm = |_: &str| Err(CommandError::ConfirmationNeeded);
            let mut out = Vec::new();
            let err = sync_push(
                &mut cloud,
                &store,
                &locals(&NEW),
                &|_| Ok(Vec::new()),
                "Game",
                false,
                None,
                ASK,
                UNIX_EPOCH,
                DATE,
                &mut confirm,
                &mut out,
            )
            .unwrap_err();
            assert!(matches!(err, CommandError::ConfirmationNeeded));
            assert!(
                String::from_utf8(out)
                    .unwrap()
                    .starts_with("Push to cloud folder Game:\n")
            );
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn noop_never_asks() {
            let (_dir, store) = setup();
            let mut cloud = cloud_with(&[("a.png", "A")]);
            let run = push_run(&mut cloud, &store, &NEW[..1], false, ASK, false);
            run.result.unwrap();
            assert!(run.prompts.is_empty());
            assert_eq!(run.text, "Push to cloud folder Game:\n1 identical\n");
            assert!(cloud.mutating_calls().is_empty());
            assert!(store.list().unwrap().is_empty());
        }

        #[test]
        fn conflict_is_an_error_after_the_plan_even_for_dry_run() {
            for mode in [ASK, DRY, YES] {
                let (_dir, store) = setup();
                let mut cloud = MemoryCloud::new();
                let files = [("a.png", "1"), ("A.png", "2")];
                let run = push_run(&mut cloud, &store, &files, false, mode, true);
                let err = run.result.unwrap_err();
                assert!(matches!(err, CommandError::PushConflicts(2)));
                assert_eq!(
                    err.to_string(),
                    "cannot push: 2 conflict(s) in the plan; rename the files involved"
                );
                assert!(run.text.contains("conflict"));
                assert!(run.prompts.is_empty());
                assert!(!run.text.contains("Dry run"));
                assert!(cloud.mutating_calls().is_empty());
                assert!(store.list().unwrap().is_empty());
            }
        }

        #[test]
        fn conflict_json_is_printed_before_the_error() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let files = [("a.png", "1"), ("A.png", "2")];
            let run = push_run(&mut cloud, &store, &files, false, json_mode(true), false);
            assert!(matches!(
                run.result.unwrap_err(),
                CommandError::PushConflicts(2)
            ));
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["done"], false);
            assert!(value["plan"].is_object());
        }

        #[test]
        fn needs_prune_is_noted_and_the_file_is_not_pushed() {
            let (_dir, store) = setup();
            let mut cloud = cloud_with(&[("a.png", "old")]);
            let files = [("a.png", "new")];

            let run = push_run(&mut cloud, &store, &files, false, ASK, true);
            run.result.unwrap();
            assert!(run.prompts.is_empty());
            assert!(run.text.ends_with(
                "1 changed file was not pushed; run again with --prune to replace it.\n"
            ));
            assert!(cloud.mutating_calls().is_empty());

            // Together with a new file: the new one is pushed, the note stays.
            let files = [("a.png", "new"), ("b.png", "B")];
            let run = push_run(&mut cloud, &store, &files, false, YES, false);
            run.result.unwrap();
            assert!(run.text.contains("  uploaded   b.png\n"));
            assert!(!run.text.contains("uploaded   a.png"));
            assert!(run.text.ends_with(
                "1 changed file was not pushed; run again with --prune to replace it.\n"
            ));
            let index = CloudIndex::load(&cloud).unwrap();
            assert!(
                index
                    .entries
                    .iter()
                    .any(|e| e.name == "a.png" && e.key == cloud_key(&sha1_hex(b"old"), "a.png"))
            );

            // Also after a dry run.
            let mut cloud = cloud_with(&[("a.png", "old"), ("c.png", "C")]);
            let files = [("a.png", "new"), ("c.png", "CC"), ("d.png", "D")];
            let run = push_run(&mut cloud, &store, &files, false, DRY, false);
            run.result.unwrap();
            assert!(run.text.ends_with(
                "Dry run: nothing was changed.\n\
                 2 changed files were not pushed; run again with --prune to replace them.\n"
            ));
        }

        #[test]
        fn prune_deletes_and_says_so() {
            let (_dir, store) = setup();
            let mut cloud = cloud_with(&[("old.png", "O"), ("a.png", "A")]);
            let files = [("a.png", "A"), ("b.png", "B")];

            let run = push_run(&mut cloud, &store, &files, false, YES, false);
            run.result.unwrap();
            assert!(!run.text.contains("not pushed"));
            assert!(run.text.ends_with("Uploaded 1 file, deleted 0 files.\n"));
            assert!(
                cloud
                    .contents(&cloud_key(&sha1_hex(b"O"), "old.png"))
                    .is_some()
            );

            let run = push_run(&mut cloud, &store, &files, true, YES, false);
            run.result.unwrap();
            assert!(run.text.contains("(prune)"));
            assert!(run.text.contains("  deleted    old.png\n"));
            assert!(run.text.ends_with("Uploaded 0 files, deleted 1 file.\n"));
            assert!(
                cloud
                    .contents(&cloud_key(&sha1_hex(b"O"), "old.png"))
                    .is_none()
            );
            let index = CloudIndex::load(&cloud).unwrap();
            assert!(!index.entries.iter().any(|e| e.name == "old.png"));
        }

        #[test]
        fn json_dry_run_shape() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            let run = push_run(&mut cloud, &store, &NEW, false, json_mode(true), false);
            run.result.unwrap();
            assert!(run.text.starts_with("{\n"));
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["dry_run"], true);
            assert_eq!(value["done"], false);
            assert_eq!(value["uploaded"], json!([]));
            assert_eq!(value["deleted"], json!([]));
            assert!(value["backup"].is_null());
            assert_eq!(value["plan"]["steps"].as_array().unwrap().len(), 2);
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn json_real_run_shape() {
            let (_dir, store) = setup();
            let mut cloud = cloud_with(&[("old.png", "O")]);
            let run = push_run(&mut cloud, &store, &NEW, true, json_mode(false), false);
            run.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["dry_run"], false);
            assert_eq!(value["done"], true);
            assert_eq!(value["uploaded"], json!(["a.png", "b.png"]));
            assert_eq!(
                value["deleted"],
                json!([cloud_key(&sha1_hex(b"O"), "old.png")])
            );
            assert_eq!(value["backup"], store.list().unwrap()[0].name);
            assert!(value["plan"].is_object());
        }

        #[test]
        fn failing_step_reports_incomplete_and_rerun_finishes() {
            let (_dir, store) = setup();
            let mut cloud = MemoryCloud::new();
            cloud.fail_once(Op::Share, None, CloudError::Backend("boom".into()));
            let files = &NEW[..1];
            let run = push_run(&mut cloud, &store, files, false, YES, false);
            let message = run.result.unwrap_err().to_string();
            assert!(message.contains("run the same push again"), "{message}");
            assert!(!run.text.contains("Uploaded"));

            let run = push_run(&mut cloud, &store, files, false, YES, false);
            run.result.unwrap();
            assert!(run.text.ends_with("Uploaded 1 file, deleted 0 files.\n"));
            let index = CloudIndex::load(&cloud).unwrap();
            assert_eq!(index.entries.len(), 1);
            assert!(index.entries[0].url.is_some());
        }

        fn manifest_path(dir: &tempfile::TempDir) -> PathBuf {
            dir.path().join(Manifest::FILE_NAME)
        }

        fn paths(manifest: &Manifest) -> Vec<(Option<&str>, &str)> {
            manifest
                .entries
                .iter()
                .map(|e| (e.path.as_deref(), e.cloud_path.as_str()))
                .collect()
        }

        #[test]
        fn push_writes_a_sorted_manifest() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = manifest_path(&mdir);
            let mut cloud = MemoryCloud::new();
            let files = [("b.png", "B"), ("a.png", "A")];
            let run = push_run_with(&mut cloud, &store, &files, false, YES, false, Some(&file));
            run.result.unwrap();
            assert!(run.text.ends_with(&format!(
                "deleted 0 files.\nUpdated manifest {}.\n",
                file.display()
            )));
            let manifest = Manifest::load(&file).unwrap();
            assert_eq!(
                paths(&manifest),
                [(Some("a.png"), "Game/a.png"), (Some("b.png"), "Game/b.png")]
            );
            let a = &manifest.entries[0];
            assert_eq!(a.sha1, sha1_hex(b"A").to_ascii_uppercase());
            assert_eq!(a.size, 1);
            let index = CloudIndex::load(&cloud).unwrap();
            let indexed = index.entries.iter().find(|e| e.name == "a.png").unwrap();
            assert_eq!(Some(&a.url), indexed.url.as_ref());
            let text = std::fs::read_to_string(&file).unwrap();
            assert_eq!(text, manifest.to_json());
            assert!(text.find("Game/a.png").unwrap() < text.find("Game/b.png").unwrap());
        }

        #[test]
        fn repeated_push_keeps_the_manifest_byte_identical() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = manifest_path(&mdir);
            let mut cloud = MemoryCloud::new();
            push_run_with(&mut cloud, &store, &NEW, false, YES, false, Some(&file))
                .result
                .unwrap();
            let first = std::fs::read(&file).unwrap();
            cloud.clear_calls();
            let run = push_run_with(&mut cloud, &store, &NEW, false, ASK, false, Some(&file));
            run.result.unwrap();
            assert!(cloud.mutating_calls().is_empty());
            assert_eq!(
                run.text,
                format!(
                    "Push to cloud folder Game:\n2 identical\nUpdated manifest {}.\n",
                    file.display()
                )
            );
            assert_eq!(std::fs::read(&file).unwrap(), first);
        }

        #[test]
        fn noop_push_creates_a_missing_manifest() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = manifest_path(&mdir);
            let mut cloud = cloud_with(&[("a.png", "A")]);
            push_run_with(
                &mut cloud,
                &store,
                &NEW[..1],
                false,
                YES,
                false,
                Some(&file),
            )
            .result
            .unwrap();
            assert_eq!(
                paths(&Manifest::load(&file).unwrap()),
                [(Some("a.png"), "Game/a.png")]
            );
        }

        #[test]
        fn dry_run_conflicts_decline_and_failure_write_no_manifest() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = manifest_path(&mdir);

            // Dry run, also when the plan is a noop.
            for mut cloud in [MemoryCloud::new(), cloud_with(&[("a.png", "A")])] {
                let files = &NEW[..1];
                let run = push_run_with(&mut cloud, &store, files, false, DRY, false, Some(&file));
                run.result.unwrap();
                assert!(!run.text.contains("manifest"));
                let run = push_run_with(
                    &mut cloud,
                    &store,
                    files,
                    false,
                    json_mode(true),
                    false,
                    Some(&file),
                );
                run.result.unwrap();
                let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
                assert!(value["manifest"].is_null());
            }

            // Declined.
            let mut cloud = MemoryCloud::new();
            push_run_with(&mut cloud, &store, &NEW, false, ASK, false, Some(&file))
                .result
                .unwrap();

            // Conflicts.
            let conflicting = [("a.png", "1"), ("A.png", "2")];
            for mode in [DRY, YES, json_mode(false)] {
                let mut cloud = MemoryCloud::new();
                let run = push_run_with(
                    &mut cloud,
                    &store,
                    &conflicting,
                    false,
                    mode,
                    true,
                    Some(&file),
                );
                assert!(matches!(
                    run.result.unwrap_err(),
                    CommandError::PushConflicts(2)
                ));
                assert!(!run.text.contains("Updated manifest"));
            }

            // An incomplete push.
            let mut cloud = MemoryCloud::new();
            cloud.fail_once(Op::Share, None, CloudError::Backend("boom".into()));
            let run = push_run_with(
                &mut cloud,
                &store,
                &NEW[..1],
                false,
                YES,
                false,
                Some(&file),
            );
            assert!(run.result.is_err());
            assert!(!run.text.contains("manifest"));

            assert!(!file.exists());
        }

        #[test]
        fn json_reports_the_manifest_path() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = manifest_path(&mdir);
            let mut cloud = MemoryCloud::new();
            let run = push_run_with(
                &mut cloud,
                &store,
                &NEW,
                false,
                json_mode(false),
                false,
                Some(&file),
            );
            run.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["manifest"], file.display().to_string());
            assert!(file.exists());
        }

        #[test]
        fn changed_file_without_prune_is_not_linked_to_the_old_version() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = manifest_path(&mdir);
            let mut cloud = cloud_with(&[("a.png", "old")]);
            let old = [("a.png", "old")];
            push_run_with(&mut cloud, &store, &old, false, YES, false, Some(&file))
                .result
                .unwrap();
            assert_eq!(
                paths(&Manifest::load(&file).unwrap()),
                [(Some("a.png"), "Game/a.png")]
            );

            let new = [("a.png", "new"), ("b.png", "B")];
            let run = push_run_with(&mut cloud, &store, &new, false, YES, false, Some(&file));
            run.result.unwrap();
            assert!(run.text.contains("not pushed"));
            assert_eq!(
                paths(&Manifest::load(&file).unwrap()),
                [(Some("b.png"), "Game/b.png")]
            );

            // With prune the new version replaces the old one.
            push_run_with(&mut cloud, &store, &new, true, YES, false, Some(&file))
                .result
                .unwrap();
            let manifest = Manifest::load(&file).unwrap();
            assert_eq!(
                paths(&manifest),
                [(Some("a.png"), "Game/a.png"), (Some("b.png"), "Game/b.png")]
            );
            assert_eq!(
                manifest.entries[0].sha1,
                sha1_hex(b"new").to_ascii_uppercase()
            );
        }

        #[test]
        fn pruned_files_leave_the_manifest_and_pathless_entries_stay() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            let file = manifest_path(&mdir);
            let keep = ManifestEntry {
                path: None,
                cloud_path: "Other/k.png".into(),
                sha1: sha1_hex(b"k").to_ascii_uppercase(),
                size: 1,
                url: "https://u/k".into(),
            };
            Manifest {
                entries: vec![keep.clone()],
            }
            .save(&file)
            .unwrap();

            let mut cloud = MemoryCloud::new();
            let both = [("a.png", "A"), ("old.png", "O")];
            push_run_with(&mut cloud, &store, &both, false, YES, false, Some(&file))
                .result
                .unwrap();
            assert_eq!(
                paths(&Manifest::load(&file).unwrap()),
                [
                    (Some("a.png"), "Game/a.png"),
                    (Some("old.png"), "Game/old.png"),
                    (None, "Other/k.png"),
                ]
            );

            let only = [("a.png", "A")];
            let run = push_run_with(&mut cloud, &store, &only, true, YES, false, Some(&file));
            run.result.unwrap();
            assert!(run.text.contains("deleted    old.png"));
            let manifest = Manifest::load(&file).unwrap();
            assert_eq!(
                paths(&manifest),
                [(Some("a.png"), "Game/a.png"), (None, "Other/k.png")]
            );
            assert_eq!(manifest.entries[1], keep);
        }

        #[test]
        fn manifest_failure_is_reported_after_the_push_succeeded() {
            let (_dir, store) = setup();
            let mdir = tempfile::tempdir().unwrap();
            // A directory cannot be read or replaced as a manifest file.
            let file = mdir.path().to_path_buf();
            let mut cloud = MemoryCloud::new();
            let run = push_run_with(
                &mut cloud,
                &store,
                &NEW[..1],
                false,
                YES,
                false,
                Some(&file),
            );
            let err = run.result.unwrap_err();
            assert!(matches!(err, CommandError::ManifestUpdate { .. }));
            assert!(
                err.to_string()
                    .starts_with("the push succeeded, but the manifest ")
            );
            assert!(run.text.ends_with("Uploaded 1 file, deleted 0 files.\n"));
            assert!(!run.text.contains("Updated manifest"));
            assert_eq!(CloudIndex::load(&cloud).unwrap().entries.len(), 1);

            let run = push_run_with(
                &mut cloud,
                &store,
                &NEW[..1],
                false,
                json_mode(false),
                false,
                Some(&file),
            );
            assert!(run.result.is_err());
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert!(value["manifest"].is_null());
            assert_eq!(value["done"], true);
        }
    }

    mod sync_pull_tests {
        use std::collections::BTreeMap;

        use ttskit_cloud::{cloud_key, sha1_hex};

        use super::*;

        const YES: RunMode = RunMode {
            dry_run: false,
            yes: true,
            json: false,
        };
        const DRY: RunMode = RunMode {
            dry_run: true,
            yes: false,
            json: false,
        };
        const ASK: RunMode = RunMode {
            dry_run: false,
            yes: false,
            json: false,
        };
        const JSON_YES: RunMode = RunMode {
            dry_run: false,
            yes: true,
            json: true,
        };
        const JSON_DRY: RunMode = RunMode {
            dry_run: true,
            yes: false,
            json: true,
        };

        /// An in-memory directory with a log of what changed it.
        #[derive(Default)]
        struct MemoryDir {
            files: BTreeMap<String, Vec<u8>>,
            log: Vec<String>,
        }

        impl MemoryDir {
            fn with(files: &[(&str, &str)]) -> Self {
                Self {
                    files: files
                        .iter()
                        .map(|(p, d)| ((*p).to_owned(), d.as_bytes().to_vec()))
                        .collect(),
                    log: Vec::new(),
                }
            }

            fn local(&self) -> Vec<LocalFile> {
                self.files
                    .iter()
                    .map(|(p, d)| LocalFile {
                        path: p.clone(),
                        size: d.len() as u64,
                        sha1: sha1_hex(d),
                    })
                    .collect()
            }

            fn names(&self) -> Vec<&str> {
                self.files.keys().map(String::as_str).collect()
            }
        }

        impl LocalDir for MemoryDir {
            fn read(&self, path: &str) -> io::Result<Vec<u8>> {
                self.files
                    .get(path)
                    .cloned()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
            }

            fn write(&mut self, path: &str, data: &[u8]) -> io::Result<()> {
                self.log.push(format!("write {path}"));
                self.files.insert(path.to_owned(), data.to_vec());
                Ok(())
            }

            fn trash(&mut self, path: &str) -> io::Result<()> {
                self.log.push(format!("trash {path}"));
                self.files
                    .remove(path)
                    .map(|_| ())
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
            }
        }

        struct Out {
            result: Result<(), CommandError>,
            text: String,
            prompts: Vec<String>,
        }

        /// A cloud with `files` (name, content) in the folder `Game`, indexed.
        fn cloud_with(files: &[(&str, &str)]) -> MemoryCloud {
            let mut index = CloudIndex::empty();
            index.folders = vec!["Game".into()];
            let mut cloud = MemoryCloud::new();
            for (name, data) in files {
                let key = cloud_key(&sha1_hex(data.as_bytes()), name);
                cloud = cloud.with_file(&key, data.as_bytes());
                index.entries.push(IndexEntry::new(
                    key.clone(),
                    *name,
                    Some(format!("https://u/{key}")),
                    Some(data.len() as u64),
                    None,
                    "Game",
                ));
            }
            seed(cloud, &index)
        }

        fn pull_run(
            cloud: &MemoryCloud,
            dir: &mut MemoryDir,
            prune: bool,
            mode: RunMode,
            answer: bool,
        ) -> Out {
            let local = dir.local();
            let mut prompts = Vec::new();
            let mut confirm = |p: &str| {
                prompts.push(p.to_owned());
                Ok(answer)
            };
            let mut out = Vec::new();
            let result = sync_pull(
                cloud,
                dir,
                &local,
                "Game",
                prune,
                mode,
                &mut confirm,
                &mut out,
            );
            Out {
                result,
                text: String::from_utf8(out).unwrap(),
                prompts,
            }
        }

        #[test]
        fn dry_run_prints_the_plan_and_changes_nothing() {
            let cloud = cloud_with(&[("a.png", "A"), ("b.png", "B")]);
            let mut dir = MemoryDir::default();
            let run = pull_run(&cloud, &mut dir, false, DRY, false);
            run.result.unwrap();
            assert!(run.text.starts_with("Pull from cloud folder Game:\n"));
            assert!(run.text.contains("  download   a.png  (new)\n"));
            assert!(run.text.contains("  download   b.png  (new)\n"));
            assert!(
                run.text
                    .ends_with("2 download\nDry run: nothing was changed.\n")
            );
            assert!(run.prompts.is_empty());
            assert!(dir.log.is_empty());
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn declined_confirmation_changes_nothing() {
            let cloud = cloud_with(&[("a.png", "A")]);
            let mut dir = MemoryDir::default();
            let run = pull_run(&cloud, &mut dir, false, ASK, false);
            run.result.unwrap();
            assert_eq!(run.prompts, ["Pull?"]);
            assert!(run.text.starts_with("Pull from cloud folder Game:\n"));
            assert!(run.text.ends_with("Cancelled: nothing was changed.\n"));
            assert!(dir.log.is_empty());
        }

        #[test]
        fn accepted_confirmation_pulls() {
            let cloud = cloud_with(&[("a.png", "A"), ("b.png", "B")]);
            let mut dir = MemoryDir::default();
            let run = pull_run(&cloud, &mut dir, false, ASK, true);
            run.result.unwrap();
            assert_eq!(run.prompts, ["Pull?"]);
            assert!(
                run.text
                    .contains("\n  downloaded a.png\n  downloaded b.png\n")
            );
            assert!(
                run.text
                    .ends_with("Downloaded 2 files, moved 0 files to the recycle bin.\n")
            );
            assert_eq!(dir.names(), ["a.png", "b.png"]);
            assert_eq!(dir.files["a.png"], b"A");
            assert!(cloud.mutating_calls().is_empty());
        }

        #[test]
        fn yes_pulls_without_asking() {
            let cloud = cloud_with(&[("a.png", "A")]);
            let mut dir = MemoryDir::default();
            let run = pull_run(&cloud, &mut dir, false, YES, false);
            run.result.unwrap();
            assert!(run.prompts.is_empty());
            assert!(
                run.text
                    .ends_with("Downloaded 1 file, moved 0 files to the recycle bin.\n")
            );
            assert_eq!(dir.names(), ["a.png"]);
        }

        #[test]
        fn confirmation_error_propagates_after_the_plan() {
            let cloud = cloud_with(&[("a.png", "A")]);
            let mut dir = MemoryDir::default();
            let mut confirm = |_: &str| Err(CommandError::ConfirmationNeeded);
            let mut out = Vec::new();
            let err = sync_pull(
                &cloud,
                &mut dir,
                &[],
                "Game",
                false,
                ASK,
                &mut confirm,
                &mut out,
            )
            .unwrap_err();
            assert!(matches!(err, CommandError::ConfirmationNeeded));
            assert!(
                String::from_utf8(out)
                    .unwrap()
                    .starts_with("Pull from cloud folder Game:\n")
            );
            assert!(dir.log.is_empty());
        }

        #[test]
        fn noop_never_asks() {
            let cloud = cloud_with(&[("a.png", "A")]);
            let mut dir = MemoryDir::with(&[("a.png", "A")]);
            let run = pull_run(&cloud, &mut dir, false, ASK, false);
            run.result.unwrap();
            assert!(run.prompts.is_empty());
            assert_eq!(run.text, "Pull from cloud folder Game:\n1 identical\n");
            assert!(dir.log.is_empty());
        }

        #[test]
        fn changed_file_is_overwritten_and_shown_in_the_plan() {
            let cloud = cloud_with(&[("a.png", "new")]);
            let mut dir = MemoryDir::with(&[("a.png", "old")]);
            let run = pull_run(&cloud, &mut dir, false, DRY, false);
            run.result.unwrap();
            assert!(
                run.text.contains("  download   a.png  (changed"),
                "{}",
                run.text
            );
            assert!(dir.log.is_empty());

            let run = pull_run(&cloud, &mut dir, false, YES, false);
            run.result.unwrap();
            assert!(run.text.contains("  downloaded a.png\n"));
            assert_eq!(dir.files["a.png"], b"new");
        }

        #[test]
        fn without_prune_local_only_files_are_kept_and_noted() {
            let cloud = cloud_with(&[("a.png", "A")]);
            let mut dir = MemoryDir::with(&[("x.png", "X"), ("y.png", "Y")]);
            let run = pull_run(&cloud, &mut dir, false, YES, false);
            run.result.unwrap();
            assert!(run.text.ends_with(
                "Downloaded 1 file, moved 0 files to the recycle bin.\n\
                 2 local files were kept; run again with --prune to move them to the recycle bin.\n"
            ));
            assert_eq!(dir.names(), ["a.png", "x.png", "y.png"]);

            let mut dir = MemoryDir::with(&[("a.png", "A"), ("x.png", "X")]);
            let run = pull_run(&cloud, &mut dir, false, ASK, false);
            run.result.unwrap();
            assert!(run.prompts.is_empty());
            assert!(run.text.ends_with(
                "1 skip, 1 identical\n\
                 1 local file was kept; run again with --prune to move it to the recycle bin.\n"
            ));
        }

        #[test]
        fn prune_lists_then_trashes() {
            let cloud = cloud_with(&[("a.png", "A")]);
            let mut dir = MemoryDir::with(&[("old.png", "O")]);
            let run = pull_run(&cloud, &mut dir, true, YES, false);
            run.result.unwrap();
            assert!(
                run.text
                    .starts_with("Pull from cloud folder Game (prune):\n")
            );
            assert!(run.text.contains("  delete     old.png"));
            assert!(run.text.contains("\n  downloaded a.png\n"));
            assert!(run.text.contains("\n  trashed    old.png\n"));
            assert!(
                run.text
                    .ends_with("Downloaded 1 file, moved 1 file to the recycle bin.\n")
            );
            assert!(!run.text.contains("kept"));
            assert_eq!(dir.names(), ["a.png"]);
            assert_eq!(dir.log, ["write a.png", "trash old.png"]);
        }

        #[test]
        fn conflict_is_an_error_after_the_plan_even_for_dry_run() {
            for mode in [ASK, DRY, YES] {
                // Two local files that differ only in case clash.
                let cloud = cloud_with(&[]);
                let mut dir = MemoryDir::with(&[("a.png", "1"), ("A.png", "2")]);
                let run = pull_run(&cloud, &mut dir, false, mode, true);
                let err = run.result.unwrap_err();
                assert!(matches!(err, CommandError::PullConflicts(2)));
                assert_eq!(
                    err.to_string(),
                    "cannot pull: 2 conflict(s) in the plan; rename the files involved"
                );
                assert!(run.text.contains("conflict"));
                assert!(run.prompts.is_empty());
                assert!(!run.text.contains("Dry run"));
                assert!(dir.log.is_empty());
            }
        }

        #[test]
        fn conflict_json_is_printed_before_the_error() {
            let cloud = cloud_with(&[]);
            let mut dir = MemoryDir::with(&[("a.png", "1"), ("A.png", "2")]);
            let run = pull_run(&cloud, &mut dir, false, JSON_DRY, false);
            assert!(matches!(
                run.result.unwrap_err(),
                CommandError::PullConflicts(2)
            ));
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["done"], false);
            assert!(value["plan"].is_object());
        }

        #[test]
        fn json_dry_run_shape() {
            let cloud = cloud_with(&[("a.png", "A")]);
            let mut dir = MemoryDir::default();
            let run = pull_run(&cloud, &mut dir, false, JSON_DRY, false);
            run.result.unwrap();
            assert!(run.text.starts_with("{\n"));
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["dry_run"], true);
            assert_eq!(value["done"], false);
            assert_eq!(value["downloaded"], json!([]));
            assert_eq!(value["deleted"], json!([]));
            assert_eq!(value["plan"]["steps"].as_array().unwrap().len(), 1);
            assert!(dir.log.is_empty());
        }

        #[test]
        fn json_real_run_shape() {
            let cloud = cloud_with(&[("a.png", "A")]);
            let mut dir = MemoryDir::with(&[("old.png", "O")]);
            let run = pull_run(&cloud, &mut dir, true, JSON_YES, false);
            run.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["dry_run"], false);
            assert_eq!(value["done"], true);
            assert_eq!(value["downloaded"], json!(["a.png"]));
            assert_eq!(value["deleted"], json!(["old.png"]));
            assert!(value["plan"].is_object());
        }
    }
    mod manifest_cmd_tests {
        use std::path::PathBuf;
        use std::time::UNIX_EPOCH;

        use ttskit_cloud::sha1_hex;

        use super::*;

        const YES: RunMode = RunMode {
            dry_run: false,
            yes: true,
            json: false,
        };
        const DRY: RunMode = RunMode {
            dry_run: true,
            yes: false,
            json: false,
        };
        const ASK: RunMode = RunMode {
            dry_run: false,
            yes: false,
            json: false,
        };
        const JSON_YES: RunMode = RunMode {
            dry_run: false,
            yes: true,
            json: true,
        };
        const JSON_DRY: RunMode = RunMode {
            dry_run: true,
            yes: false,
            json: true,
        };

        const FILES: [(&str, &str); 3] = [("a.png", "A"), ("b.png", "B"), ("sub/c.png", "C")];

        struct Fixture {
            _dir: tempfile::TempDir,
            cloud: MemoryCloud,
            manifest: PathBuf,
        }

        /// A cloud and a manifest produced by a real push of `FILES`.
        fn pushed() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path().join("backups"));
            let manifest = dir.path().join(Manifest::FILE_NAME);
            let mut cloud = MemoryCloud::new();
            let local: Vec<LocalFile> = FILES
                .iter()
                .map(|(path, data)| LocalFile {
                    path: (*path).to_owned(),
                    size: data.len() as u64,
                    sha1: sha1_hex(data.as_bytes()),
                })
                .collect();
            let read = |p: &str| {
                FILES
                    .iter()
                    .find(|(path, _)| *path == p)
                    .map(|(_, d)| d.as_bytes().to_vec())
                    .ok_or_else(|| io::Error::other("no such file"))
            };
            let mut out = Vec::new();
            sync_push(
                &mut cloud,
                &store,
                &local,
                &read,
                "Game",
                false,
                Some(&manifest),
                YES,
                UNIX_EPOCH,
                "10/10/2026 14:05:08",
                &mut |_| Ok(true),
                &mut out,
            )
            .unwrap();
            Fixture {
                _dir: dir,
                cloud,
                manifest,
            }
        }

        struct Out {
            result: Result<(), CommandError>,
            text: String,
            prompts: Vec<String>,
        }

        fn import(fx: &Fixture, text: &str, mode: RunMode, answer: bool) -> Out {
            let mut prompts = Vec::new();
            let mut confirm = |p: &str| {
                prompts.push(p.to_owned());
                Ok(answer)
            };
            let mut out = Vec::new();
            let result = import_tsv(&fx.cloud, text, &fx.manifest, mode, &mut confirm, &mut out);
            Out {
                result,
                text: String::from_utf8(out).unwrap(),
                prompts,
            }
        }

        fn show(path: &Path, format: ManifestFormat) -> String {
            let mut out = Vec::new();
            show_manifest(path, format, &mut out).unwrap();
            String::from_utf8(out).unwrap()
        }

        /// The TSV of the pushed manifest, then removes the manifest.
        fn export_and_remove(fx: &Fixture) -> String {
            let tsv = show(&fx.manifest, ManifestFormat::Tsv);
            std::fs::remove_file(&fx.manifest).unwrap();
            tsv
        }

        #[test]
        fn json_output_is_the_file_content() {
            let fx = pushed();
            let text = show(&fx.manifest, ManifestFormat::Json);
            assert_eq!(text, std::fs::read_to_string(&fx.manifest).unwrap());
            assert_eq!(text, Manifest::load(&fx.manifest).unwrap().to_json());
            assert!(text.ends_with("}\n"));
        }

        #[test]
        fn tsv_output() {
            let fx = pushed();
            let text = show(&fx.manifest, ManifestFormat::Tsv);
            let manifest = Manifest::load(&fx.manifest).unwrap();
            assert_eq!(text, manifest.to_tsv());
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 3);
            assert!(lines[0].starts_with("a.png\thttps://"));
            assert!(lines[2].starts_with("sub/c.png\thttps://"));
        }

        #[test]
        fn missing_manifest_is_empty() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(Manifest::FILE_NAME);
            assert_eq!(show(&path, ManifestFormat::Tsv), "");
            assert_eq!(
                show(&path, ManifestFormat::Json),
                Manifest::default().to_json()
            );
            assert!(show(&path, ManifestFormat::Json).contains("\"entries\": []"));
            assert!(!path.exists());
        }

        #[test]
        fn invalid_manifest_is_an_error() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(Manifest::FILE_NAME);
            std::fs::write(&path, "nope").unwrap();
            let mut out = Vec::new();
            let err = show_manifest(&path, ManifestFormat::Json, &mut out).unwrap_err();
            assert!(matches!(err, CommandError::ManifestRead { .. }));
            assert!(err.to_string().starts_with("cannot read the manifest "));
            assert!(out.is_empty());
        }

        #[test]
        fn export_then_import_round_trips_byte_identically() {
            let fx = pushed();
            let original = std::fs::read(&fx.manifest).unwrap();
            let before = Manifest::load(&fx.manifest).unwrap();
            let tsv = export_and_remove(&fx);
            let calls = fx.cloud.mutating_calls().len();
            let run = import(&fx, &tsv, YES, false);
            run.result.unwrap();
            assert_eq!(
                run.text,
                format!(
                    "Import into manifest {0}:\n\
                     \x20 add        a.png  (Game/a.png)\n\
                     \x20 add        b.png  (Game/b.png)\n\
                     \x20 add        sub/c.png  (Game/sub/c.png)\n\
                     3 add, 0 update, 0 unchanged\n\
                     Updated manifest {0}.\n",
                    fx.manifest.display()
                )
            );
            let after = Manifest::load(&fx.manifest).unwrap();
            let key = |m: &Manifest| {
                let mut v: Vec<_> = m
                    .entries
                    .iter()
                    .map(|e| {
                        (
                            e.path.clone(),
                            e.cloud_path.clone(),
                            e.sha1.clone(),
                            e.size,
                            e.url.clone(),
                        )
                    })
                    .collect();
                v.sort();
                v
            };
            assert_eq!(key(&after), key(&before));
            assert_eq!(std::fs::read(&fx.manifest).unwrap(), original);
            assert_eq!(fx.cloud.mutating_calls().len(), calls);
        }

        #[test]
        fn dry_run_writes_nothing() {
            let fx = pushed();
            let tsv = export_and_remove(&fx);
            let run = import(&fx, &tsv, DRY, false);
            run.result.unwrap();
            assert!(run.prompts.is_empty());
            assert!(run.text.contains("3 add, 0 update, 0 unchanged\n"));
            assert!(run.text.ends_with("Dry run: nothing was changed.\n"));
            assert!(!fx.manifest.exists());
        }

        #[test]
        fn declined_confirmation_writes_nothing() {
            let fx = pushed();
            let tsv = export_and_remove(&fx);
            let run = import(&fx, &tsv, ASK, false);
            run.result.unwrap();
            assert_eq!(run.prompts, ["Import?"]);
            assert!(run.text.ends_with("Cancelled: nothing was changed.\n"));
            assert!(!fx.manifest.exists());
        }

        #[test]
        fn accepted_confirmation_writes() {
            let fx = pushed();
            let tsv = export_and_remove(&fx);
            let run = import(&fx, &tsv, ASK, true);
            run.result.unwrap();
            assert_eq!(run.prompts, ["Import?"]);
            assert_eq!(Manifest::load(&fx.manifest).unwrap().entries.len(), 3);
        }

        #[test]
        fn unchanged_import_does_not_touch_the_file() {
            let fx = pushed();
            let tsv = show(&fx.manifest, ManifestFormat::Tsv);
            // Same content, different bytes: a rewrite would normalize it.
            let odd = Manifest::load(&fx.manifest)
                .unwrap()
                .to_json()
                .replace('\n', "\r\n");
            std::fs::write(&fx.manifest, &odd).unwrap();
            let run = import(&fx, &tsv, ASK, false);
            run.result.unwrap();
            assert!(run.prompts.is_empty());
            assert!(run.text.contains("0 add, 0 update, 3 unchanged\n"));
            assert!(
                run.text
                    .ends_with("Nothing to do: the manifest already has these entries.\n")
            );
            assert!(!run.text.contains("Updated manifest"));
            assert_eq!(std::fs::read_to_string(&fx.manifest).unwrap(), odd);
        }

        #[test]
        fn update_and_add_are_told_apart() {
            let fx = pushed();
            let tsv = show(&fx.manifest, ManifestFormat::Tsv);
            // a.png now points at the content of b.png, b.png is unchanged
            // and sub/c.png is missing.
            let mut manifest = Manifest::load(&fx.manifest).unwrap();
            let b = manifest.entries[1].clone();
            manifest.entries[0] = ManifestEntry {
                path: Some("a.png".into()),
                ..b
            };
            manifest.entries.pop();
            manifest.save(&fx.manifest).unwrap();
            let run = import(&fx, &tsv, DRY, false);
            run.result.unwrap();
            assert!(
                run.text.contains("  update     a.png  (Game/a.png)\n"),
                "{}",
                run.text
            );
            assert!(run.text.contains("  unchanged  b.png  (Game/b.png)\n"));
            assert!(
                run.text
                    .contains("  add        sub/c.png  (Game/sub/c.png)\n")
            );
            assert!(run.text.contains("1 add, 1 update, 1 unchanged\n"));

            import(&fx, &tsv, YES, false).result.unwrap();
            let again = Manifest::load(&fx.manifest).unwrap();
            assert_eq!(again.to_tsv(), tsv);
            assert_eq!(again.entries.len(), 3);
        }

        #[test]
        fn pathless_entries_are_kept() {
            let fx = pushed();
            let tsv = show(&fx.manifest, ManifestFormat::Tsv);
            let mut manifest = Manifest::load(&fx.manifest).unwrap();
            let extra = ManifestEntry {
                path: None,
                cloud_path: "Other/x.png".into(),
                sha1: "0".repeat(40),
                size: 1,
                url: "https://example.invalid/x".into(),
            };
            manifest.entries.push(extra.clone());
            manifest.save(&fx.manifest).unwrap();
            import(&fx, &tsv, YES, false).result.unwrap();
            assert!(
                Manifest::load(&fx.manifest)
                    .unwrap()
                    .entries
                    .contains(&extra)
            );
        }

        fn tsv_error(fx: &Fixture, text: &str) -> String {
            let before = std::fs::read(&fx.manifest).ok();
            let run = import(fx, text, YES, true);
            let err = run.result.unwrap_err();
            assert!(matches!(err, CommandError::Tsv(_)), "{err}");
            assert!(run.prompts.is_empty());
            assert_eq!(run.text, "");
            assert_eq!(std::fs::read(&fx.manifest).ok(), before);
            err.to_string()
        }

        #[test]
        fn malformed_lines_are_listed_and_write_nothing() {
            let fx = pushed();
            let url = Manifest::load(&fx.manifest).unwrap().entries[0].url.clone();
            let unknown = format!(
                "https://steamusercontent-a.akamaihd.net/ugc/1/{}/",
                "A".repeat(40)
            );
            let cases = [
                (
                    "a.png".to_owned(),
                    "line 1: expected `filename<TAB>URL`, found no tab",
                ),
                (
                    format!("a.png\t{url}\tx"),
                    "line 1: expected exactly one tab, found 2",
                ),
                (format!("\t{url}"), "line 1: the filename is empty"),
                ("a.png\t".to_owned(), "line 1: the URL is empty"),
                (
                    "a.png\tgarbage".to_owned(),
                    "line 1: not a valid cloud URL: garbage",
                ),
                (
                    format!("a.png\t{unknown}"),
                    "line 1: https://steamusercontent-a.akamaihd.net/ugc/1/",
                ),
            ];
            for (text, expected) in cases {
                let message = tsv_error(&fx, &text);
                assert!(message.starts_with(expected), "{text:?}: {message}");
                assert_eq!(message.lines().count(), 1, "{message}");
            }
            let message = tsv_error(&fx, &format!("a.png\t{url}\nA.PNG\t{url}"));
            assert!(
                message.starts_with("line 2: duplicate filename"),
                "{message}"
            );

            let text = format!("# header\n\nbad\n{url}\na.png\t{url}\n\\x\t{url}\n");
            let message = tsv_error(&fx, &text);
            let lines: Vec<&str> = message.lines().collect();
            assert_eq!(lines.len(), 3, "{message}");
            assert!(lines[0].starts_with("line 3: "));
            assert!(lines[1].starts_with("line 4: "));
            assert!(lines[2].starts_with("line 6: "));
            assert_eq!(
                message,
                [
                    "line 3: expected `filename<TAB>URL`, found no tab",
                    "line 4: expected `filename<TAB>URL`, found no tab",
                    "line 6: invalid filename \"\\\\x\": contains a backslash",
                ]
                .join("\n")
            );
        }

        #[test]
        fn json_mode_returns_the_tsv_error_without_output() {
            let fx = pushed();
            let run = import(&fx, "bad\n", JSON_YES, true);
            assert!(matches!(run.result, Err(CommandError::Tsv(_))));
            assert_eq!(run.text, "");
        }

        #[test]
        fn bom_and_crlf_are_accepted() {
            let fx = pushed();
            let tsv = export_and_remove(&fx);
            let text = format!("\u{feff}{}", tsv.replace('\n', "\r\n"));
            import(&fx, &text, YES, false).result.unwrap();
            assert_eq!(Manifest::load(&fx.manifest).unwrap().to_tsv(), tsv);
        }

        #[test]
        fn json_shapes() {
            let fx = pushed();
            let tsv = export_and_remove(&fx);
            let run = import(&fx, &tsv, JSON_DRY, false);
            run.result.unwrap();
            assert!(run.text.starts_with("{\n"));
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["manifest"], fx.manifest.display().to_string());
            assert_eq!(value["dry_run"], true);
            assert_eq!(value["done"], false);
            let entries = value["entries"].as_array().unwrap();
            assert_eq!(entries.len(), 3);
            let a = &entries[0];
            assert_eq!(a["action"], "add");
            assert_eq!(a["path"], "a.png");
            assert_eq!(a["cloud_path"], "Game/a.png");
            assert_eq!(a["sha1"], sha1_hex(b"A").to_ascii_uppercase());
            assert_eq!(a["size"], 1);
            assert!(a["url"].as_str().unwrap().starts_with("https://"));
            assert!(!fx.manifest.exists());

            let run = import(&fx, &tsv, JSON_YES, false);
            run.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["dry_run"], false);
            assert_eq!(value["done"], true);
            assert!(fx.manifest.exists());

            let run = import(&fx, &tsv, JSON_YES, false);
            let value: serde_json::Value = serde_json::from_str(&run.text).unwrap();
            assert_eq!(value["done"], true);
            assert_eq!(value["entries"][0]["action"], "unchanged");
        }

        #[test]
        fn manifest_write_failure_is_reported() {
            let fx = pushed();
            let tsv = export_and_remove(&fx);
            let missing = fx
                .manifest
                .parent()
                .unwrap()
                .join("no-such-dir")
                .join("m.json");
            let mut out = Vec::new();
            let err = import_tsv(&fx.cloud, &tsv, &missing, YES, &mut |_| Ok(true), &mut out)
                .unwrap_err();
            assert!(matches!(err, CommandError::ManifestWrite { .. }));
            assert!(err.to_string().starts_with("cannot write the manifest "));
            let text = String::from_utf8(out).unwrap();
            assert!(!text.contains("Updated manifest"));
        }

        #[test]
        fn read_tsv_file_errors() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("list.tsv");
            let err = read_tsv_file(&path).unwrap_err();
            assert!(matches!(err, CommandError::ReadFile { .. }));
            std::fs::write(&path, [0xff, 0xfe, 0x41]).unwrap();
            let err = read_tsv_file(&path).unwrap_err();
            assert!(matches!(err, CommandError::ReadFile { .. }));
            assert!(err.to_string().starts_with("cannot read "));
            std::fs::write(&path, "a\tb\n").unwrap();
            assert_eq!(read_tsv_file(&path).unwrap(), "a\tb\n");
        }
    }
    mod scan_tests {
        use super::*;

        fn fixture() -> PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/custom_assets.json")
        }

        fn run(path: &Path, json: bool) -> Result<String, CommandError> {
            let mut out = Vec::new();
            scan(path, json, &mut out)?;
            Ok(String::from_utf8(out).unwrap())
        }

        #[test]
        fn text_groups_by_host_class() {
            let text = run(&fixture(), false).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            let pos = |prefix: &str| lines.iter().position(|l| l.starts_with(prefix)).unwrap();
            assert!(pos("imgur (") < pos("steamusercontent ("));
            assert!(pos("steamusercontent (") < pos("other ("));
            assert!(text.contains("  https://i.imgur.com/AAAAAAA.png  ["));
            assert!(text.contains("  x"), "{text}");
            assert!(lines.last().unwrap().ends_with(" unique URLs"));
            assert_eq!(text, run(&fixture(), false).unwrap());
        }

        #[test]
        fn json_has_all_hosts_and_total() {
            let text = run(&fixture(), true).unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            let hosts = value["hosts"].as_object().unwrap();
            assert_eq!(hosts.len(), 3);
            let total: usize = hosts.values().map(|v| v.as_array().unwrap().len()).sum();
            assert_eq!(value["total"], total);
            assert_eq!(hosts["imgur"][0]["url"], "https://i.imgur.com/AAAAAAA.png");
            assert!(hosts["steamusercontent"].as_array().unwrap().len() >= 2);
            assert!(hosts["imgur"][0]["labels"].is_array());
            assert!(hosts["imgur"][0]["count"].is_u64());
            assert!(
                value["save"]
                    .as_str()
                    .unwrap()
                    .ends_with("custom_assets.json")
            );
        }

        #[test]
        fn no_urls() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("empty.json");
            std::fs::write(&path, "{\"ObjectStates\": []}").unwrap();
            assert_eq!(run(&path, false).unwrap(), "No asset URLs found.\n");
            let value: serde_json::Value =
                serde_json::from_str(&run(&path, true).unwrap()).unwrap();
            assert_eq!(value["total"], 0);
            assert_eq!(value["hosts"]["other"], json!([]));
        }

        #[test]
        fn invalid_and_missing_files_fail() {
            let dir = tempfile::tempdir().unwrap();
            let bad = dir.path().join("bad.json");
            std::fs::write(&bad, "{").unwrap();
            let err = run(&bad, false).unwrap_err();
            assert!(matches!(err, CommandError::InvalidSave { .. }));
            assert!(err.to_string().contains("bad.json"));
            let err = run(&dir.path().join("none.json"), false).unwrap_err();
            assert!(matches!(err, CommandError::ReadFile { .. }));
        }
    }
    mod scan_adopt_tests {
        use ttskit_cloud::{IndexEntry, sha1_hex};

        use super::*;

        const YES: RunMode = RunMode {
            dry_run: false,
            yes: true,
            json: false,
        };
        const DRY: RunMode = RunMode {
            dry_run: true,
            yes: false,
            json: false,
        };
        const ASK: RunMode = RunMode {
            dry_run: false,
            yes: false,
            json: false,
        };
        const JSON_YES: RunMode = RunMode {
            dry_run: false,
            yes: true,
            json: true,
        };

        fn sha(data: &str) -> String {
            sha1_hex(data.as_bytes())
        }

        fn index_entry(handle: u64, data: &str, name: &str, folder: &str) -> IndexEntry {
            IndexEntry::new(
                format!("{}_{name}", sha(data)),
                name,
                Some(format!(
                    "https://steamusercontent-a.akamaihd.net/ugc/{handle}/{}/",
                    sha(data)
                )),
                Some(data.len() as u64),
                None,
                folder,
            )
        }

        struct Fixture {
            dir: tempfile::TempDir,
            cloud: MemoryCloud,
            manifest: PathBuf,
        }

        impl Fixture {
            fn new(entries: Vec<IndexEntry>) -> Fixture {
                let dir = tempfile::tempdir().unwrap();
                let mut index = CloudIndex::empty();
                index.entries = entries;
                let cloud = MemoryCloud::new()
                    .with_file(ttskit_cloud::CLOUD_INFO_FILE, index.encode_info().unwrap())
                    .with_file(
                        ttskit_cloud::CLOUD_FOLDER_FILE,
                        index.encode_folders().unwrap(),
                    );
                let manifest = dir.path().join(Manifest::FILE_NAME);
                Fixture {
                    dir,
                    cloud,
                    manifest,
                }
            }

            fn save(&self, urls: &[String]) -> SaveScan {
                let objects: Vec<_> = urls
                    .iter()
                    .map(|u| json!({"Nickname": "x", "CustomImage": {"ImageURL": u}}))
                    .collect();
                let path = self.dir.path().join("save.json");
                std::fs::write(&path, json!({"ObjectStates": objects}).to_string()).unwrap();
                read_scan(&path).unwrap()
            }
        }

        struct Out {
            result: Result<(), CommandError>,
            text: String,
            prompts: usize,
        }

        fn adopt(fx: &Fixture, scan: &SaveScan, mode: RunMode, answer: bool) -> Out {
            let mut prompts = 0;
            let mut confirm = |_: &str| {
                prompts += 1;
                Ok(answer)
            };
            let mut out = Vec::new();
            let manifest = load_manifest(&fx.manifest).unwrap();
            let result = scan_adopt(
                &fx.cloud,
                scan,
                manifest,
                &fx.manifest,
                mode,
                &mut confirm,
                &mut out,
            );
            Out {
                result,
                text: String::from_utf8(out).unwrap(),
                prompts,
            }
        }

        fn akamai(handle: u64, data: &str) -> String {
            format!(
                "https://steamusercontent-a.akamaihd.net/ugc/{handle}/{}/",
                sha(data)
            )
        }

        fn cloud3(handle: u64, data: &str, slash: &str) -> String {
            format!(
                "http://cloud-3.steamusercontent.com/ugc/{handle}/{}{slash}",
                sha(data)
            )
        }

        fn fixture() -> Fixture {
            Fixture::new(vec![
                index_entry(11, "one", "one.png", "Game"),
                index_entry(22, "two", "two.png", ""),
            ])
        }

        #[test]
        fn adopts_matches_in_both_url_forms() {
            let fx = fixture();
            let scan = fx.save(&[akamai(11, "one"), cloud3(22, "two", "")]);
            let r = adopt(&fx, &scan, YES, false);
            r.result.unwrap();
            assert_eq!(r.prompts, 0);
            assert!(r.text.contains("  add        Game/one.png\n"), "{}", r.text);
            assert!(r.text.contains("  add        two.png\n"), "{}", r.text);
            assert!(r.text.contains("2 add, 0 unchanged, 0 unmatched\n"));
            assert!(r.text.contains("Updated manifest "));
            // The scan report comes first.
            assert!(r.text.starts_with("steamusercontent") || r.text.starts_with("imgur"));
            let manifest = Manifest::load(&fx.manifest).unwrap();
            assert_eq!(
                manifest.entries,
                vec![
                    ManifestEntry {
                        path: None,
                        cloud_path: "Game/one.png".into(),
                        sha1: sha("one"),
                        size: 3,
                        url: akamai(11, "one"),
                    },
                    ManifestEntry {
                        path: None,
                        cloud_path: "two.png".into(),
                        sha1: sha("two"),
                        size: 3,
                        url: akamai(22, "two"),
                    },
                ]
            );
            assert_eq!(
                manifest.to_json(),
                std::fs::read_to_string(&fx.manifest).unwrap()
            );
            assert!(fx.cloud.mutating_calls().is_empty());
        }

        #[test]
        fn trailing_slash_variants_match() {
            let fx = fixture();
            let scan = fx.save(&[
                akamai(11, "one").trim_end_matches('/').to_owned(),
                cloud3(22, "two", "/"),
            ]);
            adopt(&fx, &scan, YES, false).result.unwrap();
            assert_eq!(Manifest::load(&fx.manifest).unwrap().entries.len(), 2);
        }

        #[test]
        fn both_forms_of_one_file_give_one_entry() {
            let fx = fixture();
            let scan = fx.save(&[akamai(11, "one"), cloud3(11, "one", "/")]);
            let r = adopt(&fx, &scan, YES, false);
            r.result.unwrap();
            assert!(r.text.contains("1 add, 0 unchanged, 0 unmatched\n"));
            assert_eq!(Manifest::load(&fx.manifest).unwrap().entries.len(), 1);
        }

        #[test]
        fn rerun_is_unchanged_and_writes_nothing() {
            let fx = fixture();
            let scan = fx.save(&[akamai(11, "one")]);
            adopt(&fx, &scan, YES, false).result.unwrap();
            let before = std::fs::read(&fx.manifest).unwrap();
            let modified = std::fs::metadata(&fx.manifest).unwrap().modified().unwrap();
            let r = adopt(&fx, &scan, ASK, false);
            r.result.unwrap();
            assert_eq!(r.prompts, 0);
            assert!(r.text.contains("  unchanged  Game/one.png\n"), "{}", r.text);
            assert!(r.text.contains("0 add, 1 unchanged, 0 unmatched\n"));
            assert!(r.text.contains("Nothing to do"));
            assert!(!r.text.contains("Updated manifest"));
            assert_eq!(std::fs::read(&fx.manifest).unwrap(), before);
            assert_eq!(
                std::fs::metadata(&fx.manifest).unwrap().modified().unwrap(),
                modified
            );
        }

        #[test]
        fn dry_run_writes_nothing() {
            let fx = fixture();
            let scan = fx.save(&[akamai(11, "one")]);
            let r = adopt(&fx, &scan, DRY, true);
            r.result.unwrap();
            assert_eq!(r.prompts, 0);
            assert!(r.text.contains("1 add, 0 unchanged, 0 unmatched\n"));
            assert!(r.text.contains("Dry run: nothing was changed."));
            assert!(!fx.manifest.exists());
        }

        #[test]
        fn declined_prompt_writes_nothing() {
            let fx = fixture();
            let scan = fx.save(&[akamai(11, "one")]);
            let r = adopt(&fx, &scan, ASK, false);
            r.result.unwrap();
            assert_eq!(r.prompts, 1);
            assert!(r.text.contains("Cancelled"));
            assert!(!fx.manifest.exists());
            let r = adopt(&fx, &scan, ASK, true);
            r.result.unwrap();
            assert!(fx.manifest.exists());
        }

        #[test]
        fn unmatched_and_foreign_urls() {
            let fx = fixture();
            let stranger = akamai(99, "stranger");
            // Right handle, wrong content.
            let wrong_sha = akamai(11, "other");
            let scan = fx.save(&[
                stranger.clone(),
                wrong_sha.clone(),
                "https://i.imgur.com/AAAAAAA.png".into(),
                "https://example.com/a.png".into(),
                akamai(11, "one"),
            ]);
            let r = adopt(&fx, &scan, YES, false);
            r.result.unwrap();
            assert!(
                r.text.contains(&format!("  unmatched  {stranger}\n")),
                "{}",
                r.text
            );
            assert!(r.text.contains(&format!("  unmatched  {wrong_sha}\n")));
            assert!(!r.text.contains("unmatched  https://i.imgur"));
            assert!(!r.text.contains("unmatched  https://example"));
            assert!(r.text.contains("1 add, 0 unchanged, 2 unmatched\n"));
            assert_eq!(Manifest::load(&fx.manifest).unwrap().entries.len(), 1);
        }

        #[test]
        fn only_unmatched_asks_nothing_and_writes_nothing() {
            let fx = fixture();
            let scan = fx.save(&[akamai(99, "stranger")]);
            let r = adopt(&fx, &scan, ASK, false);
            r.result.unwrap();
            assert_eq!(r.prompts, 0);
            assert!(r.text.contains("0 add, 0 unchanged, 1 unmatched\n"));
            assert!(!fx.manifest.exists());
        }

        #[test]
        fn existing_local_path_entry_is_not_duplicated() {
            let fx = fixture();
            let local = ManifestEntry {
                path: Some("one.png".into()),
                cloud_path: "Game/one.png".into(),
                sha1: sha("one"),
                size: 3,
                url: akamai(11, "one"),
            };
            Manifest {
                entries: vec![local.clone()],
            }
            .save(&fx.manifest)
            .unwrap();
            let scan = fx.save(&[akamai(11, "one"), akamai(22, "two")]);
            let r = adopt(&fx, &scan, YES, false);
            r.result.unwrap();
            assert!(r.text.contains("  unchanged  Game/one.png\n"), "{}", r.text);
            assert!(r.text.contains("1 add, 1 unchanged, 0 unmatched\n"));
            let entries = Manifest::load(&fx.manifest).unwrap().entries;
            assert_eq!(entries.len(), 2);
            assert!(entries.contains(&local));
            assert!(
                entries
                    .iter()
                    .any(|e| e.path.is_none() && e.cloud_path == "two.png")
            );
        }

        #[test]
        fn unusable_index_entry_is_unmatched() {
            let mut no_size = index_entry(11, "one", "one.png", "Game");
            no_size.size = None;
            let fx = Fixture::new(vec![no_size]);
            let scan = fx.save(&[akamai(11, "one")]);
            let r = adopt(&fx, &scan, YES, false);
            r.result.unwrap();
            assert!(r.text.contains("0 add, 0 unchanged, 1 unmatched\n"));
            assert!(!fx.manifest.exists());
        }

        #[test]
        fn json_is_one_object_with_adopt_section() {
            let fx = fixture();
            let scan = fx.save(&[akamai(11, "one"), akamai(99, "stranger")]);
            let r = adopt(&fx, &scan, JSON_YES, false);
            r.result.unwrap();
            let value: serde_json::Value = serde_json::from_str(&r.text).unwrap();
            assert_eq!(value["total"], 2);
            assert!(value["hosts"]["steamusercontent"].is_array());
            let a = &value["adopt"];
            assert_eq!(a["dry_run"], false);
            assert_eq!(a["done"], true);
            assert_eq!(a["entries"].as_array().unwrap().len(), 1);
            assert_eq!(a["entries"][0]["action"], "add");
            assert_eq!(a["entries"][0]["cloud_path"], "Game/one.png");
            assert_eq!(a["entries"][0]["sha1"], sha("one"));
            assert_eq!(a["entries"][0]["size"], 3);
            assert_eq!(a["entries"][0]["url"], akamai(11, "one"));
            assert_eq!(a["unmatched"], json!([akamai(99, "stranger")]));
            assert!(a["manifest"].is_string());
        }

        #[test]
        fn plain_scan_json_has_no_adopt_key() {
            let fx = fixture();
            let scan = fx.save(&[akamai(11, "one")]);
            assert!(scan_json(&scan).get("adopt").is_none());
        }

        #[test]
        fn manifest_write_failure_is_reported() {
            let fx = fixture();
            let scan = fx.save(&[akamai(11, "one")]);
            let missing = fx.dir.path().join("no/such/dir").join(Manifest::FILE_NAME);
            let mut out = Vec::new();
            let err = scan_adopt(
                &fx.cloud,
                &scan,
                Manifest::default(),
                &missing,
                YES,
                &mut |_| Ok(true),
                &mut out,
            )
            .unwrap_err();
            assert!(matches!(err, CommandError::ManifestWrite { .. }));
        }
    }
}
