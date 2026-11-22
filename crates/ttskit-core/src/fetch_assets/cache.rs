//! Lookup of asset URLs in the download cache of the game.
//!
//! The game keeps downloaded assets in a `Mods` folder with one subfolder
//! per kind. A cached file is named after its URL with every character that
//! is not an ASCII letter or digit removed (case kept, scheme included),
//! followed by an extension that varies and is ignored here. No hashing or
//! truncation has been seen; none is applied.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The subfolders searched, in lookup order. Other folders (raw
/// conversions, workshop data, translations) are ignored.
pub const CACHE_FOLDERS: &[&str] = &["Images", "Models", "Assetbundles", "Audio", "PDF", "Text"];

/// Why a cache directory cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("{0} is not a directory")]
    NotADirectory(String),
    #[error(
        "{0} does not look like a Tabletop Simulator Mods folder: expected one of Images, Models, Assetbundles, Audio, PDF, Text"
    )]
    NotAModsFolder(String),
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

fn shown(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The cache key of a URL: the URL without every character that is not an
/// ASCII letter or digit.
pub fn cache_key(url: &str) -> String {
    url.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// An index of the files in the cache folders, built once.
#[derive(Debug, Clone)]
pub struct CacheDir {
    files: HashMap<String, PathBuf>,
}

impl CacheDir {
    /// Scans the known subfolders of `root` (not recursively). Fails if
    /// `root` is not a directory or has none of the subfolders.
    pub fn open(root: &Path) -> Result<CacheDir, CacheError> {
        let root_text = shown(root);
        if !root.is_dir() {
            return Err(CacheError::NotADirectory(root_text));
        }
        let mut files: HashMap<String, PathBuf> = HashMap::new();
        let mut found_folder = false;
        for folder in CACHE_FOLDERS {
            let dir = root.join(folder);
            if !dir.is_dir() {
                continue;
            }
            found_folder = true;
            let entries = std::fs::read_dir(&dir).map_err(|source| CacheError::Io {
                path: shown(&dir),
                source,
            })?;
            let mut names: Vec<(String, PathBuf)> = Vec::new();
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|t| t.is_file()) {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                names.push((name, entry.path()));
            }
            // Smallest file name first, so a duplicate stem is resolved the
            // same way on every run; the first folder wins over later ones.
            names.sort();
            for (name, path) in names {
                let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
                files.entry(stem.to_owned()).or_insert(path);
            }
        }
        if !found_folder {
            return Err(CacheError::NotAModsFolder(root_text));
        }
        Ok(CacheDir { files })
    }

    /// The cached file for `url`, whatever its extension.
    pub fn lookup(&self, url: &str) -> Option<&Path> {
        self.files.get(&cache_key(url)).map(PathBuf::as_path)
    }

    /// The lowercase extension of a cached file, if it has one.
    pub fn extension_of(path: &Path) -> Option<String> {
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
    }

    /// Number of distinct cache keys found.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}
