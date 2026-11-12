//! Uploading a file to the cloud and sharing it, and looking entries up.
//!
//! An upload has two steps. [`plan_upload`] looks at the cloud without
//! changing anything and says what [`upload`] would do; [`upload`] then
//! writes the file, shares it, and records it in the index (through
//! [`CloudIndex::save`], which takes a backup first).
//!
//! The cloud file is named `<SHA1>_<name>` after its content, so uploading the
//! same content under the same name twice finds the existing file and entry.
//! If an upload is interrupted after the file was written, running the same
//! upload again finishes it without writing the file a second time.

use std::collections::HashSet;
use std::fmt;
use std::time::SystemTime;

use sha1::{Digest, Sha1};

use crate::{
    Backup, BackupError, BackupStore, CloudBackend, CloudError, CloudIndex, IndexEntry, IndexError,
    UgcUrl, normalize_folder,
};

/// The SHA1 of `data` as 40 uppercase hexadecimal digits.
pub fn sha1_hex(data: &[u8]) -> String {
    use fmt::Write as _;
    Sha1::digest(data)
        .iter()
        .fold(String::with_capacity(40), |mut out, b| {
            let _ = write!(out, "{b:02X}");
            out
        })
}

/// The cloud key (and cloud file name) for content with the given `sha1`
/// and display `name`: `<SHA1>_<name>`.
pub fn cloud_key(sha1: &str, name: &str) -> String {
    format!("{sha1}_{name}")
}

/// The part of an upload that did not finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadStage {
    /// Sharing the cloud file failed.
    Share,
    /// Updating the index failed.
    Index,
}

impl fmt::Display for UploadStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UploadStage::Share => "sharing the file",
            UploadStage::Index => "updating the index",
        })
    }
}

/// Errors from planning or running an upload.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UploadError {
    /// The same content is already in the cloud under the same name, but in
    /// another folder.
    #[error(
        "{key} is already in the cloud in folder {}{}; \
         upload it under another name with --name",
        folder_label(folder),
        url.as_deref().map(|u| format!(" ({u})")).unwrap_or_default()
    )]
    Collision {
        /// The cloud key.
        key: String,
        /// The folder of the existing entry, `/`-separated; empty for the root.
        folder: String,
        /// The URL of the existing entry, if it has one.
        url: Option<String>,
    },
    /// The upload stopped part way. The cloud file exists but is not fully
    /// recorded in the index.
    #[error(
        "upload incomplete: the cloud file {key} exists but is not in the index \
         (failed while {stage}: {source}); run the same upload again to finish it"
    )]
    Incomplete {
        /// The cloud key of the file.
        key: String,
        /// What failed.
        stage: UploadStage,
        /// The underlying error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A cloud operation failed; nothing was left half done.
    #[error(transparent)]
    Cloud(#[from] CloudError),
    /// The index could not be read.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// The index could not be backed up or written.
    #[error(transparent)]
    Backup(#[from] BackupError),
}

fn folder_label(folder: &str) -> String {
    if folder.is_empty() {
        "/ (root)".to_owned()
    } else {
        format!("{folder:?}")
    }
}

/// Something worth telling the user about a planned upload.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UploadWarning {
    /// Another file with the same display name but different content is in
    /// the target folder.
    SameNameInFolder {
        /// The cloud key of the other file.
        key: String,
    },
}

impl fmt::Display for UploadWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UploadWarning::SameNameInFolder { key } => write!(
                f,
                "the folder already has a file with the same name but different content ({key})"
            ),
        }
    }
}

/// What [`upload`] will do; made by [`plan_upload`].
#[derive(Clone)]
pub struct UploadPlan {
    /// The cloud key, `<SHA1>_<name>`.
    pub key: String,
    /// The display name.
    pub name: String,
    /// The SHA1 of the content, uppercase.
    pub sha1: String,
    /// Size in bytes.
    pub size: u64,
    /// The target folder in the game's form (`\` separated); empty for the root.
    pub folder: String,
    /// The target folder, `/` separated; empty for the root.
    pub display_folder: String,
    /// Folders to add to `CloudFolder.bson` in the game's form, ancestors first.
    pub new_folders: Vec<String>,
    /// Whether the file has to be written to the cloud.
    pub write: bool,
    /// Whether the file has to be shared.
    pub share: bool,
    /// Whether a new index entry has to be added.
    pub add_entry: bool,
    /// The URL of the entry that already exists, if any.
    pub existing_url: Option<String>,
    /// Things the user may want to know.
    pub warnings: Vec<UploadWarning>,
    data: Vec<u8>,
}

impl fmt::Debug for UploadPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadPlan")
            .field("key", &self.key)
            .field("name", &self.name)
            .field("sha1", &self.sha1)
            .field("size", &self.size)
            .field("folder", &self.folder)
            .field("display_folder", &self.display_folder)
            .field("new_folders", &self.new_folders)
            .field("write", &self.write)
            .field("share", &self.share)
            .field("add_entry", &self.add_entry)
            .field("existing_url", &self.existing_url)
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

impl UploadPlan {
    /// Whether there is nothing to write, share or add, so [`upload`] changes
    /// nothing.
    pub fn is_noop(&self) -> bool {
        !self.write && !self.share && !self.add_entry && self.new_folders.is_empty()
    }
}

/// The result of [`upload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadOutcome {
    /// The download URL of the file.
    pub url: String,
    /// Whether the file was written.
    pub wrote: bool,
    /// Whether the file was shared.
    pub shared: bool,
    /// The backup taken before the index was written, if it was written.
    pub backup: Option<Backup>,
    /// The index files that were written.
    pub index_changed: Vec<&'static str>,
}

fn invalid_name(name: &str, reason: &str) -> UploadError {
    UploadError::Cloud(CloudError::InvalidName {
        name: name.to_owned(),
        reason: reason.to_owned(),
    })
}

fn same_folder(a: &str, b: &str) -> bool {
    normalize_folder(a).eq_ignore_ascii_case(&normalize_folder(b))
}

/// Resolves `folder` level by level against the folders of `index`, keeping
/// existing spellings (ASCII case-insensitive). Returns the folder as
/// `/`-separated display path and the folders (all missing ancestors included,
/// in the game's `\` form, ancestors first) that are not in `CloudFolder.bson`.
pub(crate) fn resolve_folder(index: &CloudIndex, folder: &str) -> (String, Vec<String>) {
    let known = index.all_folders();
    let mut display_folder = String::new();
    let mut levels = Vec::new();
    for seg in normalize_folder(folder)
        .split('/')
        .filter(|s| !s.is_empty())
    {
        if !display_folder.is_empty() {
            display_folder.push('/');
        }
        display_folder.push_str(seg);
        if let Some(existing) = known
            .iter()
            .find(|k| k.eq_ignore_ascii_case(&display_folder))
        {
            display_folder.clone_from(existing);
        }
        levels.push(display_folder.clone());
    }
    let listed: HashSet<String> = index
        .folders
        .iter()
        .map(|f| normalize_folder(f).to_ascii_lowercase())
        .collect();
    let new_folders = levels
        .into_iter()
        .filter(|l| !listed.contains(&l.to_ascii_lowercase()))
        .map(|l| l.replace('/', "\\"))
        .collect();
    (display_folder, new_folders)
}

/// Works out what uploading `data` as `name` into `folder` would do, without
/// changing anything.
///
/// `name` is the display name. `folder` may use `/` or `\`; an empty string
/// or `/` is the root. Folders that already exist (compared ASCII
/// case-insensitively) keep their spelling.
pub fn plan_upload(
    backend: &dyn CloudBackend,
    name: &str,
    data: &[u8],
    folder: &str,
) -> Result<UploadPlan, UploadError> {
    if name.is_empty() {
        return Err(invalid_name(name, "name is empty"));
    }
    if name.contains(['/', '\\', '\0']) {
        return Err(invalid_name(
            name,
            "name must not contain '/', '\\' or NUL characters",
        ));
    }
    let sha1 = sha1_hex(data);
    let key = cloud_key(&sha1, name);
    let size = u64::try_from(data.len()).unwrap_or(u64::MAX);

    let index = CloudIndex::load(backend)?;
    let present: HashSet<String> = backend
        .list()?
        .into_iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();
    let file_exists = present.contains(&key.to_ascii_lowercase());

    let (display_folder, new_folders) = resolve_folder(&index, folder);
    let game_folder = display_folder.replace('/', "\\");

    let same_key: Vec<&IndexEntry> = index
        .entries
        .iter()
        .filter(|e| e.key.eq_ignore_ascii_case(&key))
        .collect();
    if let Some(other) = same_key
        .iter()
        .find(|e| !same_folder(&e.folder, &display_folder))
    {
        return Err(UploadError::Collision {
            key,
            folder: normalize_folder(&other.folder),
            url: other.url.clone(),
        });
    }

    // A missing file behind an existing entry is written again, and shared
    // again because the old URL no longer points to anything.
    let (write, share, add_entry, existing_url) = match same_key.first() {
        Some(entry) => (
            !file_exists,
            entry.url.is_none() || !file_exists,
            false,
            entry.url.clone(),
        ),
        None if file_exists => (false, true, true, None),
        None => (true, true, true, None),
    };

    let warnings = index
        .entries
        .iter()
        .filter(|e| {
            same_folder(&e.folder, &display_folder)
                && e.name.eq_ignore_ascii_case(name)
                && !e.key.eq_ignore_ascii_case(&key)
        })
        .map(|e| UploadWarning::SameNameInFolder { key: e.key.clone() })
        .collect();

    if write {
        let available = backend.quota()?.available;
        if size > available {
            return Err(UploadError::Cloud(CloudError::QuotaExceeded {
                name: key,
                needed: size,
                available,
            }));
        }
    }

    Ok(UploadPlan {
        key,
        name: name.to_owned(),
        sha1,
        size,
        folder: game_folder,
        display_folder,
        new_folders,
        write,
        share,
        add_entry,
        existing_url,
        warnings,
        data: data.to_vec(),
    })
}

/// Carries out `plan`: writes the file, shares it, then adds or updates the
/// index entry and any new folders, after a backup of the index.
///
/// `date` is stored in the entry as given. If the file was written or is not
/// yet indexed and a later step fails, [`UploadError::Incomplete`] is
/// returned; running [`plan_upload`] and [`upload`] again finishes the job
/// without writing the file again.
pub fn upload(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    plan: &UploadPlan,
    now: SystemTime,
    date: &str,
) -> Result<UploadOutcome, UploadError> {
    if plan.is_noop() {
        return Ok(UploadOutcome {
            url: plan.existing_url.clone().unwrap_or_default(),
            wrote: false,
            shared: false,
            backup: None,
            index_changed: Vec::new(),
        });
    }
    if plan.write {
        backend.write(&plan.key, &plan.data)?;
    }
    let partial = plan.write || plan.add_entry;
    let incomplete =
        |stage, source: Box<dyn std::error::Error + Send + Sync>| UploadError::Incomplete {
            key: plan.key.clone(),
            stage,
            source,
        };

    let mut url = plan.existing_url.clone();
    if plan.share {
        match backend.share(&plan.key) {
            Ok(handle) => {
                url = Some(
                    UgcUrl {
                        handle,
                        sha1: plan.sha1.clone(),
                    }
                    .to_string(),
                );
            }
            Err(e) if partial => return Err(incomplete(UploadStage::Share, Box::new(e))),
            Err(e) => return Err(e.into()),
        }
    }

    let update = |backend: &mut dyn CloudBackend| -> Result<_, UploadError> {
        let mut index = CloudIndex::load(backend)?;
        match index
            .entries
            .iter_mut()
            .find(|e| e.key.eq_ignore_ascii_case(&plan.key))
        {
            Some(entry) => {
                if url.is_some() {
                    entry.url.clone_from(&url);
                }
                if plan.add_entry {
                    entry.size.get_or_insert(plan.size);
                    entry.date.get_or_insert_with(|| date.to_owned());
                }
            }
            None => index.entries.push(IndexEntry::new(
                plan.key.clone(),
                plan.name.clone(),
                url.clone(),
                Some(plan.size),
                Some(date.to_owned()),
                plan.folder.clone(),
            )),
        }
        for folder in &plan.new_folders {
            let wanted = normalize_folder(folder).to_ascii_lowercase();
            if !index
                .folders
                .iter()
                .any(|f| normalize_folder(f).to_ascii_lowercase() == wanted)
            {
                index.folders.push(folder.clone());
            }
        }
        Ok(index.save(backend, store, now)?)
    };
    let saved = match update(backend) {
        Ok(saved) => saved,
        Err(e) if partial => {
            let source: Box<dyn std::error::Error + Send + Sync> = match e {
                UploadError::Index(e) => Box::new(e),
                UploadError::Backup(e) => Box::new(e),
                other => Box::new(std::io::Error::other(other.to_string())),
            };
            return Err(incomplete(UploadStage::Index, source));
        }
        Err(e) => return Err(e),
    };

    Ok(UploadOutcome {
        url: url.unwrap_or_default(),
        wrote: plan.write,
        shared: plan.share,
        backup: saved.backup,
        index_changed: saved.changed,
    })
}

/// Finds index entries by a user-supplied query, in index order.
///
/// A query containing `/` or `\` is a path: the last segment is the display
/// name and the rest the folder. Otherwise the query matches the display name
/// or the key. All comparisons ignore ASCII case. Several entries may match.
pub fn find_entries<'a>(index: &'a CloudIndex, query: &str) -> Vec<&'a IndexEntry> {
    if let Some(pos) = query.rfind(['/', '\\']) {
        let (folder, name) = (&query[..pos], &query[pos + 1..]);
        index
            .entries
            .iter()
            .filter(|e| same_folder(&e.folder, folder) && e.name.eq_ignore_ascii_case(name))
            .collect()
    } else {
        index
            .entries
            .iter()
            .filter(|e| e.name.eq_ignore_ascii_case(query) || e.key.eq_ignore_ascii_case(query))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, MemoryCloud, Op};
    use std::time::UNIX_EPOCH;

    const DATE: &str = "1/2/2026 3:04:05 PM";

    fn seed(cloud: MemoryCloud, index: &CloudIndex) -> MemoryCloud {
        cloud
            .with_file(CLOUD_INFO_FILE, index.encode_info().unwrap())
            .with_file(CLOUD_FOLDER_FILE, index.encode_folders().unwrap())
    }

    fn entry(key: &str, name: &str, url: Option<&str>, folder: &str) -> IndexEntry {
        IndexEntry::new(key, name, url.map(Into::into), Some(5), None, folder)
    }

    fn run(
        cloud: &mut MemoryCloud,
        dir: &tempfile::TempDir,
        name: &str,
        data: &[u8],
        folder: &str,
    ) -> Result<UploadOutcome, UploadError> {
        let plan = plan_upload(cloud, name, data, folder)?;
        upload(
            cloud,
            &BackupStore::new(dir.path()),
            &plan,
            UNIX_EPOCH,
            DATE,
        )
    }

    fn index_of(cloud: &MemoryCloud) -> CloudIndex {
        CloudIndex::load(cloud).unwrap()
    }

    fn key_of(data: &[u8], name: &str) -> String {
        cloud_key(&sha1_hex(data), name)
    }

    #[test]
    fn sha1_known_vector() {
        assert_eq!(sha1_hex(b"abc"), "A9993E364706816ABA3E25717850C26C9CD0D89D");
        assert_eq!(cloud_key("AB", "x.png"), "AB_x.png");
    }

    #[test]
    fn uploads_into_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = MemoryCloud::new();
        let out = run(&mut cloud, &dir, "a.png", b"hello", "/").unwrap();
        let key = key_of(b"hello", "a.png");
        let handle = cloud.handle(&key).unwrap();
        assert_eq!(
            out.url,
            format!(
                "https://steamusercontent-a.akamaihd.net/ugc/{handle}/{}/",
                sha1_hex(b"hello")
            )
        );
        assert!(out.wrote && out.shared);
        assert_eq!(cloud.contents(&key).unwrap(), b"hello");
        let index = index_of(&cloud);
        assert_eq!(index.entries.len(), 1);
        let e = &index.entries[0];
        assert_eq!(e.key, key);
        assert_eq!(e.name, "a.png");
        assert_eq!(e.url.as_deref(), Some(out.url.as_str()));
        assert_eq!(e.size, Some(5));
        assert_eq!(e.date.as_deref(), Some(DATE));
        assert_eq!(e.folder, "");
        assert!(index.folders.is_empty());
    }

    #[test]
    fn creates_nested_folders_with_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = MemoryCloud::new();
        let plan = plan_upload(&cloud, "a.png", b"x", "A/B").unwrap();
        assert_eq!(plan.new_folders, ["A", "A\\B"]);
        assert_eq!(plan.folder, "A\\B");
        assert_eq!(plan.display_folder, "A/B");
        run(&mut cloud, &dir, "a.png", b"x", "A\\B").unwrap();
        let index = index_of(&cloud);
        assert_eq!(index.folders, ["A", "A\\B"]);
        assert_eq!(index.entries[0].folder, "A\\B");
    }

    #[test]
    fn reuses_existing_folder_spelling_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = CloudIndex::empty();
        index.folders = vec!["RftG".into(), "RftG\\dice".into()];
        let mut cloud = seed(MemoryCloud::new(), &index);
        let plan = plan_upload(&cloud, "a.png", b"x", "rftg/DICE/new").unwrap();
        assert_eq!(plan.folder, "RftG\\dice\\new");
        assert_eq!(plan.new_folders, ["RftG\\dice\\new"]);
        run(&mut cloud, &dir, "a.png", b"x", "rftg/DICE/new").unwrap();
        assert_eq!(
            index_of(&cloud).folders,
            ["RftG", "RftG\\dice", "RftG\\dice\\new"]
        );
    }

    #[test]
    fn folder_known_only_through_an_entry_is_listed() {
        let mut index = CloudIndex::empty();
        index.entries = vec![entry("AA_o.png", "o.png", None, "Old\\Sub")];
        let cloud = seed(MemoryCloud::new(), &index);
        let plan = plan_upload(&cloud, "a.png", b"x", "old/sub").unwrap();
        assert_eq!(plan.folder, "Old\\Sub");
        assert_eq!(plan.new_folders, ["Old", "Old\\Sub"]);
    }

    #[test]
    fn backup_is_taken_before_the_index_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = CloudIndex::empty();
        index.entries = vec![entry("AA_o.png", "o.png", Some("u"), "")];
        let mut cloud = seed(MemoryCloud::new(), &index);
        let before = cloud.contents(CLOUD_INFO_FILE).unwrap();
        let out = run(&mut cloud, &dir, "a.png", b"x", "").unwrap();
        let backup = out.backup.unwrap();
        assert_eq!(backup.read(CLOUD_INFO_FILE).unwrap().unwrap(), before);
        assert!(out.index_changed.contains(&CLOUD_INFO_FILE));
        assert_eq!(index_of(&cloud).entries.len(), 2);
    }

    #[test]
    fn same_content_same_folder_already_shared_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_of(b"x", "a.png");
        let mut index = CloudIndex::empty();
        index.folders = vec!["F".into()];
        index.entries = vec![entry(&key, "a.png", Some("https://u/"), "F")];
        let mut cloud = seed(MemoryCloud::new().with_file(&key, "x"), &index);
        let plan = plan_upload(&cloud, "a.png", b"x", "f").unwrap();
        assert!(plan.is_noop());
        assert_eq!(plan.existing_url.as_deref(), Some("https://u/"));
        let out = run(&mut cloud, &dir, "a.png", b"x", "f").unwrap();
        assert_eq!(out.url, "https://u/");
        assert!(!out.wrote && !out.shared && out.backup.is_none());
        assert!(cloud.mutating_calls().is_empty());
    }

    #[test]
    fn same_content_without_url_only_shares() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_of(b"x", "a.png");
        let mut index = CloudIndex::empty();
        index.entries = vec![entry(&key, "a.png", None, "")];
        let mut cloud = seed(MemoryCloud::new().with_file(&key, "x"), &index);
        let plan = plan_upload(&cloud, "a.png", b"x", "").unwrap();
        assert!(!plan.write && plan.share && !plan.add_entry);
        let out = run(&mut cloud, &dir, "a.png", b"x", "").unwrap();
        assert!(!out.wrote && out.shared);
        let index = index_of(&cloud);
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].url.as_deref(), Some(out.url.as_str()));
        assert!(
            !cloud
                .mutating_calls()
                .iter()
                .any(|c| c.op == Op::Write && c.name.as_deref() == Some(key.as_str()))
        );
    }

    #[test]
    fn missing_cloud_file_is_written_again() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_of(b"x", "a.png");
        let mut index = CloudIndex::empty();
        index.entries = vec![entry(&key, "a.png", Some("https://old/"), "")];
        let mut cloud = seed(MemoryCloud::new(), &index);
        let plan = plan_upload(&cloud, "a.png", b"x", "").unwrap();
        assert!(plan.write && plan.share && !plan.add_entry);
        let out = run(&mut cloud, &dir, "a.png", b"x", "").unwrap();
        assert!(out.wrote);
        assert!(cloud.contents(&key).is_some());
        let index = index_of(&cloud);
        assert_eq!(index.entries.len(), 1);
        assert_ne!(index.entries[0].url.as_deref(), Some("https://old/"));
    }

    #[test]
    fn same_key_in_another_folder_is_a_collision() {
        let key = key_of(b"x", "a.png");
        let mut index = CloudIndex::empty();
        index.entries = vec![entry(&key, "a.png", Some("https://u/"), "Other\\Dir")];
        let cloud = seed(MemoryCloud::new().with_file(&key, "x"), &index);
        let err = plan_upload(&cloud, "a.png", b"x", "Here").unwrap_err();
        match &err {
            UploadError::Collision { folder, url, .. } => {
                assert_eq!(folder, "Other/Dir");
                assert_eq!(url.as_deref(), Some("https://u/"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(err.to_string().contains("--name"));
        assert!(cloud.mutating_calls().is_empty());
    }

    #[test]
    fn same_name_with_different_content_warns_and_keeps_both() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = MemoryCloud::new();
        run(&mut cloud, &dir, "a.png", b"one", "F").unwrap();
        let plan = plan_upload(&cloud, "A.PNG", b"two", "f").unwrap();
        assert_eq!(
            plan.warnings,
            [UploadWarning::SameNameInFolder {
                key: key_of(b"one", "a.png")
            }]
        );
        run(&mut cloud, &dir, "A.PNG", b"two", "f").unwrap();
        assert_eq!(index_of(&cloud).entries.len(), 2);
    }

    #[test]
    fn quota_is_checked_at_plan_time() {
        let cloud = MemoryCloud::new().with_quota(10);
        let err = plan_upload(&cloud, "a.png", &[0; 20], "").unwrap_err();
        assert!(matches!(
            err,
            UploadError::Cloud(CloudError::QuotaExceeded {
                needed: 20,
                available: 10,
                ..
            })
        ));
    }

    #[test]
    fn file_too_large_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = MemoryCloud::new().with_max_file_size(10);
        let err = run(&mut cloud, &dir, "a.png", &[0; 20], "").unwrap_err();
        assert!(matches!(
            err,
            UploadError::Cloud(CloudError::FileTooLarge { .. })
        ));
        assert!(cloud.file_names().is_empty());
    }

    #[test]
    fn share_failure_leaves_an_unindexed_file_and_a_rerun_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = MemoryCloud::new();
        let key = key_of(b"x", "a.png");
        cloud.fail_once(Op::Share, Some(&key), CloudError::Backend("boom".into()));
        let err = run(&mut cloud, &dir, "a.png", b"x", "F").unwrap_err();
        assert!(matches!(
            err,
            UploadError::Incomplete {
                stage: UploadStage::Share,
                ..
            }
        ));
        assert!(err.to_string().contains("run the same upload again"));
        assert!(cloud.contents(&key).is_some());
        assert!(index_of(&cloud).entries.is_empty());

        let plan = plan_upload(&cloud, "a.png", b"x", "F").unwrap();
        assert!(!plan.write && plan.share && plan.add_entry);
        run(&mut cloud, &dir, "a.png", b"x", "F").unwrap();
        let writes = cloud
            .calls()
            .iter()
            .filter(|c| c.op == Op::Write && c.name.as_deref() == Some(key.as_str()))
            .count();
        assert_eq!(writes, 1);
        assert_eq!(index_of(&cloud).entries.len(), 1);
    }

    #[test]
    fn index_write_failure_is_incomplete_and_a_rerun_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = MemoryCloud::new();
        let key = key_of(b"x", "a.png");
        cloud.fail(
            Op::Write,
            Some(CLOUD_INFO_FILE),
            CloudError::Backend("boom".into()),
        );
        let err = run(&mut cloud, &dir, "a.png", b"x", "").unwrap_err();
        match &err {
            UploadError::Incomplete { stage, key: k, .. } => {
                assert_eq!(*stage, UploadStage::Index);
                assert_eq!(k, &key);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(err.to_string().contains("only partly written"));
        cloud.clear_failures();
        run(&mut cloud, &dir, "a.png", b"x", "").unwrap();
        assert_eq!(index_of(&cloud).entries.len(), 1);
    }

    #[test]
    fn invalid_names_are_rejected() {
        let cloud = MemoryCloud::new();
        for name in ["", "a/b", "a\\b", "a\0b"] {
            let err = plan_upload(&cloud, name, b"x", "").unwrap_err();
            assert!(
                matches!(err, UploadError::Cloud(CloudError::InvalidName { .. })),
                "{name:?}"
            );
        }
    }

    fn lookup_index() -> CloudIndex {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_a.png", "a.png", None, "Dice\\Red"),
            entry("BB_a.png", "a.png", None, "Cards"),
            entry("CC_b.png", "B.png", None, ""),
        ];
        index
    }

    fn keys_of(found: Vec<&IndexEntry>) -> Vec<&str> {
        found.into_iter().map(|e| e.key.as_str()).collect()
    }

    #[test]
    fn find_by_path_with_either_separator() {
        let index = lookup_index();
        assert_eq!(
            keys_of(find_entries(&index, "dice/red/A.PNG")),
            ["AA_a.png"]
        );
        assert_eq!(
            keys_of(find_entries(&index, "Dice\\Red\\a.png")),
            ["AA_a.png"]
        );
        assert_eq!(keys_of(find_entries(&index, "/b.png")), ["CC_b.png"]);
        assert!(find_entries(&index, "Dice/a.png").is_empty());
    }

    #[test]
    fn find_by_bare_name_or_key() {
        let index = lookup_index();
        assert_eq!(
            keys_of(find_entries(&index, "A.png")),
            ["AA_a.png", "BB_a.png"]
        );
        assert_eq!(keys_of(find_entries(&index, "cc_B.PNG")), ["CC_b.png"]);
        assert_eq!(keys_of(find_entries(&index, "b.png")), ["CC_b.png"]);
        assert!(find_entries(&index, "zzz").is_empty());
    }
}
