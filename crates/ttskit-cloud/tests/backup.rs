use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bson::{Bson, Document, doc};
use ttskit_cloud::{
    Backup, BackupError, BackupStore, CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, CloudError, CloudIndex,
    IndexEntry, MemoryCloud, Op, RestoreAction, plan_restore, restore,
};

const SHA: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

fn t(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// 2026-05-11 17:49:48 UTC
const NOW: u64 = 1_778_521_788;
const NOW_NAME: &str = "20260511T174948Z";

fn info_bytes(names: &[&str]) -> Vec<u8> {
    names
        .iter()
        .map(|n| {
            (
                format!("{SHA}_{n}"),
                Bson::Document(doc! { "Name": *n, "Size": 5_i64, "Extra": "keep me" }),
            )
        })
        .collect::<Document>()
        .to_vec()
        .unwrap()
}

fn folder_bytes(list: &[&str]) -> Vec<u8> {
    list.iter()
        .enumerate()
        .map(|(i, f)| (i.to_string(), Bson::String((*f).into())))
        .collect::<Document>()
        .to_vec()
        .unwrap()
}

fn cloud(info: &[u8], folders: &[u8]) -> MemoryCloud {
    MemoryCloud::new()
        .with_file(CLOUD_INFO_FILE, info)
        .with_file(CLOUD_FOLDER_FILE, folders)
}

fn backend_err() -> CloudError {
    CloudError::Backend("boom".into())
}

fn store() -> (tempfile::TempDir, BackupStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = BackupStore::new(dir.path().join("backups"));
    (dir, store)
}

fn file_size(b: &Backup, name: &str) -> Option<u64> {
    b.files.iter().find(|f| f.name == name).unwrap().size
}

// ---- create ----

#[test]
fn create_copies_both_files_byte_exact() {
    let (_d, store) = store();
    let info = info_bytes(&["a", "b"]);
    let folders = folder_bytes(&["x"]);
    let c = cloud(&info, &folders);
    let b = store.create(&c, t(NOW)).unwrap();
    assert_eq!(b.name, NOW_NAME);
    assert_eq!(b.path, store.root().join(NOW_NAME));
    assert_eq!(b.files.len(), 2);
    assert_eq!(b.files[0].name, CLOUD_INFO_FILE);
    assert_eq!(b.files[1].name, CLOUD_FOLDER_FILE);
    assert_eq!(file_size(&b, CLOUD_INFO_FILE), Some(info.len() as u64));
    assert_eq!(b.read(CLOUD_INFO_FILE).unwrap(), Some(info));
    assert_eq!(b.read(CLOUD_FOLDER_FILE).unwrap(), Some(folders));
    assert!(c.mutating_calls().is_empty());
}

#[test]
fn create_keeps_bytes_that_are_not_bson() {
    let (_d, store) = store();
    let c = MemoryCloud::new().with_file(CLOUD_INFO_FILE, b"not bson".to_vec());
    let b = store.create(&c, t(NOW)).unwrap();
    assert_eq!(b.read(CLOUD_INFO_FILE).unwrap(), Some(b"not bson".to_vec()));
}

#[test]
fn create_leaves_missing_cloud_files_absent() {
    let (_d, store) = store();
    let c = MemoryCloud::new().with_file(CLOUD_FOLDER_FILE, folder_bytes(&["x"]));
    let b = store.create(&c, t(NOW)).unwrap();
    assert_eq!(file_size(&b, CLOUD_INFO_FILE), None);
    assert_eq!(b.read(CLOUD_INFO_FILE).unwrap(), None);
    assert!(!b.path.join(CLOUD_INFO_FILE).exists());
    assert!(file_size(&b, CLOUD_FOLDER_FILE).is_some());
}

#[test]
fn create_appends_a_suffix_on_collision() {
    let (_d, store) = store();
    let c = cloud(&info_bytes(&["a"]), &folder_bytes(&[]));
    let names: Vec<String> = (0..3)
        .map(|_| store.create(&c, t(NOW)).unwrap().name)
        .collect();
    assert_eq!(
        names,
        [
            NOW_NAME.to_string(),
            format!("{NOW_NAME}-2"),
            format!("{NOW_NAME}-3")
        ]
    );
}

#[test]
fn create_leaves_no_staging_dir() {
    let (_d, store) = store();
    let c = cloud(&info_bytes(&["a"]), &folder_bytes(&[]));
    store.create(&c, t(NOW)).unwrap();
    let names: Vec<String> = fs::read_dir(store.root())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [NOW_NAME]);
}

#[test]
fn create_replaces_a_leftover_staging_dir() {
    let (_d, store) = store();
    fs::create_dir_all(store.root().join(format!(".{NOW_NAME}.tmp"))).unwrap();
    fs::write(
        store.root().join(format!(".{NOW_NAME}.tmp")).join("junk"),
        b"x",
    )
    .unwrap();
    let c = cloud(&info_bytes(&["a"]), &folder_bytes(&[]));
    let b = store.create(&c, t(NOW)).unwrap();
    assert!(!b.path.join("junk").exists());
    assert!(!store.root().join(format!(".{NOW_NAME}.tmp")).exists());
}

#[test]
fn create_aborts_on_read_errors_other_than_not_found() {
    let (_d, store) = store();
    let c = cloud(&info_bytes(&["a"]), &folder_bytes(&[]));
    c.fail_once(Op::Read, Some(CLOUD_FOLDER_FILE), backend_err());
    let err = store.create(&c, t(NOW)).unwrap_err();
    assert!(matches!(err, BackupError::Cloud(CloudError::Backend(_))));
    assert!(store.list().unwrap().is_empty());
    assert!(!store.root().join(NOW_NAME).exists());
}

#[test]
fn create_fails_when_the_root_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("file");
    fs::write(&root, b"x").unwrap();
    let c = cloud(&info_bytes(&["a"]), &folder_bytes(&[]));
    let err = BackupStore::new(&root).create(&c, t(NOW)).unwrap_err();
    assert!(matches!(err, BackupError::Io { .. }));
    assert!(err.to_string().contains("file"));
}

// ---- list / get ----

#[test]
fn list_is_newest_first_including_suffixes() {
    let (_d, store) = store();
    for name in [
        "20260101T000000Z",
        "20260101T000000Z-2",
        "20260101T000000Z-10",
        "20260101T000000Z-3",
        "20251231T235959Z",
        "20260102T000000Z",
    ] {
        fs::create_dir_all(store.root().join(name)).unwrap();
    }
    let names: Vec<String> = store.list().unwrap().into_iter().map(|b| b.name).collect();
    assert_eq!(
        names,
        [
            "20260102T000000Z",
            "20260101T000000Z-10",
            "20260101T000000Z-3",
            "20260101T000000Z-2",
            "20260101T000000Z",
            "20251231T235959Z",
        ]
    );
}

#[test]
fn list_ignores_junk() {
    let (_d, store) = store();
    fs::create_dir_all(store.root().join("20260101T000000Z")).unwrap();
    fs::create_dir_all(store.root().join(".20260102T000000Z.tmp")).unwrap();
    fs::create_dir_all(store.root().join("notes")).unwrap();
    fs::create_dir_all(store.root().join("20260103T000000Z-0")).unwrap();
    fs::write(store.root().join("20260104T000000Z"), b"a file").unwrap();
    let names: Vec<String> = store.list().unwrap().into_iter().map(|b| b.name).collect();
    assert_eq!(names, ["20260101T000000Z"]);
}

#[test]
fn list_of_a_missing_root_is_empty() {
    let (_d, store) = store();
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn get_rejects_invalid_names() {
    let (_d, store) = store();
    for name in ["..", "a/b", "a\\b", "foo", "", ".", "../20260101T000000Z"] {
        assert!(
            matches!(store.get(name), Err(BackupError::InvalidName { .. })),
            "{name:?}"
        );
    }
}

#[test]
fn get_reports_missing_backups() {
    let (_d, store) = store();
    assert!(matches!(
        store.get("20260101T000000Z"),
        Err(BackupError::NotFound { name }) if name == "20260101T000000Z"
    ));
}

#[test]
fn get_finds_a_created_backup() {
    let (_d, store) = store();
    let c = cloud(&info_bytes(&["a"]), &folder_bytes(&[]));
    let made = store.create(&c, t(NOW)).unwrap();
    assert_eq!(store.get(&made.name).unwrap(), made);
    assert_eq!(store.list().unwrap(), [made]);
}

// ---- plan_restore / restore ----

fn actions(plan: &ttskit_cloud::RestorePlan) -> Vec<RestoreAction> {
    plan.steps.iter().map(|s| s.action).collect()
}

fn backup_of(store: &BackupStore, info: &[u8], folders: &[u8], secs: u64) -> Backup {
    store.create(&cloud(info, folders), t(secs)).unwrap()
}

#[test]
fn plan_covers_write_unchanged_and_delete() {
    let (_d, store) = store();
    let info = info_bytes(&["a"]);
    let folders = folder_bytes(&["x"]);
    let b = backup_of(&store, &info, &folders, 100);

    // info differs, folders equal
    let c = cloud(&info_bytes(&["other"]), &folders);
    let plan = plan_restore(&c, &b).unwrap();
    assert_eq!(plan.steps[0].file, CLOUD_INFO_FILE);
    assert_eq!(plan.steps[1].file, CLOUD_FOLDER_FILE);
    assert_eq!(
        actions(&plan),
        [
            RestoreAction::Write {
                size: info.len() as u64
            },
            RestoreAction::Unchanged
        ]
    );
    assert!(!plan.is_noop());

    // everything equal
    let plan = plan_restore(&cloud(&info, &folders), &b).unwrap();
    assert!(plan.is_noop());

    // cloud lacks info: write; backup lacks folders but cloud has them: delete
    let only_info = store
        .create(
            &MemoryCloud::new().with_file(CLOUD_INFO_FILE, info.clone()),
            t(200),
        )
        .unwrap();
    let c = MemoryCloud::new().with_file(CLOUD_FOLDER_FILE, folders.clone());
    let plan = plan_restore(&c, &only_info).unwrap();
    assert_eq!(
        actions(&plan),
        [
            RestoreAction::Write {
                size: info.len() as u64
            },
            RestoreAction::Delete
        ]
    );

    // both absent in both
    let empty = store.create(&MemoryCloud::new(), t(300)).unwrap();
    assert!(plan_restore(&MemoryCloud::new(), &empty).unwrap().is_noop());
}

#[test]
fn plan_refuses_invalid_bson() {
    let (_d, store) = store();
    let c = MemoryCloud::new().with_file(CLOUD_INFO_FILE, b"garbage".to_vec());
    let b = store.create(&c, t(NOW)).unwrap();
    let err = plan_restore(&MemoryCloud::new(), &b).unwrap_err();
    assert!(matches!(err, BackupError::InvalidBackup { ref name, .. } if *name == b.name));
}

#[test]
fn restore_writes_and_deletes_after_backing_up_the_current_state() {
    let (_d, store) = store();
    let old_info = info_bytes(&["old"]);
    // Backup has info only.
    let b = store
        .create(
            &MemoryCloud::new().with_file(CLOUD_INFO_FILE, old_info.clone()),
            t(100),
        )
        .unwrap();
    let cur_info = info_bytes(&["new"]);
    let cur_folders = folder_bytes(&["x"]);
    let mut c = cloud(&cur_info, &cur_folders);
    let plan = plan_restore(&c, &b).unwrap();
    let out = restore(&mut c, &store, &plan, t(NOW)).unwrap();

    assert_eq!(out.changed, [CLOUD_INFO_FILE, CLOUD_FOLDER_FILE]);
    assert_eq!(c.contents(CLOUD_INFO_FILE), Some(old_info));
    assert_eq!(c.contents(CLOUD_FOLDER_FILE), None);
    let pre = out.pre_restore_backup.unwrap();
    assert_eq!(pre.name, NOW_NAME);
    assert_eq!(pre.read(CLOUD_INFO_FILE).unwrap(), Some(cur_info));
    assert_eq!(pre.read(CLOUD_FOLDER_FILE).unwrap(), Some(cur_folders));
}

#[test]
fn restore_treats_a_missing_file_on_delete_as_success() {
    let (_d, store) = store();
    let info = info_bytes(&["a"]);
    let b = backup_of(&store, &info, &folder_bytes(&["x"]), 100);
    let plan_cloud = cloud(&info, &folder_bytes(&["y"]));
    let b2 = store
        .create(
            &MemoryCloud::new().with_file(CLOUD_INFO_FILE, info.clone()),
            t(200),
        )
        .unwrap();
    let _ = b;
    let plan = plan_restore(&plan_cloud, &b2).unwrap();
    assert_eq!(plan.steps[1].action, RestoreAction::Delete);
    // The folder file vanishes between planning and applying.
    let mut c = cloud(&info, &folder_bytes(&["y"]));
    c.fail_once(
        Op::Delete,
        Some(CLOUD_FOLDER_FILE),
        CloudError::NotFound {
            name: CLOUD_FOLDER_FILE.into(),
        },
    );
    let out = restore(&mut c, &store, &plan, t(NOW)).unwrap();
    assert_eq!(out.changed, [CLOUD_FOLDER_FILE]);
}

#[test]
fn restore_writes_exactly_the_planned_bytes() {
    let (_d, store) = store();
    let info = info_bytes(&["a"]);
    let folders = folder_bytes(&["x"]);
    let b = backup_of(&store, &info, &folders, 100);
    let mut c = cloud(&info_bytes(&["z"]), &folder_bytes(&["q"]));
    let plan = plan_restore(&c, &b).unwrap();
    // Tamper with the backup after planning.
    fs::write(b.path.join(CLOUD_INFO_FILE), b"tampered").unwrap();
    restore(&mut c, &store, &plan, t(NOW)).unwrap();
    assert_eq!(c.contents(CLOUD_INFO_FILE), Some(info));
    assert_eq!(c.contents(CLOUD_FOLDER_FILE), Some(folders));
}

#[test]
fn noop_restore_makes_no_backup_and_no_mutating_calls() {
    let (_d, store) = store();
    let info = info_bytes(&["a"]);
    let folders = folder_bytes(&["x"]);
    let b = backup_of(&store, &info, &folders, 100);
    let mut c = cloud(&info, &folders);
    let plan = plan_restore(&c, &b).unwrap();
    c.clear_calls();
    let out = restore(&mut c, &store, &plan, t(NOW)).unwrap();
    assert!(out.pre_restore_backup.is_none());
    assert!(out.changed.is_empty());
    assert!(c.calls().is_empty());
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn failed_second_write_is_reported_as_incomplete() {
    let (_d, store) = store();
    let b = backup_of(&store, &info_bytes(&["a"]), &folder_bytes(&["x"]), 100);
    let cur_info = info_bytes(&["n"]);
    let mut c = cloud(&cur_info, &folder_bytes(&["y"]));
    let plan = plan_restore(&c, &b).unwrap();
    c.fail_once(Op::Write, Some(CLOUD_FOLDER_FILE), backend_err());
    let err = restore(&mut c, &store, &plan, t(NOW)).unwrap_err();
    match err {
        BackupError::Incomplete {
            ref backup,
            ref changed,
            failed,
            ..
        } => {
            assert_eq!(backup, NOW_NAME);
            assert_eq!(changed, &[CLOUD_INFO_FILE]);
            assert_eq!(failed, CLOUD_FOLDER_FILE);
        }
        other => panic!("unexpected {other:?}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains(CLOUD_INFO_FILE) && msg.contains(NOW_NAME),
        "{msg}"
    );
    // The pre-restore backup holds the previous state.
    let pre = store.get(NOW_NAME).unwrap();
    assert_eq!(pre.read(CLOUD_INFO_FILE).unwrap(), Some(cur_info));
}

#[test]
fn failing_pre_restore_backup_prevents_all_writes() {
    let dir = tempfile::tempdir().unwrap();
    let good = BackupStore::new(dir.path().join("good"));
    let b = backup_of(&good, &info_bytes(&["a"]), &folder_bytes(&["x"]), 100);
    let mut c = cloud(&info_bytes(&["n"]), &folder_bytes(&["y"]));
    let plan = plan_restore(&c, &b).unwrap();
    let file_root = dir.path().join("afile");
    fs::write(&file_root, b"x").unwrap();
    let err = restore(&mut c, &BackupStore::new(&file_root), &plan, t(NOW)).unwrap_err();
    assert!(matches!(err, BackupError::Io { .. }));
    assert!(c.mutating_calls().is_empty());
}

// ---- save ----

fn sample_index() -> (CloudIndex, Vec<u8>, Vec<u8>) {
    let info = info_bytes(&["a", "b"]);
    let folders = folder_bytes(&["x", "x/y"]);
    let idx = CloudIndex::decode(Some(&info), Some(&folders)).unwrap();
    (idx, info, folders)
}

#[test]
fn saving_an_unchanged_index_does_nothing() {
    let (_d, store) = store();
    let (idx, info, folders) = sample_index();
    let mut c = cloud(&info, &folders);
    let out = idx.save(&mut c, &store, t(NOW)).unwrap();
    assert!(out.backup.is_none());
    assert!(out.changed.is_empty());
    assert!(c.mutating_calls().is_empty());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn saving_a_modified_index_backs_up_first_and_writes() {
    let (_d, store) = store();
    let (mut idx, info, folders) = sample_index();
    idx.entries[0].folder = "x/y".into();
    idx.entries.push(IndexEntry::new(
        format!("{SHA}_c"),
        "c",
        None,
        Some(3),
        None,
        "",
    ));
    idx.folders.push("z".into());
    let mut c = cloud(&info, &folders);
    let out = idx.save(&mut c, &store, t(NOW)).unwrap();

    let backup = out.backup.unwrap();
    assert_eq!(out.changed, [CLOUD_INFO_FILE, CLOUD_FOLDER_FILE]);
    assert_eq!(backup.read(CLOUD_INFO_FILE).unwrap(), Some(info));
    assert_eq!(backup.read(CLOUD_FOLDER_FILE).unwrap(), Some(folders));

    // Reads of the current state come before the first write.
    let calls = c.calls();
    let first_write = calls.iter().position(|c| c.op == Op::Write).unwrap();
    assert!(calls[..first_write].iter().all(|c| c.op == Op::Read));
    assert_eq!(c.mutating_calls().len(), 2);

    let loaded = CloudIndex::load(&c).unwrap();
    assert_eq!(loaded.entries.len(), 3);
    assert_eq!(loaded.entries[0].folder, "x/y");
    assert_eq!(loaded.folders, ["x", "x/y", "z"]);
    // Unknown fields survive.
    let raw: Document = Document::from_reader(&c.contents(CLOUD_INFO_FILE).unwrap()[..]).unwrap();
    assert_eq!(
        raw.get_document(format!("{SHA}_a"))
            .unwrap()
            .get_str("Extra")
            .unwrap(),
        "keep me"
    );
}

#[test]
fn saving_only_writes_files_that_differ() {
    let (_d, store) = store();
    let (mut idx, info, folders) = sample_index();
    idx.entries[1].name = "renamed".into();
    let mut c = cloud(&info, &folders);
    let out = idx.save(&mut c, &store, t(NOW)).unwrap();
    assert_eq!(out.changed, [CLOUD_INFO_FILE]);
    let writes = c.mutating_calls();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].name.as_deref(), Some(CLOUD_INFO_FILE));
}

#[test]
fn saving_into_an_empty_cloud_writes_both_files() {
    let (_d, store) = store();
    let (idx, info, folders) = sample_index();
    let mut c = MemoryCloud::new();
    let out = idx.save(&mut c, &store, t(NOW)).unwrap();
    assert_eq!(out.changed, [CLOUD_INFO_FILE, CLOUD_FOLDER_FILE]);
    assert_eq!(c.contents(CLOUD_INFO_FILE), Some(info));
    assert_eq!(c.contents(CLOUD_FOLDER_FILE), Some(folders));
    let backup = out.backup.unwrap();
    assert_eq!(file_size(&backup, CLOUD_INFO_FILE), None);
    assert_eq!(file_size(&backup, CLOUD_FOLDER_FILE), None);
}

#[test]
fn save_write_failure_is_reported_as_incomplete() {
    let (_d, store) = store();
    let (mut idx, info, folders) = sample_index();
    idx.folders.push("z".into());
    idx.entries[0].name = "renamed".into();
    let mut c = cloud(&info, &folders);
    c.fail_once(Op::Write, Some(CLOUD_FOLDER_FILE), backend_err());
    let err = idx.save(&mut c, &store, t(NOW)).unwrap_err();
    assert!(matches!(
        err,
        BackupError::Incomplete { ref changed, failed, .. }
            if changed == &[CLOUD_INFO_FILE] && failed == CLOUD_FOLDER_FILE
    ));
    assert_eq!(
        store.get(NOW_NAME).unwrap().read(CLOUD_INFO_FILE).unwrap(),
        Some(info)
    );
}

#[test]
fn save_does_not_write_when_the_backup_fails() {
    let dir = tempfile::tempdir().unwrap();
    let file_root = dir.path().join("afile");
    fs::write(&file_root, b"x").unwrap();
    let (mut idx, info, folders) = sample_index();
    idx.folders.push("z".into());
    let mut c = cloud(&info, &folders);
    let err = idx
        .save(&mut c, &BackupStore::new(&file_root), t(NOW))
        .unwrap_err();
    assert!(matches!(err, BackupError::Io { .. }));
    assert!(c.mutating_calls().is_empty());
}

#[test]
fn save_propagates_read_errors_without_writing() {
    let (_d, store) = store();
    let (mut idx, info, folders) = sample_index();
    idx.folders.push("z".into());
    let mut c = cloud(&info, &folders);
    c.fail_once(Op::Read, Some(CLOUD_INFO_FILE), backend_err());
    let err = idx.save(&mut c, &store, t(NOW)).unwrap_err();
    assert!(matches!(err, BackupError::Cloud(_)));
    assert!(c.mutating_calls().is_empty());
}
