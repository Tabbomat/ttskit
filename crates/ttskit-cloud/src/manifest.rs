//! The project manifest: which local file is which shared cloud file.
//!
//! The manifest is a small JSON file, [`Manifest::FILE_NAME`], that a project
//! keeps next to its sources. It records, for each uploaded file, the local
//! path, the cloud path, the content SHA1, the size and the download URL. The
//! output is deterministic (sorted, fixed field order, trailing newline), so
//! it diffs well in version control. [`Manifest::to_tsv`] and
//! [`Manifest::import_tsv`] exchange the filename-to-URL mapping as plain
//! tab-separated text.

use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::status::lower_cmp;
use crate::{CloudIndex, IndexEntry, LocalFile, UgcUrl, normalize_folder};

/// The manifest format version this crate reads and writes.
const VERSION: u64 = 1;

/// Errors reading or writing a manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// A file operation failed.
    #[error("{}: {message}", path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// The text is not a valid manifest.
    #[error("invalid manifest: {0}")]
    Invalid(String),
    /// The manifest has a version this crate does not support.
    #[error("unsupported manifest version {0} (expected 1)")]
    UnsupportedVersion(String),
    /// An entry has a SHA1 that is not 40 hexadecimal digits.
    #[error("invalid manifest: entry {cloud_path:?} has an invalid SHA1 {sha1:?}")]
    BadSha1 {
        /// The cloud path of the entry.
        cloud_path: String,
        /// The offending value.
        sha1: String,
    },
}

/// One file in a [`Manifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    /// Local path relative to the project's local directory, `/` separated.
    /// `None` for files that only exist in the cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Cloud folder and display name, `/` separated, e.g. `MyGame/Boards/a.png`.
    pub cloud_path: String,
    /// SHA1 of the content, 40 uppercase hexadecimal digits.
    pub sha1: String,
    /// Size in bytes.
    pub size: u64,
    /// Download URL.
    pub url: String,
}

fn is_sha1(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl ManifestEntry {
    /// Builds an entry from a cloud index entry and the local path it
    /// belongs to.
    ///
    /// Returns `None` when the index entry has no URL or no size, or when its
    /// key does not start with a 40 digit SHA1.
    pub fn from_index(entry: &IndexEntry, path: Option<String>) -> Option<ManifestEntry> {
        let url = entry.url.clone()?;
        let size = entry.size?;
        let sha1 = entry.sha1();
        if !is_sha1(sha1) {
            return None;
        }
        let folder = normalize_folder(&entry.folder);
        let cloud_path = if folder.is_empty() {
            entry.name.clone()
        } else {
            format!("{folder}/{}", entry.name)
        };
        Some(ManifestEntry {
            path,
            cloud_path,
            sha1: sha1.to_ascii_uppercase(),
            size,
            url,
        })
    }

    fn order(&self, other: &Self) -> Ordering {
        lower_cmp(&self.cloud_path, &other.cloud_path)
            .then_with(|| match (&self.path, &other.path) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Less,
                (Some(_), None) => Ordering::Greater,
                (Some(a), Some(b)) => lower_cmp(a, b),
            })
            // Only for a total order; entries equal so far are dropped or
            // kept in a fixed order.
            .then_with(|| self.sha1.cmp(&other.sha1))
            .then_with(|| self.size.cmp(&other.size))
            .then_with(|| self.url.cmp(&other.url))
    }
}

#[derive(Serialize)]
struct FileOut<'a> {
    version: u64,
    entries: &'a [ManifestEntry],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIn {
    #[allow(dead_code)]
    version: serde_json::Value,
    entries: Vec<ManifestEntry>,
}

/// The set of files a project has in the cloud.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// The entries, in no particular order; output is always sorted.
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// The conventional file name of a manifest.
    pub const FILE_NAME: &'static str = "ttskit.manifest.json";

    /// Parses manifest text. Fails on malformed JSON, unknown fields, a
    /// version other than 1, or an invalid SHA1. SHA1s are normalized to
    /// uppercase.
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| ManifestError::Invalid(e.to_string()))?;
        match value.get("version") {
            Some(v) if v.as_u64() == Some(VERSION) => {}
            Some(v) => return Err(ManifestError::UnsupportedVersion(v.to_string())),
            None => {
                return Err(ManifestError::Invalid("missing field `version`".into()));
            }
        }
        let file: FileIn =
            serde_json::from_value(value).map_err(|e| ManifestError::Invalid(e.to_string()))?;
        let mut entries = file.entries;
        for e in &mut entries {
            if !is_sha1(&e.sha1) {
                return Err(ManifestError::BadSha1 {
                    cloud_path: e.cloud_path.clone(),
                    sha1: e.sha1.clone(),
                });
            }
            e.sha1.make_ascii_uppercase();
        }
        Ok(Manifest { entries })
    }

    /// Reads the manifest at `path`. A missing file gives an empty manifest.
    pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
        match fs::read_to_string(path) {
            Ok(text) => Manifest::parse(&text),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Manifest::default()),
            Err(e) => Err(io_err(path, &e)),
        }
    }

    /// Entries sorted by cloud path, then local path (ASCII case-insensitive
    /// first), without exact duplicates.
    fn sorted(&self) -> Vec<ManifestEntry> {
        let mut entries = self.entries.clone();
        entries.sort_by(ManifestEntry::order);
        entries.dedup();
        entries
    }

    /// The manifest as pretty-printed JSON with a trailing newline. The
    /// output does not depend on the order of [`Manifest::entries`].
    pub fn to_json(&self) -> String {
        let entries = self.sorted();
        let mut out = serde_json::to_string_pretty(&FileOut {
            version: VERSION,
            entries: &entries,
        })
        .expect("a manifest always serializes");
        out.push('\n');
        out
    }

    /// Writes the manifest to `path` through a temporary file next to it, so
    /// a failed write does not leave a half-written manifest.
    pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        let tmp = path.with_file_name(tmp_name);
        let result = fs::write(&tmp, self.to_json())
            .map_err(|e| io_err(&tmp, &e))
            .and_then(|()| fs::rename(&tmp, path).map_err(|e| io_err(path, &e)));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Adds `entry` or replaces the entry it supersedes.
    ///
    /// With a `path`, that is the entry with the same path. Without one, it
    /// is a path-less entry with the same cloud path and SHA1 (ignoring
    /// case). Otherwise the entry is appended.
    pub fn upsert(&mut self, entry: ManifestEntry) {
        let same = |e: &ManifestEntry| match &entry.path {
            Some(p) => e.path.as_ref() == Some(p),
            None => {
                e.path.is_none()
                    && e.cloud_path == entry.cloud_path
                    && e.sha1.eq_ignore_ascii_case(&entry.sha1)
            }
        };
        let hits: Vec<usize> = (0..self.entries.len())
            .filter(|&i| same(&self.entries[i]))
            .collect();
        match hits.split_first() {
            Some((&first, rest)) => {
                self.entries[first] = entry;
                // Keep a single entry per key.
                for &i in rest.iter().rev() {
                    self.entries.remove(i);
                }
            }
            None => self.entries.push(entry),
        }
    }

    /// Whether some entry, with or without a local path, has this cloud path
    /// and SHA1 (ignoring case).
    pub fn contains_cloud(&self, cloud_path: &str, sha1: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.cloud_path == cloud_path && e.sha1.eq_ignore_ascii_case(sha1))
    }

    /// Brings the manifest in line with the local files after an upload.
    ///
    /// For each of `local` (paths relative, `/` separated) the index entry in
    /// the cloud folder `root_folder` plus the file's directory, with the
    /// file's name, is upserted with the local path set. Folder and name
    /// compare ignoring ASCII case; only an entry with the file's SHA1
    /// counts. A local file without such an entry (or whose entry lacks a URL
    /// or size) is skipped, and any existing entry with its path is dropped,
    /// so the manifest never links a changed file to an older cloud version.
    /// Then entries whose local path is not in `local` are dropped; entries
    /// without a path are kept.
    pub fn refresh_for_local(
        &mut self,
        local: &[LocalFile],
        root_folder: &str,
        index: &CloudIndex,
    ) {
        let root = normalize_folder(root_folder);
        for file in local {
            let (dir, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
            let folder = normalize_folder(&format!("{root}/{dir}"));
            let matches: Vec<&IndexEntry> = index
                .entries
                .iter()
                .filter(|e| {
                    e.name.eq_ignore_ascii_case(name)
                        && normalize_folder(&e.folder).eq_ignore_ascii_case(&folder)
                })
                .collect();
            let chosen = matches
                .iter()
                .find(|e| e.sha1().eq_ignore_ascii_case(&file.sha1));
            match chosen.and_then(|e| ManifestEntry::from_index(e, Some(file.path.clone()))) {
                Some(entry) => self.upsert(entry),
                // No cloud file has this content: never keep a link to an
                // older version.
                None => self.entries.retain(|e| e.path.as_ref() != Some(&file.path)),
            }
        }
        self.entries.retain(|e| match &e.path {
            Some(p) => local.iter().any(|f| &f.path == p),
            None => true,
        });
    }

    /// The `filename<TAB>URL` mapping, one line per entry in sorted order,
    /// with `\n` line endings. The filename is the local path, or the cloud
    /// path for entries without one.
    pub fn to_tsv(&self) -> String {
        let mut out = String::new();
        for e in self.sorted() {
            out.push_str(e.path.as_deref().unwrap_or(&e.cloud_path));
            out.push('\t');
            out.push_str(&e.url);
            out.push('\n');
        }
        out
    }

    /// Reads a `filename<TAB>URL` mapping and resolves each URL against
    /// `index`.
    ///
    /// Blank lines and lines starting with `#` are skipped; `\r\n` line
    /// endings are fine. Every other line must have exactly one tab, a
    /// relative filename (`/` separated, without `.` or `..` components,
    /// backslashes or control characters, unique ignoring ASCII case) and a
    /// URL that [`UgcUrl::parse`] accepts and an index entry with a size
    /// carries. The result has one entry per line, with the filename as its
    /// local path. All problems are collected; if there is any, nothing is
    /// returned.
    pub fn import_tsv(text: &str, index: &CloudIndex) -> Result<Vec<ManifestEntry>, TsvError> {
        let urls: Vec<(Option<UgcUrl>, &IndexEntry)> = index
            .entries
            .iter()
            .map(|e| (e.url.as_deref().and_then(UgcUrl::parse), e))
            .collect();
        let mut imported = Vec::new();
        let mut problems = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for (i, raw) in text.split('\n').enumerate() {
            let line = i + 1;
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            if raw.trim().is_empty() || raw.starts_with('#') {
                continue;
            }
            let mut problem = |message: String| problems.push(TsvProblem { line, message });
            let mut fields = raw.split('\t');
            let (Some(name), Some(url), None) = (fields.next(), fields.next(), fields.next())
            else {
                let tabs = raw.matches('\t').count();
                problem(if tabs == 0 {
                    "expected `filename<TAB>URL`, found no tab".into()
                } else {
                    format!("expected exactly one tab, found {tabs}")
                });
                continue;
            };
            if name.is_empty() {
                problem("the filename is empty".into());
                continue;
            }
            if url.is_empty() {
                problem("the URL is empty".into());
                continue;
            }
            if let Some(why) = bad_filename(name) {
                problem(format!("invalid filename {name:?}: {why}"));
                continue;
            }
            let Some(parsed) = UgcUrl::parse(url) else {
                problem(format!("not a valid cloud URL: {url}"));
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if seen.contains(&lower) {
                problem(format!("duplicate filename {name:?}"));
                continue;
            }
            seen.push(lower);
            let found = urls.iter().find(|(u, _)| {
                u.as_ref().is_some_and(|u| {
                    u.handle == parsed.handle && u.sha1.eq_ignore_ascii_case(&parsed.sha1)
                })
            });
            let Some((_, entry)) = found else {
                problem(format!("{url} was not found in the cloud index"));
                continue;
            };
            match ManifestEntry::from_index(entry, Some(name.to_owned())) {
                Some(e) => imported.push(e),
                None => problem(format!(
                    "the index entry {:?} has no size or no valid SHA1",
                    entry.key
                )),
            }
        }
        if problems.is_empty() {
            Ok(imported)
        } else {
            Err(TsvError { problems })
        }
    }
}

fn io_err(path: &Path, e: &std::io::Error) -> ManifestError {
    ManifestError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// Why `name` is not an acceptable relative `/` separated filename.
fn bad_filename(name: &str) -> Option<&'static str> {
    if name.contains('\\') {
        return Some("contains a backslash");
    }
    if name.chars().any(char::is_control) {
        return Some("contains a control character");
    }
    let bytes = name.as_bytes();
    if name.starts_with('/')
        || (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic())
    {
        return Some("is absolute");
    }
    if name.split('/').any(|c| c == "..") {
        return Some("contains `..`");
    }
    if name.split('/').any(|c| c.is_empty() || c == ".") {
        return Some("has an empty or `.` component");
    }
    None
}

/// One problem found by [`Manifest::import_tsv`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsvProblem {
    /// 1-based line number.
    pub line: usize,
    /// What is wrong with the line.
    pub message: String,
}

/// All problems found by [`Manifest::import_tsv`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsvError {
    /// The problems, in line order. Never empty.
    pub problems: Vec<TsvProblem>,
}

impl fmt::Display for TsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, p) in self.problems.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "line {}: {}", p.line, p.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for TsvError {}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
