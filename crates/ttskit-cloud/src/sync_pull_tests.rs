use super::*;
use crate::{CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, IndexEntry, MemoryCloud, Op, cloud_key};
use std::collections::BTreeMap;

/// An in-memory directory with a log of the operations that changed it.
#[derive(Default)]
struct MemoryDir {
    files: BTreeMap<String, Vec<u8>>,
    log: Vec<String>,
    fail_write: Option<String>,
    fail_trash: Option<String>,
}

impl MemoryDir {
    fn with(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(p, d)| ((*p).to_owned(), d.as_bytes().to_vec()))
                .collect(),
            ..Self::default()
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

    fn text(&self, path: &str) -> Option<String> {
        self.files
            .get(path)
            .map(|d| String::from_utf8_lossy(d).into_owned())
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
        if self.fail_write.as_deref() == Some(path) {
            return Err(io::Error::other("disk full"));
        }
        self.log.push(format!("write {path}"));
        self.files.insert(path.to_owned(), data.to_vec());
        Ok(())
    }

    fn trash(&mut self, path: &str) -> io::Result<()> {
        if self.fail_trash.as_deref() == Some(path) {
            return Err(io::Error::other("locked"));
        }
        self.log.push(format!("trash {path}"));
        self.files
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
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

fn run(cloud: &MemoryCloud, dir: &mut MemoryDir, prune: bool) -> Result<PullOutcome, PullError> {
    let plan = plan_pull(cloud, &dir.local(), "Game", prune)?;
    pull(cloud, dir, &plan, &mut |_| {})
}

fn assert_cloud_untouched(cloud: &MemoryCloud) {
    assert_eq!(cloud.mutating_calls(), []);
    assert!(
        cloud
            .calls()
            .iter()
            .all(|c| matches!(c.op, Op::List | Op::Read))
    );
}

#[test]
fn new_changed_and_identical_files() {
    let cloud = cloud_with(&[
        ("Game", "new.png", "N"),
        ("Game", "chg.png", "cloud"),
        ("Game", "same.png", "S"),
        ("Game\\sub", "deep.png", "D"),
    ]);
    let mut dir = MemoryDir::with(&[("chg.png", "local"), ("same.png", "S")]);
    let plan = plan_pull(&cloud, &dir.local(), "Game", false).unwrap();
    let mut seen = Vec::new();
    let out = pull(&cloud, &mut dir, &plan, &mut |s| seen.push(s.path.clone())).unwrap();
    assert_eq!(out.downloaded, ["chg.png", "new.png", "sub/deep.png"]);
    assert_eq!(seen, ["chg.png", "new.png", "sub/deep.png"]);
    assert!(out.deleted.is_empty());
    assert_eq!(dir.text("chg.png").as_deref(), Some("cloud"));
    assert_eq!(dir.text("new.png").as_deref(), Some("N"));
    assert_eq!(dir.text("sub/deep.png").as_deref(), Some("D"));
    assert!(!dir.log.contains(&"write same.png".to_owned()));
    assert_cloud_untouched(&cloud);

    // A second pull has nothing to do.
    let again = run(&cloud, &mut dir, false).unwrap();
    assert!(again.downloaded.is_empty() && again.deleted.is_empty());
}

#[test]
fn nothing_is_deleted_without_prune() {
    let cloud = cloud_with(&[("Game", "a.png", "A")]);
    let mut dir = MemoryDir::with(&[("only_local.png", "L")]);
    let out = run(&cloud, &mut dir, false).unwrap();
    assert_eq!(out.downloaded, ["a.png"]);
    assert!(out.deleted.is_empty());
    assert_eq!(dir.names(), ["a.png", "only_local.png"]);
    assert!(!dir.log.iter().any(|l| l.starts_with("trash")));
}

#[test]
fn prune_trashes_local_only_files() {
    let cloud = cloud_with(&[("Game", "a.png", "A")]);
    let mut dir = MemoryDir::with(&[("a.png", "A"), ("extra.png", "X"), ("sub/old.png", "O")]);
    let plan = plan_pull(&cloud, &dir.local(), "Game", true).unwrap();
    let mut seen = Vec::new();
    let out = pull(&cloud, &mut dir, &plan, &mut |s| seen.push(s.path.clone())).unwrap();
    assert!(out.downloaded.is_empty());
    assert_eq!(out.deleted, ["extra.png", "sub/old.png"]);
    assert_eq!(seen, ["extra.png", "sub/old.png"]);
    assert_eq!(dir.names(), ["a.png"]);
    assert_cloud_untouched(&cloud);
}

#[test]
fn rename_writes_the_new_file_then_trashes_the_old_one() {
    let cloud = cloud_with(&[("Game", "new.png", "data")]);
    let mut dir = MemoryDir::with(&[("old.png", "data")]);
    let plan = plan_pull(&cloud, &dir.local(), "Game", true).unwrap();
    assert_eq!(plan.sync.count(Action::Rename), 1);
    let mut seen = Vec::new();
    let out = pull(&cloud, &mut dir, &plan, &mut |s| seen.push(s.path.clone())).unwrap();
    assert_eq!(out.downloaded, ["new.png"]);
    assert_eq!(out.deleted, ["old.png"]);
    assert_eq!(seen, ["new.png"]);
    assert_eq!(dir.log, ["write new.png", "trash old.png"]);
    assert_eq!(dir.names(), ["new.png"]);
}

#[test]
fn case_only_difference_is_skipped() {
    let cloud = cloud_with(&[("Game", "Back.png", "B")]);
    let mut dir = MemoryDir::with(&[("back.png", "B")]);
    let out = run(&cloud, &mut dir, true).unwrap();
    assert!(out.downloaded.is_empty() && out.deleted.is_empty());
    assert!(dir.log.is_empty());
    assert_eq!(dir.names(), ["back.png"]);
}

#[test]
fn changed_file_keeps_the_local_spelling() {
    let cloud = cloud_with(&[("Game", "Face.png", "cloud")]);
    let mut dir = MemoryDir::with(&[("face.png", "local")]);
    let out = run(&cloud, &mut dir, false).unwrap();
    assert_eq!(out.downloaded, ["Face.png"]);
    assert_eq!(dir.names(), ["face.png"]);
    assert_eq!(dir.text("face.png").as_deref(), Some("cloud"));
    assert_eq!(dir.log, ["write face.png"]);
}

#[test]
fn conflicts_are_refused() {
    let cloud = cloud_with(&[("Game", "a.png", "1"), ("Game", "A.png", "2")]);
    let mut dir = MemoryDir::with(&[("keep.png", "k")]);
    let err = run(&cloud, &mut dir, true).unwrap_err();
    assert!(matches!(err, PullError::Conflicts { count: 1 }), "{err}");
    assert!(dir.log.is_empty());
}

#[test]
fn stale_plan_is_refused() {
    let mut cloud = cloud_with(&[("Game", "a.png", "A"), ("Game", "b.png", "B")]);
    let mut dir = MemoryDir::default();
    let plan = plan_pull(&cloud, &dir.local(), "Game", false).unwrap();
    cloud.delete(&cloud_key(&sha1_hex(b"B"), "b.png")).unwrap();
    let err = pull(&cloud, &mut dir, &plan, &mut |_| {}).unwrap_err();
    assert!(matches!(err, PullError::PlanStale), "{err}");
    assert!(dir.log.is_empty());
}

#[test]
fn local_file_changed_after_scan_modifies_nothing() {
    let cloud = cloud_with(&[("Game", "new.png", "N"), ("Game", "chg.png", "cloud")]);
    let mut dir = MemoryDir::with(&[("chg.png", "local"), ("extra.png", "X")]);
    let plan = plan_pull(&cloud, &dir.local(), "Game", true).unwrap();

    // The file to overwrite changed.
    dir.files.insert("chg.png".to_owned(), b"edited".to_vec());
    let err = pull(&cloud, &mut dir, &plan, &mut |_| {}).unwrap_err();
    assert!(
        matches!(&err, PullError::LocalChanged { path } if path == "chg.png"),
        "{err}"
    );
    assert!(dir.log.is_empty());

    // The file to trash changed.
    dir.files.insert("chg.png".to_owned(), b"local".to_vec());
    dir.files.insert("extra.png".to_owned(), b"edited".to_vec());
    let err = pull(&cloud, &mut dir, &plan, &mut |_| {}).unwrap_err();
    assert!(
        matches!(&err, PullError::LocalChanged { path } if path == "extra.png"),
        "{err}"
    );
    assert!(dir.log.is_empty());

    // The file to trash is gone.
    dir.files.remove("extra.png");
    let err = pull(&cloud, &mut dir, &plan, &mut |_| {}).unwrap_err();
    assert!(matches!(err, PullError::LocalChanged { .. }), "{err}");
    assert!(dir.log.is_empty());
}

#[test]
fn cloud_content_with_the_wrong_sha1_is_not_written() {
    let key = cloud_key(&sha1_hex(b"B"), "b.png");
    let cloud = cloud_with(&[("Game", "a.png", "A"), ("Game", "b.png", "B")])
        .with_file(&key, b"tampered".to_vec());
    let mut dir = MemoryDir::default();
    let plan = plan_pull(&cloud, &dir.local(), "Game", false).unwrap();
    let err = pull(&cloud, &mut dir, &plan, &mut |_| {}).unwrap_err();
    let PullError::Incomplete {
        downloaded,
        failed,
        source,
        ..
    } = err
    else {
        panic!("expected Incomplete, got {err}");
    };
    assert_eq!((downloaded, failed.as_str()), (1, "b.png"));
    assert!(matches!(
        source.downcast_ref::<PullError>(),
        Some(PullError::CloudMismatch { .. })
    ));
    assert_eq!(dir.names(), ["a.png"]);
}

#[test]
fn unsafe_names_are_refused_before_anything_is_written() {
    for bad in [
        "..", "a:b", "CON", "nul.txt", "COM1.png", "lpt9", ".hidden", "a<b", "a?b", "a.", "a ",
        "x\u{1}y",
    ] {
        let cloud = cloud_with(&[("Game", "ok.png", "OK"), ("Game", bad, "BAD")]);
        let mut dir = MemoryDir::default();
        let err = run(&cloud, &mut dir, false).unwrap_err();
        assert!(
            matches!(&err, PullError::UnsafePath { path } if path == bad),
            "{bad:?}: {err}"
        );
        assert!(dir.log.is_empty(), "{bad:?}");
    }
    // Unsafe folder segments too.
    let cloud = cloud_with(&[("Game\\..", "a.png", "A")]);
    let mut dir = MemoryDir::default();
    assert!(matches!(
        run(&cloud, &mut dir, false).unwrap_err(),
        PullError::UnsafePath { .. }
    ));
    // Harmless look-alikes are fine.
    for good in ["console.png", "COM10.png", "a b.png", "x.y.png"] {
        let cloud = cloud_with(&[("Game", good, "G")]);
        let mut dir = MemoryDir::default();
        assert!(run(&cloud, &mut dir, false).is_ok(), "{good}");
    }
}

#[test]
fn rerun_after_a_failed_write_finishes_the_job() {
    let cloud = cloud_with(&[("Game", "a.png", "A"), ("Game", "b.png", "B")]);
    let mut dir = MemoryDir::with(&[("extra.png", "X")]);
    dir.fail_write = Some("b.png".to_owned());
    let err = run(&cloud, &mut dir, true).unwrap_err();
    let PullError::Incomplete {
        downloaded,
        deleted,
        failed,
        ..
    } = &err
    else {
        panic!("expected Incomplete, got {err}");
    };
    assert_eq!((*downloaded, *deleted, failed.as_str()), (1, 0, "b.png"));
    assert!(err.to_string().contains("run the same pull again"));
    // Downloads come first: the deletion has not happened.
    assert_eq!(dir.names(), ["a.png", "extra.png"]);

    dir.fail_write = None;
    let out = run(&cloud, &mut dir, true).unwrap();
    assert_eq!(out.downloaded, ["b.png"]);
    assert_eq!(out.deleted, ["extra.png"]);
    assert_eq!(dir.names(), ["a.png", "b.png"]);
}

#[test]
fn rerun_after_a_failed_trash_finishes_the_job() {
    let cloud = cloud_with(&[("Game", "new.png", "data")]);
    let mut dir = MemoryDir::with(&[("old.png", "data")]);
    dir.fail_trash = Some("old.png".to_owned());
    let err = run(&cloud, &mut dir, true).unwrap_err();
    assert!(
        matches!(&err, PullError::Incomplete { downloaded: 1, deleted: 0, failed, .. } if failed == "old.png"),
        "{err}"
    );
    assert_eq!(dir.names(), ["new.png", "old.png"]);

    dir.fail_trash = None;
    let out = run(&cloud, &mut dir, true).unwrap();
    assert!(out.downloaded.is_empty());
    assert_eq!(out.deleted, ["old.png"]);
    assert_eq!(dir.names(), ["new.png"]);
}

#[test]
fn cloud_read_failure_stops_the_run() {
    let cloud = cloud_with(&[("Game", "a.png", "A")]);
    let key = cloud_key(&sha1_hex(b"A"), "a.png");
    cloud.fail(
        Op::Read,
        Some(&key),
        CloudError::NotFound { name: key.clone() },
    );
    let mut dir = MemoryDir::default();
    let err = run(&cloud, &mut dir, false).unwrap_err();
    assert!(matches!(err, PullError::Incomplete { .. }), "{err}");
    assert!(dir.files.is_empty());
}

#[test]
fn the_cloud_is_never_changed() {
    let cloud = cloud_with(&[("Game", "a.png", "A"), ("Game", "r.png", "R")]);
    let mut dir = MemoryDir::with(&[("x.png", "X"), ("old.png", "R")]);
    run(&cloud, &mut dir, true).unwrap();
    assert!(!cloud.calls().is_empty());
    assert_cloud_untouched(&cloud);
}

#[test]
fn fs_dir_writes_replaces_and_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let mut dir = FsDir::new(tmp.path().to_path_buf());
    dir.write("sub/deep/a.txt", b"one").unwrap();
    assert_eq!(dir.read("sub/deep/a.txt").unwrap(), b"one");
    assert_eq!(
        std::fs::read(tmp.path().join("sub").join("deep").join("a.txt")).unwrap(),
        b"one"
    );
    dir.write("sub/deep/a.txt", b"two").unwrap();
    assert_eq!(dir.read("sub/deep/a.txt").unwrap(), b"two");
    // No temporary file is left behind.
    let left: Vec<_> = std::fs::read_dir(tmp.path().join("sub").join("deep"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["a.txt"]);
    assert_eq!(
        dir.read("missing.txt").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}
