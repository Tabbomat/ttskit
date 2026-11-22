//! The cache file of the image pipeline and the errors of planning and
//! building.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Name of the cache file, in the directory of `ttskit.toml`.
pub const CACHE_FILE_NAME: &str = "ttskit-image.lock.json";

/// The name of the project config; never a source or an output.
pub(super) const CONFIG_FILE_NAME: &str = "ttskit.toml";

const FORMAT_VERSION: u32 = 1;

/// Errors from planning, building and the cache file.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// Outputs that clash with each other or with sources. Nothing was written.
    #[error("the pipeline has conflicting outputs, nothing was written:\n{}", bullets(.0))]
    Conflicts(Vec<String>),
    /// Rules that cannot be planned (a missing pngquant, a bad deck grid).
    #[error("the pipeline cannot be planned:\n{}", bullets(.0))]
    Rules(Vec<String>),
    /// The cache file cannot be used.
    #[error("{}: {message}; delete the file to rebuild everything", path.display())]
    Cache { path: PathBuf, message: String },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn bullets(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| format!("  - {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A `/` separated path from a relative path's components.
pub(super) fn rel_string(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Writes `bytes` to `target` through `<name>.ttskit-tmp` and a rename,
/// creating parent directories.
pub(super) fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".ttskit-tmp");
    let tmp = target.with_file_name(name);
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, target));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// What the last build recorded about one output file. Fields are in key
/// order so the cache file is sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheEntry {
    /// Hash of the ordered (input path, input content hash) pairs.
    pub inputs: String,
    /// Hash of the step and deck options, the output format and the pngquant
    /// version.
    pub options: String,
    /// The `src` of the rule that made the output.
    pub rule: String,
    /// SHA-256 of the output file's content.
    pub sha256: String,
    /// The tool version that made the output.
    pub version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIn {
    format_version: u32,
    outputs: BTreeMap<String, CacheEntry>,
}

#[derive(Serialize)]
struct FileOut<'a> {
    format_version: u32,
    outputs: &'a BTreeMap<String, CacheEntry>,
}

/// The cache: output path (relative to the config directory, `/` separated)
/// to what was recorded when it was built.
#[derive(Debug, Clone, Default)]
pub struct Cache {
    entries: BTreeMap<String, CacheEntry>,
}

impl Cache {
    /// Reads the cache file of `config_dir`. A missing file is an empty cache.
    pub fn load(config_dir: &Path) -> Result<Cache, BuildError> {
        let path = config_dir.join(CACHE_FILE_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Cache::default()),
            Err(e) => {
                return Err(BuildError::Cache {
                    path,
                    message: e.to_string(),
                });
            }
        };
        let fail = |message: String| BuildError::Cache {
            path: path.clone(),
            message,
        };
        let file: FileIn = serde_json::from_str(&text).map_err(|e| fail(e.to_string()))?;
        if file.format_version != FORMAT_VERSION {
            return Err(fail(format!(
                "format version {} is not supported (expected {FORMAT_VERSION})",
                file.format_version
            )));
        }
        // Prune deletes files named here, so refuse anything that could point
        // outside the config directory.
        for key in file.outputs.keys() {
            let p = Path::new(key);
            let safe = !key.is_empty()
                && !key.contains('\\')
                && p.components().all(|c| matches!(c, Component::Normal(_)));
            if !safe {
                return Err(fail(format!("`{key}` is not a relative output path")));
            }
        }
        Ok(Cache {
            entries: file.outputs,
        })
    }

    /// Writes the cache file of `config_dir` atomically: pretty JSON with
    /// sorted keys and a trailing newline.
    pub fn save(&self, config_dir: &Path) -> Result<(), BuildError> {
        let path = config_dir.join(CACHE_FILE_NAME);
        let mut text = serde_json::to_string_pretty(&FileOut {
            format_version: FORMAT_VERSION,
            outputs: &self.entries,
        })
        .map_err(|e| BuildError::Cache {
            path: path.clone(),
            message: e.to_string(),
        })?;
        text.push('\n');
        write_atomic(&path, text.as_bytes()).map_err(|source| BuildError::Io { path, source })
    }

    pub fn get(&self, output: &str) -> Option<&CacheEntry> {
        self.entries.get(output)
    }

    /// All entries, sorted by output path.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &CacheEntry)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn insert(&mut self, output: String, entry: CacheEntry) {
        // Outputs are matched case-insensitively; an entry for the same file
        // under other casing would otherwise never be dropped.
        let lower = output.to_lowercase();
        self.entries
            .retain(|key, _| key == &output || key.to_lowercase() != lower);
        self.entries.insert(output, entry);
    }

    pub(super) fn remove(&mut self, output: &str) {
        self.entries.remove(output);
    }
}
