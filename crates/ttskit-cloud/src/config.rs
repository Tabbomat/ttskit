//! The project config, `ttskit.toml`.
//!
//! ```toml
//! [cloud]
//! folder = "MyGame"   # cloud folder of the game; subfolders mirror local ones
//!
//! [local]
//! dir = "assets"      # relative to the directory holding ttskit.toml
//! ignore = ["*.log", "**/*_debug_*"]  # gitignore syntax, relative to dir, case-insensitive
//! ```
//!
//! With this config, `assets/Boards/a.png` belongs to the cloud folder
//! `MyGame/Boards`.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::{IgnoreSet, normalize_folder};

/// Errors from reading or validating a project config.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file could not be read.
    #[error("{}: {message}", path.display())]
    Io {
        /// The config path.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// The text is not valid TOML, has a missing or unknown key, or a value
    /// of the wrong type. The message includes the position.
    #[error("invalid config: {0}")]
    Syntax(String),
    /// A value is not acceptable.
    #[error("invalid config: `{key}` {message}")]
    Invalid {
        /// The key, e.g. `cloud.folder`.
        key: &'static str,
        /// Why the value is refused.
        message: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    cloud: RawCloud,
    local: RawLocal,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCloud {
    folder: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocal {
    dir: String,
    #[serde(default)]
    ignore: Vec<String>,
}

/// A validated project config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    /// The cloud folder, normalized: levels joined with `/`, never empty.
    pub cloud_folder: String,
    /// The local directory as written in the config, relative to the config
    /// file's directory, `/` separated.
    pub local_dir: PathBuf,
    /// Gitignore-style patterns, relative to the local directory, as written.
    pub ignore: Vec<String>,
}

fn invalid(key: &'static str, message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        key,
        message: message.into(),
    }
}

impl ProjectConfig {
    /// File name looked for in a project directory.
    pub const FILE_NAME: &'static str = "ttskit.toml";

    /// Parses and validates config text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|e| ConfigError::Syntax(e.to_string()))?;
        let cloud_folder = validate_folder(&raw.cloud.folder)?;
        let local_dir = validate_dir(&raw.local.dir)?;
        IgnoreSet::new(&raw.local.ignore)?;
        Ok(ProjectConfig {
            cloud_folder,
            local_dir,
            ignore: raw.local.ignore,
        })
    }

    /// The compiled ignore patterns. They were validated in [`Self::parse`];
    /// a config built by hand with a bad pattern yields an empty set.
    pub fn ignore_set(&self) -> IgnoreSet {
        IgnoreSet::new(&self.ignore).unwrap_or_default()
    }

    /// Reads and validates the config file at `path`.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        Self::parse(&text)
    }

    /// The local directory resolved against the directory holding the
    /// config file.
    pub fn resolve_local_dir(&self, config_dir: &Path) -> PathBuf {
        config_dir.join(&self.local_dir)
    }
}

fn validate_folder(value: &str) -> Result<String, ConfigError> {
    const KEY: &str = "cloud.folder";
    if let Some(c) = value
        .chars()
        .find(|c| c.is_control() || "<>:\"|?*".contains(*c))
    {
        return Err(invalid(KEY, format!("must not contain {c:?}")));
    }
    let folder = normalize_folder(value);
    if folder.is_empty() {
        return Err(invalid(
            KEY,
            "must name a folder (the cloud root is not allowed)",
        ));
    }
    if folder.split('/').any(|s| s == "." || s == "..") {
        return Err(invalid(KEY, "must not contain `.` or `..` levels"));
    }
    Ok(folder)
}

fn validate_dir(value: &str) -> Result<PathBuf, ConfigError> {
    const KEY: &str = "local.dir";
    if value.trim().is_empty() {
        return Err(invalid(KEY, "must not be empty"));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(KEY, "must not contain control characters"));
    }
    let path = PathBuf::from(value.replace('\\', "/"));
    if path.has_root() || value.starts_with(['/', '\\']) || value.chars().nth(1) == Some(':') {
        return Err(invalid(KEY, "must be relative to the config file"));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(invalid(KEY, "must not contain `..`"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(folder: &str, dir: &str) -> String {
        format!("[cloud]\nfolder = {folder:?}\n[local]\ndir = {dir:?}\n")
    }

    #[test]
    fn parses_and_normalizes() {
        let c = ProjectConfig::parse(&cfg("\\MyGame\\Boards/", "assets/src")).unwrap();
        assert_eq!(c.cloud_folder, "MyGame/Boards");
        assert_eq!(c.local_dir, PathBuf::from("assets/src"));
        assert_eq!(
            c.resolve_local_dir(Path::new("proj")),
            Path::new("proj").join("assets/src")
        );
    }

    #[test]
    fn backslash_dir_is_converted() {
        let c = ProjectConfig::parse(&cfg("G", "a\\b")).unwrap();
        assert_eq!(c.local_dir, PathBuf::from("a/b"));
    }

    fn invalid_key(text: &str) -> &'static str {
        match ProjectConfig::parse(text) {
            Err(ConfigError::Invalid { key, .. }) => key,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_folders() {
        for f in ["", "/", "//", "A/../B", "A/./B", "a?b", "a:b"] {
            assert_eq!(invalid_key(&cfg(f, "x")), "cloud.folder", "{f:?}");
        }
    }

    #[test]
    fn rejects_bad_dirs() {
        for d in ["", "  ", "/abs", "\\abs", "C:/x", "C:\\x", "../x", "a/../b"] {
            assert_eq!(invalid_key(&cfg("G", d)), "local.dir", "{d:?}");
        }
    }

    #[test]
    fn missing_sections_and_keys() {
        for t in [
            "",
            "[cloud]\nfolder = \"G\"\n",
            "[local]\ndir = \"x\"\n",
            "[cloud]\n[local]\ndir = \"x\"\n",
        ] {
            let e = ProjectConfig::parse(t).unwrap_err();
            assert!(matches!(e, ConfigError::Syntax(_)), "{t:?}: {e:?}");
            assert!(e.to_string().contains("missing field"), "{e}");
        }
    }

    #[test]
    fn unknown_keys_are_refused_by_name() {
        let t = format!("{}prune = true\n", cfg("G", "x"));
        let e = ProjectConfig::parse(&t).unwrap_err();
        assert!(e.to_string().contains("prune"), "{e}");
    }

    #[test]
    fn ignore_defaults_to_empty() {
        let c = ProjectConfig::parse(&cfg("G", "x")).unwrap();
        assert!(c.ignore.is_empty());
        assert!(c.ignore_set().is_empty());
    }

    #[test]
    fn ignore_list_is_parsed() {
        let t = format!("{}ignore = [\"*.log\", \"tmp/\"]\n", cfg("G", "x"));
        let c = ProjectConfig::parse(&t).unwrap();
        assert_eq!(c.ignore, ["*.log", "tmp/"]);
        let s = c.ignore_set();
        assert!(s.is_ignored("a/b.log"));
        assert!(s.is_ignored("tmp/a.png"));
        assert!(!s.is_ignored("a.png"));
    }

    #[test]
    fn invalid_ignore_pattern_is_refused() {
        // The matcher accepts an unclosed `[`; an unclosed `{` and a reversed
        // range are refused.
        for bad in ["{a", "[z-a]"] {
            let t = format!("{}ignore = [\"*.log\", {bad:?}]\n", cfg("G", "x"));
            assert_eq!(invalid_key(&t), "local.ignore", "{bad:?}");
        }
    }

    #[test]
    fn ignore_must_be_an_array() {
        let t = format!("{}ignore = \"*.log\"\n", cfg("G", "x"));
        let e = ProjectConfig::parse(&t).unwrap_err();
        assert!(matches!(e, ConfigError::Syntax(_)), "{e:?}");
    }

    #[test]
    fn unknown_local_key_is_still_refused() {
        let t = format!("{}ignores = []\n", cfg("G", "x"));
        let e = ProjectConfig::parse(&t).unwrap_err();
        assert!(e.to_string().contains("ignores"), "{e}");
    }

    #[test]
    fn syntax_error_has_position() {
        let e = ProjectConfig::parse("[cloud\nfolder = 1").unwrap_err();
        assert!(matches!(e, ConfigError::Syntax(_)));
        assert!(e.to_string().contains("line 1"), "{e}");
    }

    #[test]
    fn wrong_type_is_a_syntax_error() {
        let e = ProjectConfig::parse("[cloud]\nfolder = 3\n[local]\ndir = \"x\"\n").unwrap_err();
        assert!(matches!(e, ConfigError::Syntax(_)), "{e:?}");
    }

    #[test]
    fn load_reports_missing_file() {
        let e = ProjectConfig::load(Path::new("definitely/not/here.toml")).unwrap_err();
        assert!(matches!(e, ConfigError::Io { .. }));
    }

    #[test]
    fn load_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(ProjectConfig::FILE_NAME);
        std::fs::write(&p, cfg("G", "x")).unwrap();
        assert_eq!(ProjectConfig::load(&p).unwrap().cloud_folder, "G");
    }
}

#[cfg(test)]
mod control_char_tests {
    use super::*;

    #[test]
    fn control_characters_are_refused() {
        assert!(matches!(
            validate_folder("a\u{1}b"),
            Err(ConfigError::Invalid {
                key: "cloud.folder",
                ..
            })
        ));
        assert!(matches!(
            validate_dir("a\u{1}b"),
            Err(ConfigError::Invalid {
                key: "local.dir",
                ..
            })
        ));
    }
}
