use super::*;
use crate::{CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, MemoryCloud, Op, cloud_key};
use std::collections::HashMap;
use std::time::UNIX_EPOCH;

type Files = HashMap<String, Vec<u8>>;

fn local_files(files: &[(&str, &str)]) -> (Vec<LocalFile>, Files) {
    let mut list = Vec::new();
    let mut map = Files::new();
    for (path, data) in files {
        list.push(LocalFile {
            path: (*path).to_owned(),
            size: data.len() as u64,
            sha1: sha1_hex(data.as_bytes()),
        });
        map.insert((*path).to_owned(), data.as_bytes().to_vec());
    }
    (list, map)
}

fn reader(files: &Files) -> impl Fn(&str) -> io::Result<Vec<u8>> + '_ {
    move |p| {
        files
            .get(p)
            .cloned()
            .ok_or_else(|| io::Error::other("no such file"))
    }
}

/// A cloud holding `files` as (folder with `\`, name, content), indexed.
fn cloud_with(files: &[(&str, &str, &str)]) -> MemoryCloud {
    let mut index = CloudIndex::empty();
    let mut cloud = MemoryCloud::new();
    for (folder, name, data) in files {
        let key = cloud_key(&sha1_hex(data.as_bytes()), name);
        cloud = cloud.with_file(&key, data.as_bytes());
        index.entries.push(IndexEntry::new(
            key.clone(),
            *name,
            Some(format!("https://u/{key}")),
            Some(data.len() as u64),
            None,
            *folder,
        ));
        let mut path = String::new();
        for level in folder.split('\\').filter(|l| !l.is_empty()) {
            if !path.is_empty() {
                path.push('\\');
            }
            path.push_str(level);
            if !index.folders.contains(&path) {
                index.folders.push(path.clone());
            }
        }
    }
    cloud
        .with_file(CLOUD_INFO_FILE, index.encode_info().unwrap())
        .with_file(CLOUD_FOLDER_FILE, index.encode_folders().unwrap())
}

/// Entries as (folder, name, key) sorted, folders sorted, cloud file names
/// sorted without the index files.
type State = (Vec<(String, String, String)>, Vec<String>, Vec<String>);

fn state(cloud: &MemoryCloud) -> State {
    let index = CloudIndex::load(cloud).unwrap();
    let mut entries: Vec<_> = index
        .entries
        .iter()
        .map(|e| (e.folder.clone(), e.name.clone(), e.key.clone()))
        .collect();
    entries.sort();
    let mut folders = index.folders.clone();
    folders.sort();
    let mut files: Vec<String> = cloud
        .file_names()
        .into_iter()
        .filter(|n| n != CLOUD_INFO_FILE && n != CLOUD_FOLDER_FILE)
        .collect();
    files.sort();
    (entries, folders, files)
}

fn count(cloud: &MemoryCloud, op: Op) -> usize {
    cloud.calls().iter().filter(|c| c.op == op).count()
}

fn run(
    cloud: &mut MemoryCloud,
    store: &BackupStore,
    local: &[LocalFile],
    files: &Files,
    prune: bool,
) -> Result<PushOutcome, PushError> {
    let plan = plan_push(cloud, local, "Game", prune, &IgnoreSet::empty(), 0)?;
    push(
        cloud,
        store,
        &plan,
        &reader(files),
        UNIX_EPOCH,
        "d",
        &mut |_| {},
    )
}

fn new_store() -> (tempfile::TempDir, BackupStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = BackupStore::new(dir.path());
    (dir, store)
}

#[test]
fn new_files_are_uploaded_shared_and_indexed_after_one_backup() {
    let (_dir, store) = new_store();
    let mut cloud = MemoryCloud::new();
    let (local, files) = local_files(&[("a.png", "A"), ("sub/b.png", "B")]);
    let mut seen = Vec::new();
    let plan = plan_push(&cloud, &local, "game", false, &IgnoreSet::empty(), 0).unwrap();
    let out = push(
        &mut cloud,
        &store,
        &plan,
        &reader(&files),
        UNIX_EPOCH,
        "d",
        &mut |s| seen.push(s.path.clone()),
    )
    .unwrap();
    assert_eq!(out.uploaded, ["a.png", "sub/b.png"]);
    assert_eq!(seen, ["a.png", "sub/b.png"]);
    assert!(out.deleted.is_empty());
    assert_eq!(store.list().unwrap().len(), 1);

    let (entries, folders, names) = state(&cloud);
    assert_eq!(
        entries,
        [
            (
                "game".to_owned(),
                "a.png".to_owned(),
                cloud_key(&sha1_hex(b"A"), "a.png")
            ),
            (
                "game\\sub".to_owned(),
                "b.png".to_owned(),
                cloud_key(&sha1_hex(b"B"), "b.png")
            ),
        ]
    );
    assert_eq!(folders, ["game", "game\\sub"]);
    assert_eq!(names.len(), 2);
    let index = CloudIndex::load(&cloud).unwrap();
    assert!(index.entries.iter().all(|e| e.url.is_some()));
    assert_eq!(count(&cloud, Op::Delete), 0);

    // Pushing again changes nothing and takes no backup.
    cloud.clear_calls();
    let plan = plan_push(&cloud, &local, "game", false, &IgnoreSet::empty(), 0).unwrap();
    assert!(plan.is_noop());
    let out = push(
        &mut cloud,
        &store,
        &plan,
        &reader(&files),
        UNIX_EPOCH,
        "d",
        &mut |_| {},
    )
    .unwrap();
    assert!(out.backup.is_none());
    assert!(cloud.mutating_calls().is_empty());
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn nothing_is_deleted_without_prune() {
    let (_dir, store) = new_store();
    let mut cloud = cloud_with(&[("Game", "old.png", "O"), ("Game", "chg.png", "1")]);
    let (local, files) = local_files(&[("new.png", "N"), ("chg.png", "2")]);
    let out = run(&mut cloud, &store, &local, &files, false).unwrap();
    assert_eq!(out.uploaded, ["new.png"]);
    assert_eq!(count(&cloud, Op::Delete), 0);
    let (entries, _, _) = state(&cloud);
    let names: Vec<_> = entries.iter().map(|e| e.1.as_str()).collect();
    assert_eq!(names, ["chg.png", "new.png", "old.png"]);
    // The changed file keeps its old version.
    assert!(
        entries
            .iter()
            .any(|e| e.2 == cloud_key(&sha1_hex(b"1"), "chg.png"))
    );
}

#[test]
fn prune_deletes_removed_files_and_replaces_changed_ones() {
    let (_dir, store) = new_store();
    let mut cloud = cloud_with(&[
        ("Game", "old.png", "O"),
        ("Game", "chg.png", "1"),
        ("Game\\sub", "gone.png", "G"),
        ("Other", "keep.png", "K"),
    ]);
    let (local, files) = local_files(&[("chg.png", "2")]);
    let out = run(&mut cloud, &store, &local, &files, true).unwrap();
    assert_eq!(out.uploaded, ["chg.png"]);
    let mut deleted = out.deleted.clone();
    deleted.sort();
    let mut expected = [
        cloud_key(&sha1_hex(b"1"), "chg.png"),
        cloud_key(&sha1_hex(b"G"), "gone.png"),
        cloud_key(&sha1_hex(b"O"), "old.png"),
    ];
    expected.sort();
    assert_eq!(deleted, expected);
    let (entries, folders, names) = state(&cloud);
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|e| e.1 == "keep.png"));
    assert!(
        entries
            .iter()
            .any(|e| e.2 == cloud_key(&sha1_hex(b"2"), "chg.png") && e.0 == "Game")
    );
    assert_eq!(names.len(), 2);
    // Empty folders stay.
    assert!(folders.contains(&"Game\\sub".to_owned()));
    assert_eq!(store.list().unwrap().len(), 1);
    // Everything matches now.
    let plan = plan_push(&cloud, &local, "Game", true, &IgnoreSet::empty(), 0).unwrap();
    assert!(plan.sync.is_in_sync());
}

#[test]
fn changed_file_keeps_the_old_cloud_file_while_another_entry_uses_it() {
    let (_dir, store) = new_store();
    let mut cloud = cloud_with(&[("Game", "chg.png", "1"), ("Elsewhere", "chg.png", "1")]);
    let (local, files) = local_files(&[("chg.png", "2")]);
    let out = run(&mut cloud, &store, &local, &files, true).unwrap();
    assert!(out.deleted.is_empty());
    assert!(
        cloud
            .contents(&cloud_key(&sha1_hex(b"1"), "chg.png"))
            .is_some()
    );
}

#[test]
fn rename_uploads_then_deletes_the_old_path() {
    let (_dir, store) = new_store();
    let mut cloud = cloud_with(&[("Game", "old.png", "same")]);
    let (local, files) = local_files(&[("new.png", "same")]);
    let plan = plan_push(&cloud, &local, "Game", true, &IgnoreSet::empty(), 0).unwrap();
    assert_eq!(plan.sync.count(Action::Rename), 1);
    let mut steps = 0;
    let out = push(
        &mut cloud,
        &store,
        &plan,
        &reader(&files),
        UNIX_EPOCH,
        "d",
        &mut |_| steps += 1,
    )
    .unwrap();
    assert_eq!(steps, 1);
    assert_eq!(out.uploaded, ["new.png"]);
    assert_eq!(out.deleted.len(), 1);
    let (entries, _, names) = state(&cloud);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, "new.png");
    assert_eq!(names, [cloud_key(&sha1_hex(b"same"), "new.png")]);
}

#[test]
fn planning_changes_nothing() {
    let (_dir, store) = new_store();
    let cloud = cloud_with(&[("Game", "a.png", "A")]);
    let (local, _) = local_files(&[("a.png", "A"), ("b.png", "B")]);
    let plan = plan_push(&cloud, &local, "Game", true, &IgnoreSet::empty(), 0).unwrap();
    assert!(!plan.is_noop());
    assert!(cloud.mutating_calls().is_empty());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn conflicts_are_refused_before_any_change() {
    let (_dir, store) = new_store();
    let mut cloud = MemoryCloud::new();
    let (local, files) = local_files(&[("x.png", "1"), ("X.png", "2")]);
    let plan = plan_push(&cloud, &local, "Game", false, &IgnoreSet::empty(), 0).unwrap();
    assert!(plan.sync.has_conflicts());
    let err = push(
        &mut cloud,
        &store,
        &plan,
        &reader(&files),
        UNIX_EPOCH,
        "d",
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(err, PushError::Conflicts { count: 2 }));
    assert!(cloud.mutating_calls().is_empty());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn quota_is_checked_before_any_change() {
    let (_dir, store) = new_store();
    let cloud = MemoryCloud::new().with_quota(3);
    let (local, _) = local_files(&[("a.png", "12345")]);
    let err = plan_push(&cloud, &local, "Game", false, &IgnoreSet::empty(), 0).unwrap_err();
    assert!(matches!(
        err,
        PushError::Cloud(CloudError::QuotaExceeded { .. })
    ));
    assert!(cloud.mutating_calls().is_empty());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn a_changed_or_unreadable_local_file_stops_before_the_backup() {
    let (_dir, store) = new_store();
    let mut cloud = MemoryCloud::new();
    let (local, _) = local_files(&[("a.png", "A"), ("b.png", "B")]);
    let (_, edited) = local_files(&[("a.png", "A"), ("b.png", "edited")]);
    let err = run(&mut cloud, &store, &local, &edited, false).unwrap_err();
    assert!(matches!(err, PushError::LocalChanged { ref path } if path == "b.png"));
    let (_, missing) = local_files(&[("a.png", "A")]);
    let err = run(&mut cloud, &store, &local, &missing, false).unwrap_err();
    assert!(matches!(err, PushError::ReadLocal { ref path, .. } if path == "b.png"));
    assert!(cloud.mutating_calls().is_empty());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn a_plan_that_no_longer_matches_the_cloud_is_refused() {
    let (_dir, store) = new_store();
    let mut cloud = MemoryCloud::new();
    let (local, files) = local_files(&[("a.png", "A")]);
    let plan = plan_push(&cloud, &local, "Game", false, &IgnoreSet::empty(), 0).unwrap();
    // Someone else pushes the same file in the meantime.
    run(&mut cloud, &store, &local, &files, false).unwrap();
    let err = push(
        &mut cloud,
        &store,
        &plan,
        &reader(&files),
        UNIX_EPOCH,
        "d",
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(err, PushError::PlanStale));
}

/// Cloud and local state with every kind of step.
fn mixed() -> (MemoryCloud, Vec<LocalFile>, Files) {
    let cloud = cloud_with(&[
        ("Game", "same.png", "S"),
        ("Game", "chg.png", "1"),
        ("Game", "old.png", "moved"),
        ("Game\\sub", "gone.png", "G"),
        ("Other", "keep.png", "K"),
    ]);
    let (local, files) = local_files(&[
        ("same.png", "S"),
        ("chg.png", "2"),
        ("new.png", "moved"),
        ("deep/er/x.png", "X"),
        ("sub/y.png", "Y"),
    ]);
    (cloud, local, files)
}

#[test]
fn every_failure_half_way_can_be_repeated() {
    // The reference: an uninterrupted run. Its calls are the failure points.
    let (_dir, store) = new_store();
    let (mut clean, local, files) = mixed();
    run(&mut clean, &store, &local, &files, true).unwrap();
    let expected = state(&clean);
    let mut points: Vec<(Op, String)> = Vec::new();
    for c in clean.mutating_calls() {
        let point = (c.op, c.name.unwrap());
        if !points.contains(&point) {
            points.push(point);
        }
    }
    assert!(points.len() >= 8, "{points:?}");

    for (op, name) in points {
        let (_dir, store) = new_store();
        let (mut cloud, local, files) = mixed();
        cloud.fail_once(op, Some(&name), CloudError::Backend("boom".to_owned()));
        let err = run(&mut cloud, &store, &local, &files, true).unwrap_err();
        let PushError::Incomplete { backup, .. } = &err else {
            panic!("{op:?} {name}: {err:?}");
        };
        assert!(store.get(backup).is_ok());
        assert!(err.to_string().contains("run the same push again"));
        let is_index = |n: &str| n == CLOUD_INFO_FILE || n == CLOUD_FOLDER_FILE;
        let written_before: Vec<String> = cloud
            .calls()
            .into_iter()
            .filter(|c| c.op == Op::Write)
            .filter_map(|c| c.name)
            .filter(|n| !is_index(n))
            .collect();

        cloud.clear_calls();
        run(&mut cloud, &store, &local, &files, true)
            .unwrap_or_else(|e| panic!("{op:?} {name}: rerun failed: {e:?}"));
        assert_eq!(state(&cloud), expected, "{op:?} {name}");
        // A file that made it into the cloud is not written again, unless
        // the failed call was its write.
        for n in cloud
            .calls()
            .iter()
            .filter(|c| c.op == Op::Write)
            .filter_map(|c| c.name.as_ref())
            .filter(|n| !is_index(n))
        {
            let failed_write = op == Op::Write && n.eq_ignore_ascii_case(&name);
            assert!(
                failed_write || !written_before.contains(n),
                "{op:?} {name}: {n} written twice"
            );
        }
        let plan = plan_push(&cloud, &local, "Game", true, &IgnoreSet::empty(), 0).unwrap();
        assert!(plan.sync.is_in_sync(), "{op:?} {name}");
    }
}

#[test]
fn a_failing_index_write_names_the_index() {
    let (_dir, store) = new_store();
    let mut cloud = cloud_with(&[("Game", "other.png", "O")]);
    let (local, files) = local_files(&[("a.png", "A"), ("other.png", "O")]);
    cloud.fail(
        Op::Write,
        Some(CLOUD_INFO_FILE),
        CloudError::Backend("no".to_owned()),
    );
    let err = run(&mut cloud, &store, &local, &files, false).unwrap_err();
    let PushError::Incomplete {
        failed, uploaded, ..
    } = err
    else {
        panic!("{err:?}");
    };
    assert_eq!(failed, "the index");
    assert_eq!(uploaded, 0);
    // The cloud file exists; clearing the failure finishes the job.
    cloud.clear_failures();
    cloud.clear_calls();
    run(&mut cloud, &store, &local, &files, false).unwrap();
    assert!(!cloud.calls().iter().any(|c| {
        c.op == Op::Write
            && c.name
                .as_deref()
                .is_some_and(|n| n.to_ascii_lowercase().ends_with("a.png"))
    }));
    assert_eq!(state(&cloud).0.len(), 2);
}

#[test]
fn prune_leaves_ignored_cloud_files_alone() {
    let (_dir, store) = new_store();
    let mut cloud = cloud_with(&[("Game", "keep.png", "K"), ("Game", "cache.log", "C")]);
    let (local, files) = local_files(&[("keep.png", "K"), ("new.png", "N")]);
    let ignore = IgnoreSet::new(&["*.log".to_owned()]).unwrap();
    let plan = plan_push(&cloud, &local, "Game", true, &ignore, 2).unwrap();
    assert_eq!(plan.sync.count(Action::Delete), 0);
    assert!(plan.sync.steps.iter().all(|s| s.path != "cache.log"));
    assert_eq!(plan.sync.ignored_cloud, 1);
    assert_eq!(plan.sync.ignored_local, 2);
    let out = push(
        &mut cloud,
        &store,
        &plan,
        &reader(&files),
        UNIX_EPOCH,
        "d",
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(out.uploaded, ["new.png"]);
    assert!(out.deleted.is_empty());
    let (entries, _, names) = state(&cloud);
    assert!(entries.iter().any(|e| e.1 == "cache.log"));
    assert!(names.iter().any(|n| n.ends_with("_cache.log")));
}
