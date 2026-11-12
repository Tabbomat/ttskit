//! Comparing a local directory with a cloud folder.
//!
//! [`scan_local`] reads the local side (paths, sizes, SHA1s) and [`compare`]
//! matches it with the cloud index, without any I/O. Files correspond by
//! folder and display name; the content is compared by SHA1, which is the
//! first part of the cloud key.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::{CloudFile, CloudIndex, IndexEntry, normalize_folder, sha1_hex};

/// Errors from scanning a local directory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StatusError {
    /// A local file operation failed.
    #[error("{}: {message}", path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// A file or directory name is not valid UTF-8.
    #[error("{}: the name is not valid UTF-8", path.display())]
    NotUtf8 {
        /// The path involved.
        path: PathBuf,
    },
    /// The path is not a directory.
    #[error("{}: not a directory", path.display())]
    NotADirectory {
        /// The path involved.
        path: PathBuf,
    },
}

fn io_err(path: &Path, e: &std::io::Error) -> StatusError {
    StatusError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// A file found in the local directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    /// Path relative to the scanned directory, `/` separated, e.g. `sub/a.png`.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA1 of the content, 40 uppercase hexadecimal digits.
    pub sha1: String,
}

pub(crate) fn lower_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_ascii_lowercase()
        .cmp(&b.to_ascii_lowercase())
        .then_with(|| a.cmp(b))
}

/// Scans `dir` recursively.
///
/// Files and directories whose name starts with `.` and symbolic links are
/// skipped. The result is sorted by path, ignoring ASCII case first.
pub fn scan_local(dir: &Path) -> Result<Vec<LocalFile>, StatusError> {
    let meta = fs::metadata(dir).map_err(|e| io_err(dir, &e))?;
    if !meta.is_dir() {
        return Err(StatusError::NotADirectory {
            path: dir.to_path_buf(),
        });
    }
    let mut files = Vec::new();
    scan_into(dir, "", &mut files)?;
    files.sort_by(|a, b| lower_cmp(&a.path, &b.path));
    Ok(files)
}

fn scan_into(dir: &Path, prefix: &str, files: &mut Vec<LocalFile>) -> Result<(), StatusError> {
    for entry in fs::read_dir(dir).map_err(|e| io_err(dir, &e))? {
        let entry = entry.map_err(|e| io_err(dir, &e))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| StatusError::NotUtf8 { path: path.clone() })?;
        if name.starts_with('.') {
            continue;
        }
        let kind = entry.file_type().map_err(|e| io_err(&path, &e))?;
        if kind.is_symlink() {
            continue;
        }
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if kind.is_dir() {
            scan_into(&path, &rel, files)?;
        } else if kind.is_file() {
            let data = fs::read(&path).map_err(|e| io_err(&path, &e))?;
            files.push(LocalFile {
                path: rel,
                size: u64::try_from(data.len()).unwrap_or(u64::MAX),
                sha1: sha1_hex(&data),
            });
        }
    }
    Ok(())
}

/// How a file compares between the local directory and the cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// Local only: no cloud entry has this folder and name.
    New,
    /// Both exist but the content differs.
    Changed,
    /// Cloud only: no local file has this folder and name.
    Missing,
    /// Same content, and the cloud file exists.
    Identical,
    /// Same content, but the index entry's cloud file is gone.
    Broken,
}

impl FileState {
    /// All states, in the order they are reported.
    pub const ALL: [FileState; 5] = [
        FileState::New,
        FileState::Changed,
        FileState::Missing,
        FileState::Broken,
        FileState::Identical,
    ];

    /// The lowercase name: `new`, `changed`, `missing`, `identical`, `broken`.
    pub fn as_str(self) -> &'static str {
        match self {
            FileState::New => "new",
            FileState::Changed => "changed",
            FileState::Missing => "missing",
            FileState::Identical => "identical",
            FileState::Broken => "broken",
        }
    }
}

/// One file in a [`StatusReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRow {
    /// Path relative to the cloud folder, `/` separated.
    pub path: String,
    /// The comparison result.
    pub state: FileState,
    /// Size of the local file; `None` for [`FileState::Missing`].
    pub local_size: Option<u64>,
    /// SHA1 of the local file; `None` for [`FileState::Missing`].
    pub local_sha1: Option<String>,
    /// The cloud key of the corresponding entry; `None` for [`FileState::New`].
    pub cloud_name: Option<String>,
    /// The URL of that entry, if it has one.
    pub url: Option<String>,
}

/// The result of [`compare`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    /// The cloud folder, `/` separated, with the spelling of the existing
    /// folder if there is one; empty for the root.
    pub folder: String,
    /// Whether the cloud folder exists (the root always does).
    pub folder_exists: bool,
    /// One row per file, sorted by path ignoring ASCII case.
    pub rows: Vec<StatusRow>,
}

impl StatusReport {
    /// Whether every row is [`FileState::Identical`].
    pub fn is_in_sync(&self) -> bool {
        self.rows.iter().all(|r| r.state == FileState::Identical)
    }

    /// The number of rows in `state`.
    pub fn count(&self, state: FileState) -> usize {
        self.rows.iter().filter(|r| r.state == state).count()
    }
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

/// Compares `local` (from [`scan_local`]) with the cloud folder `folder`.
///
/// Everything compares ignoring ASCII case. A local file `sub/a.png`
/// corresponds to the index entries named `a.png` in the cloud folder `sub`
/// below `folder`. Entries outside `folder` and cloud files without an entry
/// are ignored. A cloud folder that does not exist yet is fine: all local
/// files are then new.
pub fn compare(
    index: &CloudIndex,
    cloud_files: &[CloudFile],
    folder: &str,
    local: &[LocalFile],
) -> StatusReport {
    let wanted = normalize_folder(folder);
    let root_lower = wanted.to_ascii_lowercase();
    let (resolved, folder_exists) = if wanted.is_empty() {
        (String::new(), true)
    } else {
        match index
            .all_folders()
            .into_iter()
            .find(|f| f.eq_ignore_ascii_case(&wanted))
        {
            Some(existing) => (existing, true),
            None => (wanted.clone(), false),
        }
    };

    let present: HashSet<String> = cloud_files
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();

    // Entries under the folder, by (relative folder, name) lowercased, in
    // index order, together with the relative folder as spelled.
    let mut placed: Vec<((String, String), &IndexEntry, String)> = Vec::new();
    let mut by_place: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for entry in &index.entries {
        let full = normalize_folder(&entry.folder);
        let full_lower = full.to_ascii_lowercase();
        let rel = if full_lower == root_lower {
            ""
        } else if root_lower.is_empty() {
            full.as_str()
        } else if full_lower
            .strip_prefix(&root_lower)
            .is_some_and(|r| r.starts_with('/'))
        {
            &full[root_lower.len() + 1..]
        } else {
            continue;
        };
        let place = (rel.to_ascii_lowercase(), entry.name.to_ascii_lowercase());
        by_place
            .entry(place.clone())
            .or_default()
            .push(placed.len());
        placed.push((place, entry, rel.to_owned()));
    }

    let mut rows = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for file in local {
        let (rel_folder, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
        let place = (
            normalize_folder(rel_folder).to_ascii_lowercase(),
            name.to_ascii_lowercase(),
        );
        let matches: Vec<&IndexEntry> = by_place
            .get(&place)
            .map(|ids| ids.iter().map(|&i| placed[i].1).collect())
            .unwrap_or_default();
        let same = |e: &IndexEntry| e.sha1().eq_ignore_ascii_case(&file.sha1);
        let (state, entry) = if let Some(e) = matches
            .iter()
            .find(|e| same(e) && present.contains(&e.key.to_ascii_lowercase()))
        {
            (FileState::Identical, Some(*e))
        } else if let Some(e) = matches.iter().find(|e| same(e)) {
            (FileState::Broken, Some(*e))
        } else if let Some(e) = matches.first() {
            (FileState::Changed, Some(*e))
        } else {
            (FileState::New, None)
        };
        seen.insert(place);
        rows.push(StatusRow {
            path: file.path.clone(),
            state,
            local_size: Some(file.size),
            local_sha1: Some(file.sha1.clone()),
            cloud_name: entry.map(|e| e.key.clone()),
            url: entry.and_then(|e| e.url.clone()),
        });
    }

    for (place, e, rel) in &placed {
        if !seen.contains(place) {
            rows.push(StatusRow {
                path: join(rel, &e.name),
                state: FileState::Missing,
                local_size: None,
                local_sha1: None,
                cloud_name: Some(e.key.clone()),
                url: e.url.clone(),
            });
        }
    }

    rows.sort_by(|a, b| lower_cmp(&a.path, &b.path));
    StatusReport {
        folder: resolved,
        folder_exists,
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(data: &str) -> String {
        sha1_hex(data.as_bytes())
    }

    fn local(path: &str, data: &str) -> LocalFile {
        LocalFile {
            path: path.to_owned(),
            size: data.len() as u64,
            sha1: sha(data),
        }
    }

    /// An entry whose key carries the SHA1 of `data`.
    fn entry(data: &str, name: &str, folder: &str) -> IndexEntry {
        IndexEntry::new(
            format!("{}_{name}", sha(data)),
            name,
            Some(format!("https://u/{name}")),
            Some(1),
            None,
            folder,
        )
    }

    fn cloud(entries: &[&IndexEntry]) -> Vec<CloudFile> {
        entries
            .iter()
            .map(|e| CloudFile {
                name: e.key.clone(),
                size: 1,
            })
            .collect()
    }

    fn index(entries: Vec<IndexEntry>, folders: &[&str]) -> CloudIndex {
        let mut index = CloudIndex::empty();
        index.entries = entries;
        index.folders = folders.iter().map(|f| (*f).to_owned()).collect();
        index
    }

    fn states(report: &StatusReport) -> Vec<(&str, FileState)> {
        report
            .rows
            .iter()
            .map(|r| (r.path.as_str(), r.state))
            .collect()
    }

    #[test]
    fn every_state() {
        let same = entry("one", "same.png", "F");
        let changed = entry("old", "changed.png", "F");
        let broken = entry("three", "broken.png", "F");
        let missing = entry("four", "missing.png", "F");
        let idx = index(
            vec![same.clone(), changed.clone(), broken, missing.clone()],
            &["F"],
        );
        let files = cloud(&[&same, &changed, &missing]);
        let report = compare(
            &idx,
            &files,
            "F",
            &[
                local("same.png", "one"),
                local("changed.png", "new"),
                local("broken.png", "three"),
                local("new.png", "five"),
            ],
        );
        assert_eq!(
            states(&report),
            [
                ("broken.png", FileState::Broken),
                ("changed.png", FileState::Changed),
                ("missing.png", FileState::Missing),
                ("new.png", FileState::New),
                ("same.png", FileState::Identical),
            ]
        );
        assert!(!report.is_in_sync());
        assert_eq!(report.count(FileState::New), 1);
        let changed_row = &report.rows[1];
        assert_eq!(
            changed_row.cloud_name.as_deref(),
            Some(changed.key.as_str())
        );
        assert_eq!(changed_row.url.as_deref(), Some("https://u/changed.png"));
        assert_eq!(changed_row.local_sha1.as_deref(), Some(sha("new").as_str()));
        let missing_row = &report.rows[2];
        assert_eq!(missing_row.local_size, None);
        assert_eq!(missing_row.local_sha1, None);
        assert_eq!(report.rows[3].cloud_name, None);
        assert_eq!(report.folder, "F");
        assert!(report.folder_exists);
    }

    #[test]
    fn nested_folders_and_case_insensitive_matching() {
        let a = entry("a", "A.png", "Decks\\Sub");
        let b = entry("b", "b.png", "decks");
        let idx = index(vec![a.clone(), b.clone()], &["Decks", "Decks\\Sub"]);
        let files = cloud(&[&a, &b]);
        let report = compare(
            &idx,
            &files,
            "DECKS",
            &[local("sub/a.PNG", "a"), local("B.png", "b")],
        );
        assert!(report.is_in_sync());
        assert_eq!(report.folder, "Decks");
        assert_eq!(report.rows.len(), 2);
    }

    #[test]
    fn duplicate_names_with_one_identical_are_identical() {
        let old = entry("old", "a.png", "");
        let cur = entry("cur", "a.png", "");
        let idx = index(vec![old.clone(), cur.clone()], &[]);
        let files = cloud(&[&old, &cur]);
        let report = compare(&idx, &files, "", &[local("a.png", "cur")]);
        assert_eq!(states(&report), [("a.png", FileState::Identical)]);
        assert_eq!(report.rows[0].cloud_name.as_deref(), Some(cur.key.as_str()));
        // Neither matches: Changed refers to the first entry.
        let report = compare(&idx, &files, "", &[local("a.png", "other")]);
        assert_eq!(report.rows[0].state, FileState::Changed);
        assert_eq!(report.rows[0].cloud_name.as_deref(), Some(old.key.as_str()));
    }

    #[test]
    fn deck_does_not_match_decks() {
        let decks = entry("x", "x.png", "Decks");
        let deck = entry("y", "y.png", "Deck");
        let idx = index(vec![decks.clone(), deck.clone()], &["Decks", "Deck"]);
        let files = cloud(&[&decks, &deck]);
        let report = compare(&idx, &files, "Deck", &[]);
        assert_eq!(states(&report), [("y.png", FileState::Missing)]);
        let report = compare(&idx, &files, "Decks", &[]);
        assert_eq!(states(&report), [("x.png", FileState::Missing)]);
    }

    #[test]
    fn root_folder_covers_everything() {
        let a = entry("a", "a.png", "");
        let b = entry("b", "b.png", "Sub\\Deep");
        let idx = index(vec![a.clone(), b.clone()], &["Sub", "Sub\\Deep"]);
        let files = cloud(&[&a, &b]);
        for root in ["", "/"] {
            let report = compare(&idx, &files, root, &[local("a.png", "a")]);
            assert_eq!(
                states(&report),
                [
                    ("a.png", FileState::Identical),
                    ("Sub/Deep/b.png", FileState::Missing)
                ]
            );
            assert_eq!(report.folder, "");
            assert!(report.folder_exists);
        }
    }

    #[test]
    fn folder_that_does_not_exist_makes_everything_new() {
        let a = entry("a", "a.png", "");
        let idx = index(vec![a.clone()], &[]);
        let report = compare(
            &idx,
            &cloud(&[&a]),
            "Not\\There",
            &[local("a.png", "a"), local("s/b.png", "b")],
        );
        assert_eq!(
            states(&report),
            [("a.png", FileState::New), ("s/b.png", FileState::New)]
        );
        assert_eq!(report.folder, "Not/There");
        assert!(!report.folder_exists);
    }

    #[test]
    fn folder_known_only_through_an_entry_exists() {
        let a = entry("a", "a.png", "Only\\Entry");
        let idx = index(vec![a.clone()], &[]);
        let report = compare(&idx, &cloud(&[&a]), "only", &[]);
        assert!(report.folder_exists);
        assert_eq!(report.folder, "Only");
        assert_eq!(states(&report), [("Entry/a.png", FileState::Missing)]);
    }

    #[test]
    fn empty_comparison_is_in_sync() {
        let report = compare(&CloudIndex::empty(), &[], "", &[]);
        assert!(report.is_in_sync());
        assert!(report.rows.is_empty());
    }

    #[test]
    fn state_names() {
        let names: Vec<_> = FileState::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(names, ["new", "changed", "missing", "broken", "identical"]);
    }

    #[test]
    fn scan_skips_dot_entries_recurses_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::create_dir_all(p.join("sub/deep")).unwrap();
        fs::create_dir_all(p.join(".git")).unwrap();
        fs::write(p.join("b.png"), "abc").unwrap();
        fs::write(p.join("A.png"), "").unwrap();
        fs::write(p.join(".hidden"), "x").unwrap();
        fs::write(p.join(".git/config"), "x").unwrap();
        fs::write(p.join("sub/c.txt"), "abc").unwrap();
        fs::write(p.join("sub/deep/d.txt"), "d").unwrap();
        let files = scan_local(p).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["A.png", "b.png", "sub/c.txt", "sub/deep/d.txt"]);
        assert_eq!(files[1].size, 3);
        assert_eq!(files[1].sha1, "A9993E364706816ABA3E25717850C26C9CD0D89D");
        assert_eq!(files[0].size, 0);
    }

    #[test]
    fn scan_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(matches!(
            scan_local(&missing).unwrap_err(),
            StatusError::Io { .. }
        ));
        let file = dir.path().join("f.txt");
        fs::write(&file, "x").unwrap();
        assert!(matches!(
            scan_local(&file).unwrap_err(),
            StatusError::NotADirectory { .. }
        ));
    }

    #[test]
    fn scan_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.txt");
        fs::write(&target, "x").unwrap();
        let scanned = dir.path().join("scanned");
        fs::create_dir(&scanned).unwrap();
        fs::write(scanned.join("a.txt"), "a").unwrap();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, scanned.join("link.txt"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, scanned.join("link.txt"));
        if made.is_err() {
            return; // no privilege to create symlinks here
        }
        let files = scan_local(&scanned).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt");
    }
}
