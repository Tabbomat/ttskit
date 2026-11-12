//! The `cloud` subcommands.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::json;
use ttskit_cloud::{
    Backup, BackupError, BackupStore, CloudBackend, CloudError, CloudIndex, DeleteError,
    DeletePlan, DeleteTarget, FileState, IndexEntry, IndexError, LocalFile, RestoreAction,
    StatusError, UgcUrl, UploadError, UploadPlan, compare, delete, find_entries, is_game_internal,
    normalize_folder, plan_delete, plan_restore, plan_upload, restore, upload,
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
/// line of the text output.
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
                    writeln!(out, "{}", url.as_deref().unwrap_or_default())?;
                }
            }
            Decision::DryRun | Decision::Cancelled => {}
        }
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
            }),
            out,
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
            (name, data, folder): (&str, &[u8], &str),
            mode: RunMode,
            answer: bool,
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
}
