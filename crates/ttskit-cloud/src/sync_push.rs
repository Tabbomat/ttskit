//! Running a push sync: making a cloud folder match a local directory.
//!
//! [`plan_push`] looks at the cloud without changing anything and returns a
//! [`PushPlan`], which wraps the [`SyncPlan`] from [`plan_sync`]. [`push`]
//! then carries it out. It first plans again and refuses to run if the result
//! differs from the plan it was given, so what runs is what was shown.
//!
//! A real run takes one backup of the index and reuses it for every index
//! write. Uploads come first, then the deletions that prune asks for.
//!
//! The order of the steps is chosen so that an interrupted run can simply be
//! run again (plan and push once more):
//!
//! - A file is written and shared before the index mentions it. A file that
//!   is in the cloud but not in the index is found and reused by the next
//!   run, which then does not write it a second time.
//! - When a changed file replaces an older version, the old cloud file is
//!   deleted after the new one is in place and before the index swaps the two
//!   entries. If the run stops in between, the index still lists the old
//!   version (whose file is gone), the next plan sees the file as changed
//!   again, and the upload finishes the job.
//! - Deletions remove the cloud file first and the index entry second, as
//!   `delete` does.
//! - New folders are added to the index before the first file goes into them.
//! - The index is written after every step, so a stop loses at most the
//!   step that was running.
//!
//! The first failing step stops the run with [`PushError::Incomplete`].
//! Empty folders are never removed.

use std::collections::HashSet;
use std::io;
use std::time::SystemTime;

use crate::upload::resolve_folder;
use crate::{
    Action, Backup, BackupError, BackupStore, CloudBackend, CloudError, CloudFile, CloudIndex,
    Direction, IgnoreSet, IndexEntry, IndexError, LocalFile, PlanOptions, Reason, SyncPlan,
    SyncStep, UgcUrl, normalize_folder, plan_sync, sha1_hex,
};

/// Errors from planning or running a push.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PushError {
    /// The plan has conflicts and cannot be carried out.
    #[error("the plan has {count} conflict(s); rename the files involved and try again")]
    Conflicts {
        /// The number of conflicting steps.
        count: usize,
    },
    /// The cloud no longer matches the plan.
    #[error("the cloud changed since the plan was made; plan again")]
    PlanStale,
    /// A local file differs from what was scanned.
    #[error("{path} changed since it was scanned; scan again")]
    LocalChanged {
        /// The path relative to the local directory.
        path: String,
    },
    /// A local file cannot be read.
    #[error("cannot read {path}: {source}")]
    ReadLocal {
        /// The path relative to the local directory.
        path: String,
        /// Why reading failed.
        source: io::Error,
    },
    /// The run stopped part way.
    #[error(
        "push incomplete: uploaded {uploaded}, deleted {deleted} file(s), failed on {failed}: \
         {source}; the previous index is in backup {backup}; run the same push again to finish it"
    )]
    Incomplete {
        /// The backup of the index taken before the run.
        backup: String,
        /// How many files were uploaded.
        uploaded: usize,
        /// How many cloud files were deleted.
        deleted: usize,
        /// What failed: a path or `the index`.
        failed: String,
        /// The underlying error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A cloud operation failed; nothing was changed.
    #[error(transparent)]
    Cloud(#[from] CloudError),
    /// The index could not be read.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// The index could not be backed up.
    #[error(transparent)]
    Backup(#[from] BackupError),
}

/// What [`push`] will do; made by [`plan_push`].
#[derive(Debug, Clone)]
pub struct PushPlan {
    /// The sync plan, to print.
    pub sync: SyncPlan,
    local: Vec<LocalFile>,
    ignore: IgnoreSet,
}

/// Plans are equal when the sync plan and the scanned files are; the ignore set
/// has no equality of its own and only shaped those two.
impl PartialEq for PushPlan {
    fn eq(&self, other: &Self) -> bool {
        self.sync == other.sync && self.local == other.local
    }
}

impl Eq for PushPlan {}

impl PushPlan {
    /// Whether [`push`] changes nothing: no upload, rename or delete.
    pub fn is_noop(&self) -> bool {
        self.sync.is_noop()
    }
}

/// The result of [`push`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    /// The backup of the index taken before the run; `None` for a no-op.
    pub backup: Option<Backup>,
    /// The paths uploaded (a renamed file counts here).
    pub uploaded: Vec<String>,
    /// The cloud keys deleted (not those that were already gone).
    pub deleted: Vec<String>,
}

fn present_set(files: &[CloudFile]) -> HashSet<String> {
    files.iter().map(|f| f.name.to_ascii_lowercase()).collect()
}

fn join(folder: &str, rel: &str) -> String {
    if folder.is_empty() {
        rel.to_owned()
    } else if rel.is_empty() {
        folder.to_owned()
    } else {
        format!("{folder}/{rel}")
    }
}

fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Fails if the files that must be written do not fit into the quota.
fn check_quota(
    backend: &dyn CloudBackend,
    plan: &SyncPlan,
    present: &HashSet<String>,
) -> Result<(), CloudError> {
    let needed: u64 = plan
        .steps
        .iter()
        .filter(|s| matches!(s.action, Action::Upload | Action::Rename))
        .filter(|s| {
            s.cloud_key
                .as_ref()
                .is_some_and(|k| !present.contains(&k.to_ascii_lowercase()))
        })
        .map(|s| s.size.unwrap_or(0))
        .fold(0u64, u64::saturating_add);
    if needed == 0 {
        return Ok(());
    }
    let available = backend.quota()?.available;
    if needed > available {
        return Err(CloudError::QuotaExceeded {
            name: "the files to upload".to_owned(),
            needed,
            available,
        });
    }
    Ok(())
}

/// Works out what pushing `local` to the cloud folder `folder` would do,
/// without changing anything.
///
/// `local` comes from [`scan_local`](crate::scan_local). The plan may contain
/// conflicts; [`push`] refuses to run it then. Fails if the files to upload do
/// not fit into the quota.
///
/// `ignore` is the set `local` was scanned with: cloud entries it matches are
/// left alone. `ignored_local` (the scan's ignored count) is only reported in
/// the plan.
pub fn plan_push(
    backend: &dyn CloudBackend,
    local: &[LocalFile],
    folder: &str,
    prune: bool,
    ignore: &IgnoreSet,
    ignored_local: usize,
) -> Result<PushPlan, PushError> {
    let (sync, present) = make_plan(backend, local, folder, prune, ignore, ignored_local)?;
    if !sync.has_conflicts() {
        check_quota(backend, &sync, &present)?;
    }
    Ok(PushPlan {
        sync,
        local: local.to_vec(),
        ignore: ignore.clone(),
    })
}

fn make_plan(
    backend: &dyn CloudBackend,
    local: &[LocalFile],
    folder: &str,
    prune: bool,
    ignore: &IgnoreSet,
    ignored_local: usize,
) -> Result<(SyncPlan, HashSet<String>), PushError> {
    let index = CloudIndex::load(backend)?;
    let files = backend.list()?;
    let mut sync = plan_sync(
        Direction::Push,
        local,
        &index,
        &files,
        folder,
        PlanOptions { prune },
        ignore,
    );
    sync.ignored_local = ignored_local;
    Ok((sync, present_set(&files)))
}

struct StepFail {
    failed: String,
    source: Box<dyn std::error::Error + Send + Sync>,
}

fn fail(failed: &str, source: impl std::error::Error + Send + Sync + 'static) -> StepFail {
    StepFail {
        failed: failed.to_owned(),
        source: Box::new(source),
    }
}

fn find_entry(index: &CloudIndex, folder: &str, name: &str) -> Option<usize> {
    let folder = normalize_folder(folder);
    index.entries.iter().position(|e| {
        normalize_folder(&e.folder).eq_ignore_ascii_case(&folder)
            && e.name.eq_ignore_ascii_case(name)
    })
}

/// Reads and verifies a local file.
fn read_checked(
    read_local: &dyn Fn(&str) -> io::Result<Vec<u8>>,
    path: &str,
    sha1: &str,
) -> Result<Vec<u8>, PushError> {
    let data = read_local(path).map_err(|source| PushError::ReadLocal {
        path: path.to_owned(),
        source,
    })?;
    if !sha1_hex(&data).eq_ignore_ascii_case(sha1) {
        return Err(PushError::LocalChanged {
            path: path.to_owned(),
        });
    }
    Ok(data)
}

struct Run<'a> {
    backend: &'a mut dyn CloudBackend,
    backup: &'a Backup,
    index: CloudIndex,
    present: HashSet<String>,
    root: String,
    date: &'a str,
    read_local: &'a dyn Fn(&str) -> io::Result<Vec<u8>>,
    uploaded: Vec<String>,
    deleted: Vec<String>,
}

impl Run<'_> {
    fn checkpoint(&mut self) -> Result<(), StepFail> {
        self.index
            .write_after_backup(self.backend, self.backup)
            .map(|_| ())
            .map_err(|e| fail("the index", e))
    }

    /// Whether an entry other than `skip` uses the cloud file `key`.
    fn key_used_elsewhere(&self, key: &str, skip: Option<usize>) -> bool {
        self.index
            .entries
            .iter()
            .enumerate()
            .any(|(i, e)| Some(i) != skip && e.key.eq_ignore_ascii_case(key))
    }

    /// Deletes the cloud file of the entry at `at` unless another entry uses
    /// it. A file that is already gone counts as deleted.
    fn delete_file_of(&mut self, at: usize) -> Result<(), StepFail> {
        let key = self.index.entries[at].key.clone();
        if self.key_used_elsewhere(&key, Some(at)) {
            return Ok(());
        }
        if !self.present.contains(&key.to_ascii_lowercase()) {
            return Ok(());
        }
        match self.backend.delete(&key) {
            Ok(()) => {
                self.present.remove(&key.to_ascii_lowercase());
                self.deleted.push(key);
                Ok(())
            }
            Err(CloudError::NotFound { .. }) => {
                self.present.remove(&key.to_ascii_lowercase());
                Ok(())
            }
            Err(e) => Err(fail(&key, e)),
        }
    }

    fn upload(&mut self, step: &SyncStep) -> Result<(), StepFail> {
        let path = step.path.as_str();
        let (rel_folder, name) = split(path);
        let (Some(key), Some(sha1)) = (step.cloud_key.clone(), step.sha1.clone()) else {
            return Err(fail(
                path,
                io::Error::other("the plan step has no cloud name"),
            ));
        };
        let size = step.size.unwrap_or(0);
        let data = read_checked(self.read_local, path, &sha1).map_err(|e| fail(path, e))?;
        let full_folder = join(&self.root, rel_folder);
        let (display_folder, new_folders) = resolve_folder(&self.index, &full_folder);
        let game_folder = display_folder.replace('/', "\\");

        let add_folders = !new_folders.is_empty();
        for folder in new_folders {
            let wanted = normalize_folder(&folder).to_ascii_lowercase();
            if !self
                .index
                .folders
                .iter()
                .any(|f| normalize_folder(f).to_ascii_lowercase() == wanted)
            {
                self.index.folders.push(folder);
            }
        }
        // Folders first: an entry never exists in a folder the index lacks.
        if add_folders {
            self.checkpoint()?;
        }

        let file_existed = self.present.contains(&key.to_ascii_lowercase());
        if !file_existed {
            self.backend.write(&key, &data).map_err(|e| fail(path, e))?;
            self.present.insert(key.to_ascii_lowercase());
        }
        drop(data);

        let same_key = self
            .index
            .entries
            .iter()
            .position(|e| e.key.eq_ignore_ascii_case(&key));
        let known_url = same_key.and_then(|i| self.index.entries[i].url.clone());
        let url = if known_url.is_none() || !file_existed {
            let handle = self.backend.share(&key).map_err(|e| fail(path, e))?;
            Some(
                UgcUrl {
                    handle,
                    sha1: sha1.clone(),
                }
                .to_string(),
            )
        } else {
            known_url
        };

        // The older version of this path, if it has another content.
        let older = find_entry(&self.index, &full_folder, name)
            .filter(|&i| !self.index.entries[i].key.eq_ignore_ascii_case(&key));
        if let Some(old) = older {
            if step.reason == Reason::Changed {
                self.delete_file_of(old)?;
            }
            self.index.entries.remove(old);
        }

        match self
            .index
            .entries
            .iter_mut()
            .find(|e| e.key.eq_ignore_ascii_case(&key))
        {
            Some(entry) => {
                entry.url.clone_from(&url);
                entry.size.get_or_insert(size);
                entry.date.get_or_insert_with(|| self.date.to_owned());
            }
            None => self.index.entries.push(IndexEntry::new(
                key,
                name,
                url,
                Some(size),
                Some(self.date.to_owned()),
                game_folder,
            )),
        }
        self.checkpoint()?;
        self.uploaded.push(path.to_owned());
        Ok(())
    }

    /// Removes the file at `path` (target spelling) from the cloud and the
    /// index.
    fn remove(&mut self, path: &str) -> Result<(), StepFail> {
        let (rel_folder, name) = split(path);
        let full_folder = join(&self.root, rel_folder);
        let Some(at) = find_entry(&self.index, &full_folder, name) else {
            return Ok(());
        };
        self.delete_file_of(at)?;
        self.index.entries.remove(at);
        self.checkpoint()
    }
}

/// Carries out `plan`.
///
/// `read_local` returns the content of a local file given its path relative to
/// the scanned directory (`/` separated); `date` is stored in new index
/// entries. Nothing is changed, and no backup taken, if the plan has
/// conflicts, no longer matches the cloud, or a local file cannot be read or
/// has changed. Deletions only happen if the plan was made with prune.
///
/// `on_step` is called after each step that changed something, with that step.
/// After a failure further down the line, [`PushError::Incomplete`] is
/// returned; planning and pushing again finishes the job.
pub fn push(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    plan: &PushPlan,
    read_local: &dyn Fn(&str) -> io::Result<Vec<u8>>,
    now: SystemTime,
    date: &str,
    on_step: &mut dyn FnMut(&SyncStep),
) -> Result<PushOutcome, PushError> {
    if plan.sync.has_conflicts() {
        return Err(PushError::Conflicts {
            count: plan.sync.count(Action::Conflict),
        });
    }
    // Plan again: what runs must be what was shown.
    let (again, present) = make_plan(
        backend,
        &plan.local,
        &plan.sync.cloud_folder,
        plan.sync.options.prune,
        &plan.ignore,
        plan.sync.ignored_local,
    )?;
    if again != plan.sync {
        return Err(PushError::PlanStale);
    }
    if plan.is_noop() {
        return Ok(PushOutcome {
            backup: None,
            uploaded: Vec::new(),
            deleted: Vec::new(),
        });
    }
    check_quota(backend, &plan.sync, &present)?;
    for step in &plan.sync.steps {
        if matches!(step.action, Action::Upload | Action::Rename)
            && let Some(sha1) = &step.sha1
        {
            read_checked(read_local, &step.path, sha1)?;
        }
    }

    let index = CloudIndex::load(backend)?;
    let backup = store.create(backend, now)?;
    let mut run = Run {
        backend,
        backup: &backup,
        index,
        present,
        root: plan.sync.cloud_folder.clone(),
        date,
        read_local,
        uploaded: Vec::new(),
        deleted: Vec::new(),
    };

    let result = (|| -> Result<(), StepFail> {
        for step in &plan.sync.steps {
            if matches!(step.action, Action::Upload | Action::Rename) {
                run.upload(step)?;
                on_step(step);
            }
        }
        for step in &plan.sync.steps {
            let old = match step.action {
                Action::Delete => Some(step.path.as_str()),
                Action::Rename => step.from.as_deref(),
                _ => None,
            };
            if let Some(old) = old {
                run.remove(old)?;
                if step.action == Action::Delete {
                    on_step(step);
                }
            }
        }
        Ok(())
    })();
    let Run {
        uploaded, deleted, ..
    } = run;
    match result {
        Ok(()) => Ok(PushOutcome {
            backup: Some(backup),
            uploaded,
            deleted,
        }),
        Err(StepFail { failed, source }) => Err(PushError::Incomplete {
            backup: backup.name.clone(),
            uploaded: uploaded.len(),
            deleted: deleted.len(),
            failed,
            source,
        }),
    }
}

#[cfg(test)]
#[path = "sync_push_tests.rs"]
mod tests;
