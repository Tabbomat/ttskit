//! Running a pull sync: making a local directory match a cloud folder.
//!
//! [`plan_pull`] looks at the cloud without changing anything and returns a
//! [`PullPlan`], which wraps the [`SyncPlan`] from [`plan_sync`]. [`pull`]
//! then carries it out. It first plans again and refuses to run if the result
//! differs from the plan it was given, so what runs is what was shown.
//!
//! A pull never changes the cloud: no index backup is taken and nothing is
//! written, shared or deleted there. Local files are changed through
//! [`LocalDir`]; [`FsDir`] is the real one.
//!
//! Before anything is changed, every target path is checked to be a plain
//! relative path that is valid on Windows, and every local file that would be
//! overwritten or moved to the recycle bin is read again to see that it still
//! has the content that was scanned. Then the downloads run, then the
//! deletions that prune asks for. Each download is checked against the SHA1 in
//! the plan before it is written, and files are written atomically.
//!
//! Downloads come before deletions, so an interrupted run can simply be run
//! again (plan and pull once more). The first failing step stops the run with
//! [`PullError::Incomplete`]. Empty directories are never removed.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::{
    Action, CloudBackend, CloudError, CloudIndex, Direction, IgnoreSet, IndexError, LocalFile,
    PlanOptions, Reason, SyncPlan, SyncStep, plan_sync, sha1_hex,
};

/// Errors from planning or running a pull.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PullError {
    /// The plan has conflicts and cannot be carried out.
    #[error("the plan has {count} conflict(s); rename the files involved and try again")]
    Conflicts {
        /// The number of conflicting steps.
        count: usize,
    },
    /// The cloud no longer matches the plan.
    #[error("the cloud changed since the plan was made; plan again")]
    PlanStale,
    /// A path from the cloud cannot be used as a local path.
    #[error("{path} is not a safe local path; nothing was changed")]
    UnsafePath {
        /// The path relative to the local directory.
        path: String,
    },
    /// A local file differs from what was scanned.
    #[error("{path} changed since it was scanned; scan again")]
    LocalChanged {
        /// The path relative to the local directory.
        path: String,
    },
    /// A downloaded file does not have the content the index promises.
    #[error(
        "the cloud file for {path} does not match its recorded SHA1; nothing was written for it"
    )]
    CloudMismatch {
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
        "pull incomplete: downloaded {downloaded}, deleted {deleted} file(s), failed on \
         {failed}: {source}; run the same pull again to finish it"
    )]
    Incomplete {
        /// How many files were downloaded.
        downloaded: usize,
        /// How many local files were deleted.
        deleted: usize,
        /// The path that failed.
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
}

/// What [`pull`] will do; made by [`plan_pull`].
#[derive(Debug, Clone)]
pub struct PullPlan {
    /// The sync plan, to print.
    pub sync: SyncPlan,
    local: Vec<LocalFile>,
    ignore: IgnoreSet,
}

/// Plans are equal when the sync plan and the scanned files are; the ignore set
/// has no equality of its own and only shaped those two.
impl PartialEq for PullPlan {
    fn eq(&self, other: &Self) -> bool {
        self.sync == other.sync && self.local == other.local
    }
}

impl Eq for PullPlan {}

impl PullPlan {
    /// Whether [`pull`] changes nothing: no download, rename or delete.
    pub fn is_noop(&self) -> bool {
        self.sync.is_noop()
    }
}

/// The result of [`pull`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullOutcome {
    /// The paths written (a renamed file counts here).
    pub downloaded: Vec<String>,
    /// The local paths moved to the recycle bin (the old path of a rename
    /// counts here).
    pub deleted: Vec<String>,
}

/// The local directory a pull writes to. Paths are relative to the synced
/// directory and use `/`.
pub trait LocalDir {
    /// Reads a file.
    fn read(&self, path: &str) -> io::Result<Vec<u8>>;
    /// Writes a file, replacing an existing one and creating missing parent
    /// directories.
    fn write(&mut self, path: &str, data: &[u8]) -> io::Result<()>;
    /// Moves a file to the recycle bin.
    fn trash(&mut self, path: &str) -> io::Result<()>;
}

/// A [`LocalDir`] on the file system.
#[derive(Debug, Clone)]
pub struct FsDir {
    root: PathBuf,
}

impl FsDir {
    /// Uses the directory `root`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn resolve(&self, path: &str) -> PathBuf {
        let mut full = self.root.clone();
        full.extend(path.split('/'));
        full
    }
}

impl LocalDir for FsDir {
    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        fs::read(self.resolve(path))
    }

    /// Writes to a temporary file in the same directory, then renames it over
    /// the target, so a stop never leaves a half-written file.
    fn write(&mut self, path: &str, data: &[u8]) -> io::Result<()> {
        let target = self.resolve(path);
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("path has no parent directory"))?;
        fs::create_dir_all(parent)?;
        let name = target
            .file_name()
            .ok_or_else(|| io::Error::other("path has no file name"))?;
        let mut temp_name = std::ffi::OsString::from(".");
        temp_name.push(name);
        temp_name.push(".ttskit-tmp");
        let temp = parent.join(temp_name);
        let result = fs::write(&temp, data).and_then(|()| fs::rename(&temp, &target));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn trash(&mut self, path: &str) -> io::Result<()> {
        trash::delete(self.resolve(path)).map_err(io::Error::other)
    }
}

fn make_plan(
    backend: &dyn CloudBackend,
    local: &[LocalFile],
    folder: &str,
    prune: bool,
    ignore: &IgnoreSet,
    ignored_local: usize,
) -> Result<SyncPlan, PullError> {
    let index = CloudIndex::load(backend)?;
    let files = backend.list()?;
    let mut sync = plan_sync(
        Direction::Pull,
        local,
        &index,
        &files,
        folder,
        PlanOptions { prune },
        ignore,
    );
    sync.ignored_local = ignored_local;
    Ok(sync)
}

/// Works out what pulling the cloud folder `folder` into the directory
/// described by `local` would do, without changing anything.
///
/// `local` comes from [`scan_local`](crate::scan_local). The plan may contain
/// conflicts; [`pull`] refuses to run it then.
///
/// `ignore` is the set `local` was scanned with: cloud entries it matches are
/// not downloaded, and ignored local files are not in `local`, so prune
/// cannot touch them. `ignored_local` (the scan's ignored count) is only
/// reported in the plan.
pub fn plan_pull(
    backend: &dyn CloudBackend,
    local: &[LocalFile],
    folder: &str,
    prune: bool,
    ignore: &IgnoreSet,
    ignored_local: usize,
) -> Result<PullPlan, PullError> {
    Ok(PullPlan {
        sync: make_plan(backend, local, folder, prune, ignore, ignored_local)?,
        local: local.to_vec(),
        ignore: ignore.clone(),
    })
}

/// Whether `path` can safely be created below the local directory on every
/// supported system.
fn is_safe_path(path: &str) -> bool {
    !path.is_empty() && path.split('/').all(is_safe_segment)
}

fn is_safe_segment(seg: &str) -> bool {
    if seg.is_empty() || seg.starts_with('.') || seg.ends_with('.') || seg.ends_with(' ') {
        return false;
    }
    if seg
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\'))
    {
        return false;
    }
    let stem = seg.split('.').next().unwrap_or(seg).trim_end();
    let upper = stem.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|p| {
            upper
                .strip_prefix(p)
                .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
        });
    !reserved
}

fn find_local<'a>(local: &'a [LocalFile], path: &str) -> Option<&'a LocalFile> {
    local
        .iter()
        .find(|f| f.path == path)
        .or_else(|| local.iter().find(|f| f.path.eq_ignore_ascii_case(path)))
}

/// Reads a local file and checks it still has the scanned content.
fn verify_local(dir: &dyn LocalDir, path: &str, sha1: &str) -> Result<(), PullError> {
    match dir.read(path) {
        Ok(data) if sha1_hex(&data).eq_ignore_ascii_case(sha1) => Ok(()),
        Ok(_) => Err(PullError::LocalChanged {
            path: path.to_owned(),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(PullError::LocalChanged {
            path: path.to_owned(),
        }),
        Err(source) => Err(PullError::ReadLocal {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Where a download is written: the existing spelling of a changed file.
fn target_path(plan: &PullPlan, step: &SyncStep) -> String {
    if step.reason == Reason::Changed
        && let Some(existing) = find_local(&plan.local, &step.path)
    {
        return existing.path.clone();
    }
    step.path.clone()
}

/// Everything that must hold before the first change.
fn validate(dir: &dyn LocalDir, plan: &PullPlan) -> Result<(), PullError> {
    for step in &plan.sync.steps {
        match step.action {
            Action::Download | Action::Rename => {
                if !is_safe_path(&step.path) {
                    return Err(PullError::UnsafePath {
                        path: step.path.clone(),
                    });
                }
                if step.reason == Reason::Changed
                    && let Some(existing) = find_local(&plan.local, &step.path)
                {
                    verify_local(dir, &existing.path, &existing.sha1)?;
                }
            }
            _ => {}
        }
        if let Some((path, sha1)) = removal(plan, step) {
            verify_local(dir, &path, &sha1)?;
        }
    }
    Ok(())
}

/// The local file a step moves to the recycle bin and its scanned SHA1.
fn removal(plan: &PullPlan, step: &SyncStep) -> Option<(String, String)> {
    match step.action {
        Action::Delete => Some((step.path.clone(), step.sha1.clone()?)),
        Action::Rename => {
            let from = step.from.as_deref()?;
            let file = find_local(&plan.local, from)?;
            Some((file.path.clone(), file.sha1.clone()))
        }
        _ => None,
    }
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

fn download(
    backend: &dyn CloudBackend,
    dir: &mut dyn LocalDir,
    plan: &PullPlan,
    step: &SyncStep,
) -> Result<(), StepFail> {
    let path = step.path.as_str();
    let (Some(key), Some(sha1)) = (&step.cloud_key, &step.sha1) else {
        return Err(fail(
            path,
            io::Error::other("the plan step has no cloud name or SHA1"),
        ));
    };
    let data = backend.read(key).map_err(|e| fail(path, e))?;
    if !sha1_hex(&data).eq_ignore_ascii_case(sha1) {
        return Err(fail(
            path,
            PullError::CloudMismatch {
                path: path.to_owned(),
            },
        ));
    }
    let target = target_path(plan, step);
    dir.write(&target, &data).map_err(|e| fail(path, e))
}

/// Carries out `plan`.
///
/// Nothing is changed if the plan has conflicts, no longer matches the cloud,
/// has a target path that is not safe, or a local file that would be
/// overwritten or removed has changed since it was scanned. Deletions only
/// happen if the plan was made with prune. The cloud is only read.
///
/// `on_step` is called after each step that changed something, with that step.
/// After a failure further down the line, [`PullError::Incomplete`] is
/// returned; planning and pulling again finishes the job.
pub fn pull(
    backend: &dyn CloudBackend,
    dir: &mut dyn LocalDir,
    plan: &PullPlan,
    on_step: &mut dyn FnMut(&SyncStep),
) -> Result<PullOutcome, PullError> {
    if plan.sync.has_conflicts() {
        return Err(PullError::Conflicts {
            count: plan.sync.count(Action::Conflict),
        });
    }
    // Plan again: what runs must be what was shown.
    let again = make_plan(
        backend,
        &plan.local,
        &plan.sync.cloud_folder,
        plan.sync.options.prune,
        &plan.ignore,
        plan.sync.ignored_local,
    )?;
    if again != plan.sync {
        return Err(PullError::PlanStale);
    }
    let mut outcome = PullOutcome {
        downloaded: Vec::new(),
        deleted: Vec::new(),
    };
    if plan.is_noop() {
        return Ok(outcome);
    }
    validate(dir, plan)?;

    let result = (|| -> Result<(), StepFail> {
        for step in &plan.sync.steps {
            if matches!(step.action, Action::Download | Action::Rename) {
                download(backend, dir, plan, step)?;
                outcome.downloaded.push(step.path.clone());
                on_step(step);
            }
        }
        for step in &plan.sync.steps {
            if let Some((path, _)) = removal(plan, step) {
                dir.trash(&path).map_err(|e| fail(&path, e))?;
                outcome.deleted.push(path);
                if step.action == Action::Delete {
                    on_step(step);
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(outcome),
        Err(StepFail { failed, source }) => Err(PullError::Incomplete {
            downloaded: outcome.downloaded.len(),
            deleted: outcome.deleted.len(),
            failed,
            source,
        }),
    }
}

#[cfg(test)]
#[path = "sync_pull_tests.rs"]
mod tests;
