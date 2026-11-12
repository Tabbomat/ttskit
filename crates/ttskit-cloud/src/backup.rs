//! Local backups of the cloud index files, restore, and the safe index write.
//!
//! A backup is a directory named after the UTC time it was taken
//! (`YYYYMMDDTHHMMSSZ`, with `-2`, `-3`, ... on collisions) holding copies of
//! `CloudInfo.bson` and `CloudFolder.bson`. Every write to the cloud index
//! goes through [`CloudIndex::save`] or [`restore`], which take a backup first.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, CloudBackend, CloudError, CloudIndex, IndexError};

/// The two index files, in the order they are backed up and written.
const INDEX_FILES: [&str; 2] = [CLOUD_INFO_FILE, CLOUD_FOLDER_FILE];

/// Errors from backups, restores and index writes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BackupError {
    /// A local file operation failed.
    #[error("{}: {message}", path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// A cloud operation failed.
    #[error(transparent)]
    Cloud(#[from] CloudError),
    /// The index could not be encoded or does not survive a round trip.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// The name is not a backup name.
    #[error("invalid backup name {name:?}: expected the form YYYYMMDDTHHMMSSZ")]
    InvalidName {
        /// The rejected name.
        name: String,
    },
    /// No backup of this name exists.
    #[error("backup {name} not found")]
    NotFound {
        /// The requested name.
        name: String,
    },
    /// The backup holds index files that cannot be parsed.
    #[error("backup {name} is not a valid cloud index backup: {source}")]
    InvalidBackup {
        /// The backup name.
        name: String,
        /// Why the files were rejected.
        source: IndexError,
    },
    /// A cloud write failed after earlier ones succeeded.
    #[error(
        "cloud index only partly written: changed {changed:?}, failed on {failed}: {source}; \
         the previous index is in backup {backup}"
    )]
    Incomplete {
        /// The backup holding the previous index files.
        backup: String,
        /// Files already changed in the cloud.
        changed: Vec<&'static str>,
        /// The file whose change failed.
        failed: &'static str,
        /// The cloud error.
        source: CloudError,
    },
}

fn io_err(path: &Path, e: &io::Error) -> BackupError {
    BackupError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// A backup on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// The backup name, `YYYYMMDDTHHMMSSZ` plus an optional `-N` suffix.
    pub name: String,
    /// The backup directory.
    pub path: PathBuf,
    /// Exactly two entries: `CloudInfo.bson`, then `CloudFolder.bson`.
    pub files: Vec<BackupFile>,
}

/// One index file within a [`Backup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFile {
    /// The cloud file name.
    pub name: &'static str,
    /// Size in bytes, or `None` if the file did not exist in the cloud.
    pub size: Option<u64>,
}

impl Backup {
    fn open(name: &str, path: PathBuf) -> Result<Backup, BackupError> {
        let mut files = Vec::new();
        for file in INDEX_FILES {
            let p = path.join(file);
            let size = match fs::metadata(&p) {
                Ok(m) => Some(m.len()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(io_err(&p, &e)),
            };
            files.push(BackupFile { name: file, size });
        }
        Ok(Backup {
            name: name.to_owned(),
            path,
            files,
        })
    }

    /// Reads a backed-up file; `None` if the file was absent from the cloud.
    pub fn read(&self, file: &'static str) -> Result<Option<Vec<u8>>, BackupError> {
        let p = self.path.join(file);
        match fs::read(&p) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(&p, &e)),
        }
    }
}

/// A directory holding backups.
#[derive(Debug, Clone)]
pub struct BackupStore {
    root: PathBuf,
}

/// Parses a backup name into its timestamp part and suffix number
/// (no suffix counts as 1). `None` if the name is not well-formed.
fn parse_name(name: &str) -> Option<(&str, u64)> {
    let b = name.as_bytes();
    if b.len() < 16 {
        return None;
    }
    let ok = b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'Z';
    if !ok {
        return None;
    }
    let rest = &name[16..];
    if rest.is_empty() {
        return Some((&name[..16], 1));
    }
    let digits = rest.strip_prefix('-')?;
    if digits.starts_with('0') || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((&name[..16], digits.parse().ok()?))
}

/// Formats the UTC time as `YYYYMMDDTHHMMSSZ`; times before the epoch count
/// as the epoch.
fn format_utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Reads a cloud file; a missing file gives `None`.
fn read_cloud(backend: &dyn CloudBackend, name: &str) -> Result<Option<Vec<u8>>, CloudError> {
    match backend.read(name) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(CloudError::NotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

impl BackupStore {
    /// A store keeping its backups in `root`. The directory is created on the
    /// first backup.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        BackupStore { root: root.into() }
    }

    /// The directory holding the backups.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Copies both index files from the cloud into a new backup.
    ///
    /// A file missing in the cloud is absent from the backup. The bytes are
    /// copied as they are, valid BSON or not. The backup appears under its
    /// name only once complete.
    pub fn create(
        &self,
        backend: &dyn CloudBackend,
        now: SystemTime,
    ) -> Result<Backup, BackupError> {
        let mut contents = Vec::new();
        for file in INDEX_FILES {
            contents.push((file, read_cloud(backend, file)?));
        }
        fs::create_dir_all(&self.root).map_err(|e| io_err(&self.root, &e))?;

        let base = format_utc(now);
        let mut name = base.clone();
        let mut n = 1;
        while self.root.join(&name).exists() {
            n += 1;
            name = format!("{base}-{n}");
        }
        let staging = self.root.join(format!(".{name}.tmp"));
        if staging.exists() {
            fs::remove_dir_all(&staging).map_err(|e| io_err(&staging, &e))?;
        }
        fs::create_dir(&staging).map_err(|e| io_err(&staging, &e))?;
        let filled = (|| {
            for (file, data) in &contents {
                if let Some(data) = data {
                    let p = staging.join(file);
                    fs::write(&p, data).map_err(|e| io_err(&p, &e))?;
                }
            }
            Ok(())
        })();
        let target = self.root.join(&name);
        let result =
            filled.and_then(|()| fs::rename(&staging, &target).map_err(|e| io_err(&target, &e)));
        if let Err(e) = result {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
        Backup::open(&name, target)
    }

    /// All backups, newest first. A missing root gives an empty list.
    pub fn list(&self) -> Result<Vec<Backup>, BackupError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&self.root, &e)),
        };
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_err(&self.root, &e))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Some((ts, n)) = parse_name(&name) else {
                continue;
            };
            let key = (ts.to_owned(), n);
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            found.push((key, Backup::open(&name, path)?));
        }
        found.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(found.into_iter().map(|(_, b)| b).collect())
    }

    /// Looks up one backup by name.
    pub fn get(&self, name: &str) -> Result<Backup, BackupError> {
        if parse_name(name).is_none() {
            return Err(BackupError::InvalidName {
                name: name.to_owned(),
            });
        }
        let path = self.root.join(name);
        if !path.is_dir() {
            return Err(BackupError::NotFound {
                name: name.to_owned(),
            });
        }
        Backup::open(name, path)
    }
}

/// What a restore does to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAction {
    /// Write the backed-up content, `size` bytes.
    Write {
        /// Size of the content in bytes.
        size: u64,
    },
    /// Delete the file: it did not exist when the backup was taken.
    Delete,
    /// The cloud already matches the backup.
    Unchanged,
}

/// One step of a [`RestorePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreStep {
    /// The cloud file.
    pub file: &'static str,
    /// What happens to it.
    pub action: RestoreAction,
}

/// What restoring a backup would change; see [`plan_restore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    /// The backup to restore.
    pub backup: Backup,
    /// One step per index file, in write order.
    pub steps: Vec<RestoreStep>,
    /// The bytes to write, aligned with `steps`; exactly what was planned.
    data: Vec<Option<Vec<u8>>>,
}

impl RestorePlan {
    /// Whether the cloud already matches the backup.
    pub fn is_noop(&self) -> bool {
        self.steps
            .iter()
            .all(|s| s.action == RestoreAction::Unchanged)
    }
}

/// The result of [`restore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// The backup of the state before the restore; `None` if nothing changed.
    pub pre_restore_backup: Option<Backup>,
    /// The files that were written or deleted.
    pub changed: Vec<&'static str>,
}

/// Compares `backup` with the cloud and plans the restore without changing
/// anything. Refuses backups whose files are not a valid index.
pub fn plan_restore(
    backend: &dyn CloudBackend,
    backup: &Backup,
) -> Result<RestorePlan, BackupError> {
    let info = backup.read(CLOUD_INFO_FILE)?;
    let folders = backup.read(CLOUD_FOLDER_FILE)?;
    CloudIndex::decode(info.as_deref(), folders.as_deref()).map_err(|source| {
        BackupError::InvalidBackup {
            name: backup.name.clone(),
            source,
        }
    })?;
    let mut steps = Vec::new();
    let mut data = Vec::new();
    for (file, wanted) in INDEX_FILES.into_iter().zip([info, folders]) {
        let current = read_cloud(backend, file)?;
        let action = match (&wanted, &current) {
            (Some(w), Some(c)) if w == c => RestoreAction::Unchanged,
            (Some(w), _) => RestoreAction::Write {
                size: u64::try_from(w.len()).unwrap_or(u64::MAX),
            },
            (None, Some(_)) => RestoreAction::Delete,
            (None, None) => RestoreAction::Unchanged,
        };
        steps.push(RestoreStep { file, action });
        data.push(wanted);
    }
    Ok(RestorePlan {
        backup: backup.clone(),
        steps,
        data,
    })
}

/// A change to one cloud file.
enum Change<'a> {
    Write(&'a [u8]),
    Delete,
}

/// Applies the changes in order. A failure is reported as
/// [`BackupError::Incomplete`] naming `backup`.
fn apply(
    backend: &mut dyn CloudBackend,
    backup: &Backup,
    changes: Vec<(&'static str, Change<'_>)>,
) -> Result<Vec<&'static str>, BackupError> {
    let mut changed = Vec::new();
    for (file, change) in changes {
        let result = match change {
            Change::Write(data) => backend.write(file, data),
            Change::Delete => match backend.delete(file) {
                Err(CloudError::NotFound { .. }) => Ok(()),
                other => other,
            },
        };
        match result {
            Ok(()) => changed.push(file),
            Err(source) => {
                return Err(BackupError::Incomplete {
                    backup: backup.name.clone(),
                    changed,
                    failed: file,
                    source,
                });
            }
        }
    }
    Ok(changed)
}

/// Restores the index files from the planned backup.
///
/// Backs up the current index first (into `store`), then applies the plan.
/// Does nothing if the plan is a no-op. If the pre-restore backup cannot be
/// created, the cloud is not touched.
pub fn restore(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    plan: &RestorePlan,
    now: SystemTime,
) -> Result<RestoreOutcome, BackupError> {
    if plan.is_noop() {
        return Ok(RestoreOutcome {
            pre_restore_backup: None,
            changed: Vec::new(),
        });
    }
    let pre = store.create(backend, now)?;
    let changes = plan
        .steps
        .iter()
        .zip(&plan.data)
        .filter_map(|(step, data)| match (&step.action, data) {
            (RestoreAction::Write { .. }, Some(d)) => Some((step.file, Change::Write(d))),
            (RestoreAction::Delete, _) => Some((step.file, Change::Delete)),
            _ => None,
        })
        .collect();
    let changed = apply(backend, &pre, changes)?;
    Ok(RestoreOutcome {
        pre_restore_backup: Some(pre),
        changed,
    })
}

/// The result of [`CloudIndex::save`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    /// The backup taken before writing; `None` if nothing needed writing.
    pub backup: Option<Backup>,
    /// The files that were written.
    pub changed: Vec<&'static str>,
}

impl CloudIndex {
    /// Writes the index to the cloud after backing up the current index files.
    ///
    /// Both files are encoded and checked first: each must decode again and
    /// re-encode to the same bytes, or nothing is written. Only files whose
    /// bytes differ from the cloud are written; if none differ, no backup is
    /// taken either. This is the only intended way to write the index.
    pub fn save(
        &self,
        backend: &mut dyn CloudBackend,
        store: &BackupStore,
        now: SystemTime,
    ) -> Result<SaveOutcome, BackupError> {
        let pending = self.pending_writes(backend)?;
        if pending.is_empty() {
            return Ok(SaveOutcome {
                backup: None,
                changed: Vec::new(),
            });
        }
        let backup = store.create(backend, now)?;
        let changed = apply_writes(backend, &backup, &pending)?;
        Ok(SaveOutcome {
            backup: Some(backup),
            changed,
        })
    }

    /// Writes the index like [`save`](Self::save), but into an existing
    /// `backup` taken earlier instead of taking a new one. Only files whose
    /// bytes differ from the cloud are written. A failed write is reported as
    /// [`BackupError::Incomplete`] naming `backup`.
    pub(crate) fn write_after_backup(
        &self,
        backend: &mut dyn CloudBackend,
        backup: &Backup,
    ) -> Result<Vec<&'static str>, BackupError> {
        let pending = self.pending_writes(backend)?;
        apply_writes(backend, backup, &pending)
    }

    /// Encodes and checks both files and returns those that differ from the
    /// cloud.
    fn pending_writes(
        &self,
        backend: &dyn CloudBackend,
    ) -> Result<Vec<(&'static str, Vec<u8>)>, BackupError> {
        let info = self.encode_info()?;
        let folders = self.encode_folders()?;
        let again = CloudIndex::decode(Some(&info), None)?.encode_info()?;
        if again != info {
            return Err(round_trip_error(CLOUD_INFO_FILE));
        }
        let again = CloudIndex::decode(None, Some(&folders))?.encode_folders()?;
        if again != folders {
            return Err(round_trip_error(CLOUD_FOLDER_FILE));
        }

        let mut pending = Vec::new();
        for (file, data) in INDEX_FILES.into_iter().zip([info, folders]) {
            if read_cloud(backend, file)?.as_deref() != Some(data.as_slice()) {
                pending.push((file, data));
            }
        }
        Ok(pending)
    }
}

fn apply_writes(
    backend: &mut dyn CloudBackend,
    backup: &Backup,
    pending: &[(&'static str, Vec<u8>)],
) -> Result<Vec<&'static str>, BackupError> {
    let changes = pending
        .iter()
        .map(|(file, data)| (*file, Change::Write(data)))
        .collect();
    apply(backend, backup, changes)
}

fn round_trip_error(file: &'static str) -> BackupError {
    BackupError::Index(IndexError::Bson {
        file,
        message: "encoded index does not survive a decode and re-encode round trip".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(secs: u64) -> String {
        format_utc(UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn formats_the_epoch() {
        assert_eq!(at(0), "19700101T000000Z");
    }

    #[test]
    fn formats_a_known_timestamp() {
        assert_eq!(at(1_778_521_788), "20260511T174948Z");
    }

    #[test]
    fn formats_a_leap_day() {
        // 2024-02-29 12:34:56 UTC
        assert_eq!(at(1_709_210_096), "20240229T123456Z");
    }

    #[test]
    fn formats_the_end_of_a_year() {
        // 1999-12-31 23:59:59 UTC and one second later
        assert_eq!(at(946_684_799), "19991231T235959Z");
        assert_eq!(at(946_684_800), "20000101T000000Z");
    }

    #[test]
    fn times_before_the_epoch_count_as_the_epoch() {
        assert_eq!(
            format_utc(UNIX_EPOCH - Duration::from_secs(5)),
            "19700101T000000Z"
        );
    }

    #[test]
    fn parses_names() {
        assert_eq!(
            parse_name("20261111T184948Z"),
            Some(("20261111T184948Z", 1))
        );
        assert_eq!(
            parse_name("20261111T184948Z-12"),
            Some(("20261111T184948Z", 12))
        );
        for bad in [
            "",
            "..",
            "a/b",
            "foo",
            "20261111T184948Z-",
            "20261111T184948Z-0",
            "20261111T184948Z-01",
            "20261111T184948Z-x",
            "20261111T184948",
            "2026111T1849480Z",
            "20261111T184948Zx",
        ] {
            assert_eq!(parse_name(bad), None, "{bad:?}");
        }
    }
}
