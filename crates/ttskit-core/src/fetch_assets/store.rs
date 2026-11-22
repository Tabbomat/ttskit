//! The output directory: `index.json` and the files it lists.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};

/// File name of the index inside the output directory.
pub const INDEX_FILE: &str = "index.json";
/// Directory (inside the output directory) that holds the downloaded files.
pub const FILES_DIR: &str = "files";
/// Directory for downloads in progress.
pub const PARTIAL_DIR: &str = ".partial";

const INDEX_VERSION: u64 = 1;

/// Why the output directory could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot access {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid {}: {reason}", path.display())]
    Invalid { path: PathBuf, reason: String },
}

/// One downloaded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Path relative to the output directory, with forward slashes:
    /// `files/<sha256>.<type>`.
    pub file: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the content.
    pub sha256: String,
    /// The file extension, e.g. `png`.
    pub file_type: String,
}

/// The index: URL to downloaded file, sorted by URL (byte order).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    pub files: BTreeMap<String, IndexEntry>,
}

impl Index {
    /// Parses the text of an `index.json`.
    pub fn parse(text: &str) -> Result<Index, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let obj = value.as_object().ok_or("not a JSON object")?;
        match obj.get("version").and_then(Value::as_u64) {
            Some(INDEX_VERSION) => {}
            Some(v) => return Err(format!("unsupported version {v}")),
            None => return Err("missing version".into()),
        }
        let list = obj
            .get("files")
            .and_then(Value::as_array)
            .ok_or("missing files array")?;
        let mut files = BTreeMap::new();
        for item in list {
            let field = |name: &str| item.get(name).and_then(Value::as_str);
            let (Some(url), Some(file), Some(sha256), Some(file_type)) =
                (field("url"), field("file"), field("sha256"), field("type"))
            else {
                return Err("a file entry lacks url, file, sha256 or type".into());
            };
            let Some(size) = item.get("size").and_then(Value::as_u64) else {
                return Err(format!("entry for {url} lacks a size"));
            };
            if !safe_relative(file) {
                return Err(format!("entry for {url} has an unsafe file path"));
            }
            files.insert(
                url.to_owned(),
                IndexEntry {
                    file: file.to_owned(),
                    size,
                    sha256: sha256.to_owned(),
                    file_type: file_type.to_owned(),
                },
            );
        }
        Ok(Index { files })
    }

    /// The text of `index.json`: pretty printed, LF line endings, trailing
    /// newline, sorted by URL, no timestamps.
    pub fn to_text(&self) -> String {
        let files: Vec<Value> = self
            .files
            .iter()
            .map(|(url, e)| {
                let mut m = Map::new();
                m.insert("url".into(), url.clone().into());
                m.insert("file".into(), e.file.clone().into());
                m.insert("size".into(), e.size.into());
                m.insert("sha256".into(), e.sha256.clone().into());
                m.insert("type".into(), e.file_type.clone().into());
                Value::Object(m)
            })
            .collect();
        let mut root = Map::new();
        root.insert("version".into(), INDEX_VERSION.into());
        root.insert("files".into(), Value::Array(files));
        let mut text = serde_json::to_string_pretty(&Value::Object(root))
            .expect("a JSON value always serializes");
        text.push('\n');
        text
    }
}

/// `files/<name>` with a plain name: nothing that could leave the directory.
fn safe_relative(path: &str) -> bool {
    let Some(name) = path.strip_prefix("files/") else {
        return false;
    };
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !name.contains("..")
}

/// An output directory with its loaded index.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    index: Mutex<Index>,
}

impl Store {
    /// Opens `root`. A missing directory or index counts as empty; nothing is
    /// created. An unreadable, invalid or unknown-version index is an error.
    pub fn open(root: impl Into<PathBuf>) -> Result<Store, StoreError> {
        let root = root.into();
        let path = root.join(INDEX_FILE);
        let index = match std::fs::read_to_string(&path) {
            Ok(text) => Index::parse(&text).map_err(|reason| StoreError::Invalid {
                path: path.clone(),
                reason,
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Index::default(),
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        Ok(Store {
            root,
            index: Mutex::new(index),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A copy of the current index.
    pub fn index(&self) -> Index {
        self.index.lock().unwrap().clone()
    }

    pub(crate) fn lookup(&self, url: &str) -> Option<IndexEntry> {
        self.index.lock().unwrap().files.get(url).cloned()
    }

    pub(crate) fn insert(&self, url: &str, entry: IndexEntry) {
        self.index
            .lock()
            .unwrap()
            .files
            .insert(url.to_owned(), entry);
    }

    /// Absolute path of an entry's file.
    pub fn file_path(&self, entry: &IndexEntry) -> PathBuf {
        self.root.join(&entry.file)
    }

    pub(crate) fn files_dir(&self) -> PathBuf {
        self.root.join(FILES_DIR)
    }

    pub(crate) fn partial_dir(&self) -> PathBuf {
        self.root.join(PARTIAL_DIR)
    }

    /// Writes `index.json` atomically (temporary file, then rename). Holds
    /// the index lock, so concurrent writes are serialized.
    pub fn write_index(&self) -> Result<(), StoreError> {
        let index = self.index.lock().unwrap();
        let io = |path: &Path| {
            let path = path.to_owned();
            move |source| StoreError::Io { path, source }
        };
        std::fs::create_dir_all(&self.root).map_err(io(&self.root))?;
        let target = self.root.join(INDEX_FILE);
        let temp = self.root.join(format!("{INDEX_FILE}.tmp"));
        std::fs::write(&temp, index.to_text()).map_err(io(&temp))?;
        std::fs::rename(&temp, &target).map_err(io(&target))
    }
}
