//! Parser and encoder for the folder index the game keeps in the cloud.
//!
//! The game stores its folder tree in two BSON files: `CloudInfo.bson` maps
//! each cloud file to its display name, URL, size, date and folder, and
//! `CloudFolder.bson` lists all folder paths. [`CloudIndex`] reads both and
//! writes them back without changing anything it does not understand, so an
//! unmodified index encodes to the same bytes it was read from.

use std::fmt;

use bson::{Bson, Document};

use crate::{CloudBackend, CloudError, UgcHandle};

/// Name of the cloud file holding the per-file index.
pub const CLOUD_INFO_FILE: &str = "CloudInfo.bson";
/// Name of the cloud file holding the folder list.
pub const CLOUD_FOLDER_FILE: &str = "CloudFolder.bson";
/// Cloud files the game itself uses; they are never asset files.
pub const GAME_INTERNAL_FILES: [&str; 4] = [
    CLOUD_INFO_FILE,
    CLOUD_FOLDER_FILE,
    "WorkshopUpload",
    "WorkshopImageUpload.png",
];

/// Whether `name` is one of the [`GAME_INTERNAL_FILES`] (ASCII case-insensitive).
pub fn is_game_internal(name: &str) -> bool {
    GAME_INTERNAL_FILES
        .iter()
        .any(|f| f.eq_ignore_ascii_case(name))
}

/// Errors reading or encoding the cloud index.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IndexError {
    /// Reading an index file from the cloud failed.
    #[error(transparent)]
    Cloud(#[from] CloudError),
    /// An index file is not valid BSON, or could not be encoded.
    #[error("{file}: invalid BSON: {message}")]
    Bson {
        /// The index file.
        file: &'static str,
        /// What went wrong.
        message: String,
    },
    /// A field of an entry has an unexpected type or value.
    #[error("CloudInfo.bson: entry {key:?}: field {field} must be {expected}")]
    FieldType {
        /// The entry key.
        key: String,
        /// The field name.
        field: &'static str,
        /// A description of what the field must be.
        expected: &'static str,
    },
    /// An entry value is not a document.
    #[error("CloudInfo.bson: entry {key:?} is not a document")]
    NotADocument {
        /// The entry key.
        key: String,
    },
    /// An entry key is not of the form `<SHA1>_<name>`.
    #[error("CloudInfo.bson: entry key {key:?} is not of the form <SHA1>_<name>")]
    BadKey {
        /// The entry key.
        key: String,
    },
    /// A folder list value is not a string.
    #[error("CloudFolder.bson: folder {index} is not a string")]
    FolderType {
        /// Position in the folder list.
        index: usize,
    },
}

fn bson_err(file: &'static str, e: impl fmt::Display) -> IndexError {
    IndexError::Bson {
        file,
        message: e.to_string(),
    }
}

/// The non-empty levels of a folder path. Both `\` (what the game writes)
/// and `/` separate levels.
fn folder_segments(path: &str) -> impl Iterator<Item = &str> {
    path.split(['\\', '/']).filter(|s| !s.is_empty())
}

/// Normalizes a folder path for display and comparison: levels are
/// separated by `\` or `/`, empty levels are dropped, and the result is
/// joined with `/`. The root is the empty string.
pub fn normalize_folder(path: &str) -> String {
    folder_segments(path).collect::<Vec<_>>().join("/")
}

/// One file in `CloudInfo.bson`.
///
/// Fields the game writes that this type does not know are kept and written
/// back unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexEntry {
    /// The key, `<SHA1>_<name>`; also the name of the file in the cloud.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Download URL, see [`UgcUrl`].
    pub url: Option<String>,
    /// Size in bytes.
    pub size: Option<u64>,
    /// Date as the game wrote it; kept verbatim.
    pub date: Option<String>,
    /// Folder path as the game wrote it; empty for the root. The game
    /// separates levels with `\`; see [`normalize_folder`].
    pub folder: String,
    raw: Document,
}

fn key_has_sha1(key: &str) -> bool {
    key.split_once('_').is_some_and(|(sha, _)| !sha.is_empty())
}

fn key_name(key: &str) -> &str {
    key.split_once('_').map_or(key, |(_, n)| n)
}

impl IndexEntry {
    /// Creates an entry that is not backed by an existing document.
    pub fn new(
        key: impl Into<String>,
        name: impl Into<String>,
        url: Option<String>,
        size: Option<u64>,
        date: Option<String>,
        folder: impl Into<String>,
    ) -> Self {
        IndexEntry {
            key: key.into(),
            name: name.into(),
            url,
            size,
            date,
            folder: folder.into(),
            raw: Document::new(),
        }
    }

    /// The SHA1 part of the key (everything before the first `_`).
    pub fn sha1(&self) -> &str {
        self.key.split('_').next().unwrap_or("")
    }

    fn decode(key: &str, value: &Bson) -> Result<Self, IndexError> {
        let Bson::Document(raw) = value else {
            return Err(IndexError::NotADocument { key: key.into() });
        };
        if !key_has_sha1(key) {
            return Err(IndexError::BadKey { key: key.into() });
        }
        let string = |field: &'static str| -> Result<Option<String>, IndexError> {
            match raw.get(field) {
                None | Some(Bson::Null) => Ok(None),
                Some(Bson::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(IndexError::FieldType {
                    key: key.into(),
                    field,
                    expected: "a string",
                }),
            }
        };
        let size_err = || IndexError::FieldType {
            key: key.into(),
            field: "Size",
            expected: "a non-negative whole number",
        };
        let size = match raw.get("Size") {
            None | Some(Bson::Null) => None,
            Some(Bson::Int32(n)) => Some(u64::try_from(*n).map_err(|_| size_err())?),
            Some(Bson::Int64(n)) => Some(u64::try_from(*n).map_err(|_| size_err())?),
            Some(Bson::Double(d))
                if d.is_finite()
                    && *d >= 0.0
                    && d.fract() == 0.0
                    && *d < 18_446_744_073_709_551_616.0 =>
            {
                Some(*d as u64)
            }
            Some(_) => return Err(size_err()),
        };
        Ok(IndexEntry {
            key: key.into(),
            name: string("Name")?.unwrap_or_else(|| key_name(key).to_string()),
            url: string("URL")?,
            size,
            date: string("Date")?,
            folder: string("Folder")?.unwrap_or_default(),
            raw: raw.clone(),
        })
    }

    fn encode(&self) -> Result<Document, IndexError> {
        let mut raw = self.raw.clone();
        // Name and Folder have defaults, so an absent field stays absent
        // while it still holds the default.
        if raw.is_empty() || raw.contains_key("Name") || self.name != key_name(&self.key) {
            raw.insert("Name", self.name.clone());
        }
        put(&mut raw, "URL", self.url.clone().map(Bson::String));
        match self.size {
            Some(n) => {
                let as_double = n as f64;
                let value = match raw.get("Size") {
                    Some(Bson::Int32(_)) if i32::try_from(n).is_ok() => Bson::Int32(n as i32),
                    Some(Bson::Double(_))
                        if as_double < 18_446_744_073_709_551_616.0 && as_double as u64 == n =>
                    {
                        Bson::Double(as_double)
                    }
                    _ => Bson::Int64(i64::try_from(n).map_err(|_| {
                        bson_err(CLOUD_INFO_FILE, format!("size {n} is too large"))
                    })?),
                };
                raw.insert("Size", value);
            }
            None => put(&mut raw, "Size", None),
        }
        put(&mut raw, "Date", self.date.clone().map(Bson::String));
        if raw.contains_key("Folder") || !self.folder.is_empty() {
            raw.insert("Folder", self.folder.clone());
        }
        Ok(raw)
    }
}

/// Sets `field` in place (appending when new), or removes it for `None`.
/// An explicit null stays as it is when the value is `None`.
fn put(raw: &mut Document, field: &str, value: Option<Bson>) {
    match value {
        Some(v) => {
            raw.insert(field, v);
        }
        None => {
            if !matches!(raw.get(field), None | Some(Bson::Null)) {
                raw.remove(field);
            }
        }
    }
}

/// The cloud index: the files in `CloudInfo.bson` and the folders in
/// `CloudFolder.bson`.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudIndex {
    /// Entries in file order.
    pub entries: Vec<IndexEntry>,
    /// Folder paths in `CloudFolder.bson`, in file order. May lack ancestors
    /// and folders that only entries mention; see [`CloudIndex::all_folders`].
    pub folders: Vec<String>,
    raw_folders: Document,
    original_folders: Vec<String>,
}

impl CloudIndex {
    /// An index with no entries and no folders.
    pub fn empty() -> Self {
        CloudIndex {
            entries: Vec::new(),
            folders: Vec::new(),
            raw_folders: Document::new(),
            original_folders: Vec::new(),
        }
    }

    /// Parses the content of the two index files. `None` means the file does
    /// not exist, which gives an empty part.
    pub fn decode(info: Option<&[u8]>, folders: Option<&[u8]>) -> Result<Self, IndexError> {
        let mut index = CloudIndex::empty();
        if let Some(bytes) = info {
            let doc = Document::from_reader(bytes).map_err(|e| bson_err(CLOUD_INFO_FILE, e))?;
            for (key, value) in doc.iter() {
                index.entries.push(IndexEntry::decode(key, value)?);
            }
        }
        if let Some(bytes) = folders {
            let doc = Document::from_reader(bytes).map_err(|e| bson_err(CLOUD_FOLDER_FILE, e))?;
            for (i, value) in doc.values().enumerate() {
                let Bson::String(s) = value else {
                    return Err(IndexError::FolderType { index: i });
                };
                index.folders.push(s.clone());
            }
            index.raw_folders = doc;
            index.original_folders = index.folders.clone();
        }
        Ok(index)
    }

    /// Reads and parses both index files from the cloud. A missing file counts
    /// as empty; other read errors are returned.
    pub fn load(backend: &dyn CloudBackend) -> Result<Self, IndexError> {
        let read = |name: &str| match backend.read(name) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(CloudError::NotFound { .. }) => Ok(None),
            Err(e) => Err(IndexError::Cloud(e)),
        };
        let info = read(CLOUD_INFO_FILE)?;
        let folders = read(CLOUD_FOLDER_FILE)?;
        Self::decode(info.as_deref(), folders.as_deref())
    }

    /// Encodes the content of `CloudInfo.bson`.
    pub fn encode_info(&self) -> Result<Vec<u8>, IndexError> {
        let mut root = Document::new();
        for entry in &self.entries {
            root.insert(entry.key.clone(), Bson::Document(entry.encode()?));
        }
        root.to_vec().map_err(|e| bson_err(CLOUD_INFO_FILE, e))
    }

    /// Encodes the content of `CloudFolder.bson`.
    pub fn encode_folders(&self) -> Result<Vec<u8>, IndexError> {
        let doc = if self.folders == self.original_folders {
            self.raw_folders.clone()
        } else {
            self.folders
                .iter()
                .enumerate()
                .map(|(i, f)| (i.to_string(), Bson::String(f.clone())))
                .collect()
        };
        doc.to_vec().map_err(|e| bson_err(CLOUD_FOLDER_FILE, e))
    }

    /// Every folder that exists, root excluded: the listed folders, the
    /// folders of all entries, and all their ancestors.
    ///
    /// Paths are normalized with [`normalize_folder`], deduplicated exactly
    /// and sorted by ASCII-lowercase, then by original spelling.
    pub fn all_folders(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let paths = self
            .folders
            .iter()
            .map(String::as_str)
            .chain(self.entries.iter().map(|e| e.folder.as_str()));
        for path in paths {
            let mut current = String::new();
            for seg in folder_segments(path) {
                if !current.is_empty() {
                    current.push('/');
                }
                current.push_str(seg);
                out.push(current.clone());
            }
        }
        out.sort_by(|a, b| {
            a.to_ascii_lowercase()
                .cmp(&b.to_ascii_lowercase())
                .then_with(|| a.cmp(b))
        });
        out.dedup();
        out
    }
}

/// A parsed download URL of a shared cloud file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UgcUrl {
    /// The UGC handle.
    pub handle: UgcHandle,
    /// The SHA1 part of the URL, spelled as in the URL.
    pub sha1: String,
}

impl UgcUrl {
    const PREFIXES: [&'static str; 2] = [
        "https://steamusercontent-a.akamaihd.net/ugc/",
        "http://cloud-3.steamusercontent.com/ugc/",
    ];

    /// Parses `https://steamusercontent-a.akamaihd.net/ugc/<handle>/<sha1>/`
    /// or the older `http://cloud-3.steamusercontent.com` form. The trailing
    /// slash is optional.
    pub fn parse(url: &str) -> Option<UgcUrl> {
        let rest = Self::PREFIXES.iter().find_map(|p| url.strip_prefix(p))?;
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (handle, sha1) = rest.split_once('/')?;
        if handle.is_empty()
            || !handle.bytes().all(|b| b.is_ascii_digit())
            || sha1.is_empty()
            || !sha1.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return None;
        }
        Some(UgcUrl {
            handle: UgcHandle(handle.parse().ok()?),
            sha1: sha1.to_string(),
        })
    }
}

impl fmt::Display for UgcUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "https://steamusercontent-a.akamaihd.net/ugc/{}/{}/",
            self.handle, self.sha1
        )
    }
}
