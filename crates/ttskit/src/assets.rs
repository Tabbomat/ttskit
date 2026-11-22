//! Builds the asset map (`asset:{PATH}` placeholders) from the project config
//! and manifest.

use std::path::{Path, PathBuf};

use ttskit_cloud::{Manifest, ProjectConfig, sha1_hex};
use ttskit_core::AssetMap;

/// The config to use: the given path, else `ttskit.toml` in the current
/// directory if it exists.
pub fn find_config(config: Option<&Path>) -> Option<PathBuf> {
    match config {
        Some(path) => Some(path.to_path_buf()),
        None => {
            let default = PathBuf::from(ProjectConfig::FILE_NAME);
            default.exists().then_some(default)
        }
    }
}

/// Loads the asset map of the project. Without a config the map is empty; a
/// missing manifest file also gives an empty map. An entry is stale when its
/// local file exists and differs from the pushed content; a missing local file
/// is not stale.
pub fn load_asset_map(config: Option<&Path>) -> Result<AssetMap, String> {
    let Some(config) = find_config(config) else {
        return Ok(AssetMap::new());
    };
    let project = ProjectConfig::load(&config).map_err(|e| e.to_string())?;
    let config_dir = match config.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let manifest =
        Manifest::load(&config_dir.join(Manifest::FILE_NAME)).map_err(|e| e.to_string())?;
    let local_dir = project.resolve_local_dir(config_dir);

    // (path, url, stale, entry matches the local file)
    let mut chosen: Vec<(String, String, bool, bool)> = Vec::new();
    for entry in &manifest.entries {
        let Some(path) = &entry.path else { continue };
        let local = std::fs::read(local_dir.join(path)).ok();
        let (stale, current) = match &local {
            Some(bytes) => {
                let same = sha1_hex(bytes).eq_ignore_ascii_case(&entry.sha1);
                (!same, same)
            }
            None => (false, false),
        };
        match chosen.iter_mut().find(|c| &c.0 == path) {
            // Prefer the entry that matches the local file; else keep the first.
            Some(existing) if !existing.3 && current => {
                *existing = (path.clone(), entry.url.clone(), stale, current);
            }
            Some(_) => {}
            None => chosen.push((path.clone(), entry.url.clone(), stale, current)),
        }
    }
    let mut map = AssetMap::new();
    for (path, url, stale, _) in chosen {
        map.insert(path, url, stale);
    }
    Ok(map)
}

/// Packs `dir`, resolving `asset:{PATH}` placeholders from the project.
///
/// The tree is packed first with the map in `options` as it is. Only when that
/// fails because of placeholders is the project's map loaded (from `config` or
/// the default config) and the pack repeated, so trees without placeholders
/// never read the config or hash any asset. Errors come with a hint.
pub fn pack_resolving(
    dir: &Path,
    options: &ttskit_core::PackOptions,
    config: Option<&Path>,
) -> Result<ttskit_core::Packed, String> {
    let first = match ttskit_core::pack(dir, options) {
        Err(ttskit_core::PackError::Assets { .. }) => None,
        other => Some(other.map_err(|e| e.to_string())),
    };
    if let Some(result) = first {
        return result;
    }
    let config_found = find_config(config).is_some();
    let mut options = options.clone();
    options.assets = load_asset_map(config)?;
    ttskit_core::pack(dir, &options).map_err(|e| pack_error(&e, config_found))
}

/// The message of a pack error, with a hint for asset errors.
fn pack_error(error: &ttskit_core::PackError, config_found: bool) -> String {
    let mut message = error.to_string();
    if let ttskit_core::PackError::Assets { missing, stale } = error {
        if !missing.is_empty() && !config_found {
            message.push_str(
                "\nno ttskit.toml found in the current directory; run from the project directory or pass --config",
            );
        }
        if !stale.is_empty() {
            message.push_str(
                "\nrun `ttskit cloud sync push` to upload the changed files, or pass --allow-stale to use the old URLs",
            );
        }
    }
    message
}
