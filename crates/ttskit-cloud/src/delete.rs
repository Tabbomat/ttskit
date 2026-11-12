//! Deleting a file or a whole folder from the cloud.
//!
//! A delete has two steps. [`plan_delete`] looks at the cloud without
//! changing anything and says what [`delete`] would do; [`delete`] then takes
//! a backup of the index, deletes the cloud files, and finally removes the
//! entries and folders from the index (writing it into that same backup, not
//! a second one).
//!
//! Files go first and the index last, so an interrupted delete leaves index
//! entries whose files are gone. Planning the same target again then sees
//! those files as missing from the cloud, and running the delete again only
//! has to update the index.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::{
    Backup, BackupError, BackupStore, CloudBackend, CloudError, CloudIndex, IndexEntry, IndexError,
    find_entries, is_game_internal, normalize_folder,
};

/// What to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTarget<'a> {
    /// One file, given as a path (`Folder/name`), a display name or a cloud
    /// key; see [`find_entries`]. A file that is in the cloud but not in the
    /// index can be given by its cloud name.
    File(&'a str),
    /// A folder with everything below it. `/` or `\` separate levels.
    Folder(&'a str),
}

/// Errors from planning or running a delete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeleteError {
    /// No file matches.
    #[error("no file matches {query:?}")]
    NotFound {
        /// What was asked for.
        query: String,
    },
    /// Several files match.
    #[error(
        "{query:?} matches {} files; give the folder or the cloud key: {}",
        matches.len(),
        matches
            .iter()
            .map(|(path, key)| format!("{path} ({key})"))
            .collect::<Vec<_>>()
            .join(", ")
    )]
    Ambiguous {
        /// What was asked for.
        query: String,
        /// The matches as `(display path, cloud key)`.
        matches: Vec<(String, String)>,
    },
    /// No such folder.
    #[error("no folder {folder:?}")]
    FolderNotFound {
        /// The folder as given.
        folder: String,
    },
    /// The root folder cannot be deleted.
    #[error("the root folder cannot be deleted")]
    RootFolder,
    /// The file belongs to the game itself.
    #[error("{name} is a file the game needs and is never deleted")]
    GameFile {
        /// The file name.
        name: String,
    },
    /// The delete stopped part way. The index still lists files that may
    /// already be gone from the cloud.
    #[error(
        "delete incomplete: removed {} file(s), failed on {failed}: {source}; \
         the previous index is in backup {backup}; run the same delete again to finish it",
        deleted.len()
    )]
    Incomplete {
        /// The backup of the index taken before the delete.
        backup: String,
        /// The cloud keys deleted so far.
        deleted: Vec<String>,
        /// The cloud file (or index file) that failed.
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

/// One file a delete removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteFile {
    /// The cloud key, which is the name of the cloud file.
    pub key: String,
    /// The display name.
    pub name: String,
    /// The folder, `/` separated; empty for the root and for files that are
    /// not in the index.
    pub folder: String,
    /// Size in bytes: the cloud size if the file is there, else the index size.
    pub size: Option<u64>,
    /// The download URL from the index, if any.
    pub url: Option<String>,
    /// Whether the file has an index entry.
    pub indexed: bool,
    /// Whether the file exists in the cloud. A file with an entry but not in
    /// the cloud is only removed from the index.
    pub in_cloud: bool,
}

/// What [`delete`] will do; made by [`plan_delete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePlan {
    /// For a folder target, the folder as it exists (`/` separated, existing
    /// spelling); `None` for a file target.
    pub folder: Option<String>,
    /// The files to delete, sorted by folder, name and key (ignoring case).
    pub files: Vec<DeleteFile>,
    /// The folders to remove from `CloudFolder.bson` (`/` separated, the
    /// game's spelling), deepest first. Empty for a file target.
    pub folders: Vec<String>,
}

impl DeletePlan {
    /// Whether the index has to be changed: some file is indexed or some
    /// folder is removed.
    pub fn changes_index(&self) -> bool {
        self.files.iter().any(|f| f.indexed) || !self.folders.is_empty()
    }
}

/// The result of [`delete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteOutcome {
    /// The backup of the index taken before anything was deleted.
    pub backup: Backup,
    /// The cloud keys that were deleted (not those already gone).
    pub deleted: Vec<String>,
    /// The index files that were written.
    pub index_changed: Vec<&'static str>,
}

fn display_path(folder: &str, name: &str) -> String {
    let folder = normalize_folder(folder);
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

fn delete_file(entry: &IndexEntry, cloud: &HashMap<String, u64>) -> DeleteFile {
    let cloud_size = cloud.get(&entry.key.to_ascii_lowercase()).copied();
    DeleteFile {
        key: entry.key.clone(),
        name: entry.name.clone(),
        folder: normalize_folder(&entry.folder),
        size: cloud_size.or(entry.size),
        url: entry.url.clone(),
        indexed: true,
        in_cloud: cloud_size.is_some(),
    }
}

/// Works out what deleting `target` would do, without changing anything.
///
/// Names and folders compare ignoring ASCII case. Deleting the last file of a
/// folder keeps the folder. The game's own files are refused.
pub fn plan_delete(
    backend: &dyn CloudBackend,
    target: DeleteTarget<'_>,
) -> Result<DeletePlan, DeleteError> {
    match target {
        DeleteTarget::File(query) => plan_file(backend, query),
        DeleteTarget::Folder(folder) => plan_folder(backend, folder),
    }
}

fn plan_file(backend: &dyn CloudBackend, query: &str) -> Result<DeletePlan, DeleteError> {
    if is_game_internal(query) {
        return Err(DeleteError::GameFile {
            name: query.to_owned(),
        });
    }
    let index = CloudIndex::load(backend)?;
    let listed = backend.list()?;
    let cloud: HashMap<String, u64> = listed
        .iter()
        .map(|f| (f.name.to_ascii_lowercase(), f.size))
        .collect();

    let found = find_entries(&index, query);
    let file = match found.as_slice() {
        [] => {
            let stored = listed
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(query))
                .ok_or_else(|| DeleteError::NotFound {
                    query: query.to_owned(),
                })?;
            if is_game_internal(&stored.name) {
                return Err(DeleteError::GameFile {
                    name: stored.name.clone(),
                });
            }
            DeleteFile {
                key: stored.name.clone(),
                name: stored.name.clone(),
                folder: String::new(),
                size: Some(stored.size),
                url: None,
                indexed: false,
                in_cloud: true,
            }
        }
        [entry] => {
            if is_game_internal(&entry.key) {
                return Err(DeleteError::GameFile {
                    name: entry.key.clone(),
                });
            }
            delete_file(entry, &cloud)
        }
        many => {
            return Err(DeleteError::Ambiguous {
                query: query.to_owned(),
                matches: many
                    .iter()
                    .map(|e| (display_path(&e.folder, &e.name), e.key.clone()))
                    .collect(),
            });
        }
    };
    Ok(DeletePlan {
        folder: None,
        files: vec![file],
        folders: Vec::new(),
    })
}

/// Whether `path` (normalized) is `root` or below it; `root_lower` is the
/// ASCII-lowercased `root`.
fn within(path: &str, root_lower: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path == root_lower
        || path
            .strip_prefix(root_lower)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn plan_folder(backend: &dyn CloudBackend, folder: &str) -> Result<DeletePlan, DeleteError> {
    let wanted = normalize_folder(folder);
    if wanted.is_empty() {
        return Err(DeleteError::RootFolder);
    }
    let index = CloudIndex::load(backend)?;
    let resolved = index
        .all_folders()
        .into_iter()
        .find(|f| f.eq_ignore_ascii_case(&wanted))
        .ok_or_else(|| DeleteError::FolderNotFound {
            folder: folder.to_owned(),
        })?;
    let root = resolved.to_ascii_lowercase();

    let cloud: HashMap<String, u64> = backend
        .list()?
        .into_iter()
        .map(|f| (f.name.to_ascii_lowercase(), f.size))
        .collect();
    let mut files: Vec<DeleteFile> = index
        .entries
        .iter()
        .filter(|e| within(&normalize_folder(&e.folder), &root))
        .map(|e| delete_file(e, &cloud))
        .collect();
    files.sort_by_cached_key(|f| {
        (
            f.folder.to_ascii_lowercase(),
            f.name.to_ascii_lowercase(),
            f.key.clone(),
        )
    });

    let mut folders: Vec<String> = index
        .folders
        .iter()
        .map(|f| normalize_folder(f))
        .filter(|f| within(f, &root))
        .collect();
    folders.sort_by_cached_key(|f| {
        (
            std::cmp::Reverse(f.split('/').count()),
            f.to_ascii_lowercase(),
            f.clone(),
        )
    });
    folders.dedup();

    Ok(DeletePlan {
        folder: Some(resolved),
        files,
        folders,
    })
}

/// Carries out `plan`: backs up the index, deletes the files that are in the
/// cloud, then removes the entries and folders from the index.
///
/// The backup is always taken first, even when the index does not change, and
/// is reused for the index write. A file that is already gone counts as
/// deleted. If a step fails after the backup, [`DeleteError::Incomplete`] is
/// returned; planning and deleting again finishes the job.
pub fn delete(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    plan: &DeletePlan,
    now: SystemTime,
) -> Result<DeleteOutcome, DeleteError> {
    let backup = store.create(backend, now)?;
    let mut deleted = Vec::new();
    let incomplete =
        |deleted: &[String], failed: &str, source: Box<dyn std::error::Error + Send + Sync>| {
            DeleteError::Incomplete {
                backup: backup.name.clone(),
                deleted: deleted.to_vec(),
                failed: failed.to_owned(),
                source,
            }
        };

    for file in plan.files.iter().filter(|f| f.in_cloud) {
        match backend.delete(&file.key) {
            Ok(()) => deleted.push(file.key.clone()),
            Err(CloudError::NotFound { .. }) => {}
            Err(e) => return Err(incomplete(&deleted, &file.key, Box::new(e))),
        }
    }

    let mut index_changed = Vec::new();
    if plan.changes_index() {
        let update = |backend: &mut dyn CloudBackend| -> Result<_, DeleteError> {
            let mut index = CloudIndex::load(backend)?;
            index.entries.retain(|e| {
                !plan
                    .files
                    .iter()
                    .any(|f| f.indexed && f.key.eq_ignore_ascii_case(&e.key))
            });
            index.folders.retain(|f| {
                let f = normalize_folder(f);
                !plan.folders.iter().any(|p| p.eq_ignore_ascii_case(&f))
            });
            Ok(index.write_after_backup(backend, &backup)?)
        };
        index_changed = match update(backend) {
            Ok(changed) => changed,
            Err(e) => {
                let source: Box<dyn std::error::Error + Send + Sync> = match e {
                    DeleteError::Index(e) => Box::new(e),
                    DeleteError::Backup(e) => Box::new(e),
                    other => Box::new(std::io::Error::other(other.to_string())),
                };
                return Err(incomplete(&deleted, "the index", source));
            }
        };
    }

    Ok(DeleteOutcome {
        backup,
        deleted,
        index_changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, MemoryCloud, Op};
    use std::time::UNIX_EPOCH;

    fn seed(cloud: MemoryCloud, index: &CloudIndex) -> MemoryCloud {
        cloud
            .with_file(CLOUD_INFO_FILE, index.encode_info().unwrap())
            .with_file(CLOUD_FOLDER_FILE, index.encode_folders().unwrap())
    }

    fn entry(key: &str, name: &str, folder: &str) -> IndexEntry {
        IndexEntry::new(
            key,
            name,
            Some(format!("https://u/{key}")),
            Some(5),
            None,
            folder,
        )
    }

    /// A cloud holding the files of `entries` and the index describing them.
    fn cloud_with(entries: Vec<IndexEntry>, folders: &[&str]) -> MemoryCloud {
        let mut index = CloudIndex::empty();
        index.folders = folders.iter().map(|f| (*f).to_owned()).collect();
        let mut cloud = MemoryCloud::new();
        for e in &entries {
            cloud = cloud.with_file(&e.key, "hello");
        }
        index.entries = entries;
        seed(cloud, &index)
    }

    fn sample() -> MemoryCloud {
        cloud_with(
            vec![
                entry("AA_a.png", "a.png", "Dice\\Red"),
                entry("BB_a.png", "a.png", "Cards"),
                entry("CC_b.png", "b.png", "Decks"),
                entry("DD_c.png", "c.png", "Decks\\Sub"),
                entry("EE_d.png", "d.png", "Deck"),
                entry("FF_e.png", "e.png", ""),
            ],
            &["Dice", "Dice\\Red", "Cards", "Decks", "Decks\\Sub", "Deck"],
        )
    }

    fn index_of(cloud: &MemoryCloud) -> CloudIndex {
        CloudIndex::load(cloud).unwrap()
    }

    fn keys(index: &CloudIndex) -> Vec<&str> {
        index.entries.iter().map(|e| e.key.as_str()).collect()
    }

    fn run(
        cloud: &mut MemoryCloud,
        dir: &tempfile::TempDir,
        target: DeleteTarget<'_>,
    ) -> Result<DeleteOutcome, DeleteError> {
        let plan = plan_delete(cloud, target)?;
        delete(cloud, &BackupStore::new(dir.path()), &plan, UNIX_EPOCH)
    }

    fn plan_keys(plan: &DeletePlan) -> Vec<&str> {
        plan.files.iter().map(|f| f.key.as_str()).collect()
    }

    #[test]
    fn file_by_path_with_either_separator() {
        for query in ["dice/red/A.PNG", "Dice\\Red\\a.png"] {
            let dir = tempfile::tempdir().unwrap();
            let mut cloud = sample();
            let out = run(&mut cloud, &dir, DeleteTarget::File(query)).unwrap();
            assert_eq!(out.deleted, ["AA_a.png"]);
            assert!(cloud.contents("AA_a.png").is_none());
            assert!(!keys(&index_of(&cloud)).contains(&"AA_a.png"));
            assert_eq!(index_of(&cloud).entries.len(), 5);
        }
    }

    #[test]
    fn file_by_name_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        let plan = plan_delete(&cloud, DeleteTarget::File("B.png")).unwrap();
        assert_eq!(plan_keys(&plan), ["CC_b.png"]);
        let f = &plan.files[0];
        assert!(f.indexed && f.in_cloud);
        assert_eq!(f.folder, "Decks");
        assert_eq!(f.size, Some(5));
        assert_eq!(f.url.as_deref(), Some("https://u/CC_b.png"));
        assert_eq!(plan.folder, None);
        assert!(plan.folders.is_empty());
        run(&mut cloud, &dir, DeleteTarget::File("B.png")).unwrap();

        run(&mut cloud, &dir, DeleteTarget::File("ff_E.png")).unwrap();
        assert!(cloud.contents("FF_e.png").is_none());
        assert_eq!(index_of(&cloud).entries.len(), 4);
    }

    #[test]
    fn ambiguous_name_lists_both_matches() {
        let cloud = sample();
        let err = plan_delete(&cloud, DeleteTarget::File("a.png")).unwrap_err();
        match &err {
            DeleteError::Ambiguous { matches, .. } => assert_eq!(
                matches,
                &[
                    ("Dice/Red/a.png".to_owned(), "AA_a.png".to_owned()),
                    ("Cards/a.png".to_owned(), "BB_a.png".to_owned()),
                ]
            ),
            other => panic!("unexpected {other:?}"),
        }
        assert!(err.to_string().contains("AA_a.png"));
    }

    #[test]
    fn unknown_file_is_not_found() {
        let cloud = sample();
        assert!(matches!(
            plan_delete(&cloud, DeleteTarget::File("zzz")).unwrap_err(),
            DeleteError::NotFound { .. }
        ));
    }

    #[test]
    fn unindexed_file_is_deleted_with_a_backup_and_the_index_stays() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample().with_file("ZZ_Stray.png", "stray");
        let before = cloud.contents(CLOUD_INFO_FILE).unwrap();
        let plan = plan_delete(&cloud, DeleteTarget::File("zz_stray.PNG")).unwrap();
        assert!(!plan.changes_index());
        let f = &plan.files[0];
        assert!(!f.indexed && f.in_cloud);
        assert_eq!((f.key.as_str(), f.folder.as_str()), ("ZZ_Stray.png", ""));
        assert_eq!(f.size, Some(5));
        assert_eq!(f.url, None);

        let out = run(&mut cloud, &dir, DeleteTarget::File("zz_stray.PNG")).unwrap();
        assert_eq!(out.deleted, ["ZZ_Stray.png"]);
        assert!(out.index_changed.is_empty());
        assert!(cloud.contents("ZZ_Stray.png").is_none());
        assert_eq!(cloud.contents(CLOUD_INFO_FILE).unwrap(), before);
        assert_eq!(out.backup.read(CLOUD_INFO_FILE).unwrap().unwrap(), before);
        assert_eq!(BackupStore::new(dir.path()).list().unwrap().len(), 1);
    }

    #[test]
    fn game_files_are_refused() {
        let cloud = sample().with_file("WorkshopUpload", "x");
        for name in ["CloudInfo.bson", "cloudfolder.bson", "WorkshopUpload"] {
            let err = plan_delete(&cloud, DeleteTarget::File(name)).unwrap_err();
            assert!(matches!(err, DeleteError::GameFile { .. }), "{name}");
        }
        assert!(cloud.mutating_calls().is_empty());
    }

    #[test]
    fn entry_without_a_cloud_file_is_only_removed_from_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        cloud.delete("CC_b.png").unwrap();
        cloud.clear_calls();
        let plan = plan_delete(&cloud, DeleteTarget::File("b.png")).unwrap();
        let f = &plan.files[0];
        assert!(f.indexed && !f.in_cloud);
        assert_eq!(f.size, Some(5));
        let out = run(&mut cloud, &dir, DeleteTarget::File("b.png")).unwrap();
        assert!(out.deleted.is_empty());
        assert!(out.index_changed.contains(&CLOUD_INFO_FILE));
        assert!(!cloud.calls().iter().any(|c| c.op == Op::Delete));
        assert!(!keys(&index_of(&cloud)).contains(&"CC_b.png"));
    }

    #[test]
    fn folder_delete_is_recursive_and_leaves_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        let plan = plan_delete(&cloud, DeleteTarget::Folder("decks")).unwrap();
        assert_eq!(plan.folder.as_deref(), Some("Decks"));
        assert_eq!(plan_keys(&plan), ["CC_b.png", "DD_c.png"]);
        assert_eq!(plan.folders, ["Decks/Sub", "Decks"]);

        let out = run(&mut cloud, &dir, DeleteTarget::Folder("decks")).unwrap();
        assert_eq!(out.deleted, ["CC_b.png", "DD_c.png"]);
        let index = index_of(&cloud);
        assert_eq!(
            keys(&index),
            ["AA_a.png", "BB_a.png", "EE_d.png", "FF_e.png"]
        );
        assert_eq!(index.folders, ["Dice", "Dice\\Red", "Cards", "Deck"]);
        assert!(cloud.contents("EE_d.png").is_some());
        assert!(cloud.contents("CC_b.png").is_none());
        assert!(cloud.contents("DD_c.png").is_none());
    }

    #[test]
    fn nested_folder_with_slash_or_backslash() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        run(&mut cloud, &dir, DeleteTarget::Folder("DICE\\red")).unwrap();
        let index = index_of(&cloud);
        assert_eq!(index.entries.len(), 5);
        assert_eq!(
            index.folders,
            ["Dice", "Cards", "Decks", "Decks\\Sub", "Deck"]
        );
    }

    #[test]
    fn folder_known_only_through_an_entry_can_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = cloud_with(vec![entry("AA_a.png", "a.png", "Old\\Sub")], &[]);
        let plan = plan_delete(&cloud, DeleteTarget::Folder("old")).unwrap();
        assert_eq!(plan_keys(&plan), ["AA_a.png"]);
        assert!(plan.folders.is_empty());
        assert!(plan.changes_index());
        run(&mut cloud, &dir, DeleteTarget::Folder("old")).unwrap();
        let index = index_of(&cloud);
        assert!(index.entries.is_empty());
        assert!(cloud.contents("AA_a.png").is_none());
    }

    #[test]
    fn root_and_unknown_folders_are_refused() {
        let cloud = sample();
        for root in ["", "/", "\\"] {
            assert!(matches!(
                plan_delete(&cloud, DeleteTarget::Folder(root)).unwrap_err(),
                DeleteError::RootFolder
            ));
        }
        assert!(matches!(
            plan_delete(&cloud, DeleteTarget::Folder("Nope")).unwrap_err(),
            DeleteError::FolderNotFound { .. }
        ));
        assert!(matches!(
            plan_delete(&cloud, DeleteTarget::Folder("Dec")).unwrap_err(),
            DeleteError::FolderNotFound { .. }
        ));
    }

    #[test]
    fn deleting_the_last_file_keeps_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        run(&mut cloud, &dir, DeleteTarget::File("Cards/a.png")).unwrap();
        let index = index_of(&cloud);
        assert!(index.folders.iter().any(|f| f == "Cards"));
        assert_eq!(index.entries.len(), 5);
    }

    #[test]
    fn backup_comes_first_and_holds_the_previous_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        let info = cloud.contents(CLOUD_INFO_FILE).unwrap();
        let folders = cloud.contents(CLOUD_FOLDER_FILE).unwrap();
        cloud.clear_calls();
        let out = run(&mut cloud, &dir, DeleteTarget::Folder("Decks")).unwrap();
        assert_eq!(out.backup.read(CLOUD_INFO_FILE).unwrap().unwrap(), info);
        assert_eq!(
            out.backup.read(CLOUD_FOLDER_FILE).unwrap().unwrap(),
            folders
        );
        assert_eq!(BackupStore::new(dir.path()).list().unwrap().len(), 1);
        let first = cloud.mutating_calls().first().unwrap().clone();
        assert_eq!(first.op, Op::Delete);
        // The backup directory exists before the first mutation; the index
        // was only read (not written) before it.
        let calls = cloud.calls();
        let first_mut = calls.iter().position(|c| c.op.is_mutating()).unwrap();
        assert!(
            calls[..first_mut]
                .iter()
                .any(|c| c.op == Op::Read && c.name.as_deref() == Some(CLOUD_INFO_FILE))
        );
    }

    #[test]
    fn planning_makes_no_mutating_calls() {
        let cloud = sample().with_file("ZZ_s.png", "x");
        for target in [
            DeleteTarget::File("b.png"),
            DeleteTarget::File("zz_s.png"),
            DeleteTarget::Folder("Decks"),
        ] {
            plan_delete(&cloud, target).unwrap();
        }
        assert!(plan_delete(&cloud, DeleteTarget::File("a.png")).is_err());
        assert!(cloud.mutating_calls().is_empty());
    }

    #[test]
    fn failure_on_the_second_file_is_incomplete_and_a_rerun_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        cloud.fail(
            Op::Delete,
            Some("DD_c.png"),
            CloudError::Backend("boom".into()),
        );
        let err = run(&mut cloud, &dir, DeleteTarget::Folder("Decks")).unwrap_err();
        let backup_name = BackupStore::new(dir.path()).list().unwrap()[0].name.clone();
        match &err {
            DeleteError::Incomplete {
                backup,
                deleted,
                failed,
                ..
            } => {
                assert_eq!(backup, &backup_name);
                assert_eq!(deleted, &["CC_b.png"]);
                assert_eq!(failed, "DD_c.png");
            }
            other => panic!("unexpected {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains(&backup_name));
        assert!(msg.ends_with("run the same delete again to finish it"));
        assert!(cloud.contents("CC_b.png").is_none());
        assert!(cloud.contents("DD_c.png").is_some());
        assert_eq!(index_of(&cloud).entries.len(), 6);

        cloud.clear_failures();
        cloud.clear_calls();
        let plan = plan_delete(&cloud, DeleteTarget::Folder("Decks")).unwrap();
        assert!(!plan.files[0].in_cloud && plan.files[1].in_cloud);
        run(&mut cloud, &dir, DeleteTarget::Folder("Decks")).unwrap();
        let deletes: Vec<_> = cloud
            .calls()
            .into_iter()
            .filter(|c| c.op == Op::Delete)
            .collect();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].name.as_deref(), Some("DD_c.png"));
        let index = index_of(&cloud);
        assert_eq!(index.entries.len(), 4);
        assert_eq!(index.folders, ["Dice", "Dice\\Red", "Cards", "Deck"]);
    }

    #[test]
    fn index_write_failure_is_incomplete_and_a_rerun_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        cloud.fail(
            Op::Write,
            Some(CLOUD_INFO_FILE),
            CloudError::Backend("boom".into()),
        );
        let err = run(&mut cloud, &dir, DeleteTarget::File("b.png")).unwrap_err();
        match &err {
            DeleteError::Incomplete {
                deleted, failed, ..
            } => {
                assert_eq!(deleted, &["CC_b.png"]);
                assert_eq!(failed, "the index");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(err.to_string().contains("run the same delete again"));
        assert_eq!(index_of(&cloud).entries.len(), 6);
        // Only the one backup so far: the index write reused it.
        assert_eq!(BackupStore::new(dir.path()).list().unwrap().len(), 1);

        cloud.clear_failures();
        run(&mut cloud, &dir, DeleteTarget::File("b.png")).unwrap();
        assert_eq!(index_of(&cloud).entries.len(), 5);
    }

    #[test]
    fn index_write_after_deleting_uses_the_single_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut cloud = sample();
        let out = run(&mut cloud, &dir, DeleteTarget::Folder("Dice")).unwrap();
        assert!(out.index_changed.contains(&CLOUD_INFO_FILE));
        assert!(out.index_changed.contains(&CLOUD_FOLDER_FILE));
        assert_eq!(BackupStore::new(dir.path()).list().unwrap().len(), 1);
    }
}
