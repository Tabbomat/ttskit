//! Gitignore-style patterns that exclude local files from cloud sync.

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::ConfigError;

/// A compiled set of ignore patterns (gitignore syntax, case-insensitive).
///
/// Paths are matched relative to the scan root, `/` separated.
#[derive(Debug, Clone)]
pub struct IgnoreSet {
    inner: Gitignore,
}

impl Default for IgnoreSet {
    fn default() -> Self {
        Self::empty()
    }
}

impl IgnoreSet {
    /// A set that ignores nothing.
    pub fn empty() -> Self {
        IgnoreSet {
            inner: Gitignore::empty(),
        }
    }

    /// Compiles `patterns`; a pattern the matcher refuses is reported as an
    /// invalid `local.ignore` value.
    pub fn new(patterns: &[String]) -> Result<Self, ConfigError> {
        let mut builder = GitignoreBuilder::new("");
        builder.case_insensitive(true).map_err(bad)?;
        for p in patterns {
            builder.add_line(None, p).map_err(bad)?;
        }
        let inner = builder.build().map_err(bad)?;
        Ok(IgnoreSet { inner })
    }

    /// True when no pattern is present.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Whether the FILE at `rel_path` (relative to the scan root, `/`
    /// separated) is ignored. Directory patterns such as `tmp/` also ignore
    /// the files below the directory.
    pub fn is_ignored(&self, rel_path: &str) -> bool {
        self.inner
            .matched_path_or_any_parents(rel_path, false)
            .is_ignore()
    }
}

fn bad(e: ignore::Error) -> ConfigError {
    ConfigError::Invalid {
        key: "local.ignore",
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(p: &[&str]) -> IgnoreSet {
        let v: Vec<String> = p.iter().map(|s| s.to_string()).collect();
        IgnoreSet::new(&v).unwrap()
    }

    #[test]
    fn star_matches_at_any_depth() {
        let s = set(&["*.log"]);
        assert!(s.is_ignored("a.log"));
        assert!(s.is_ignored("x/y/a.log"));
        assert!(!s.is_ignored("a.json"));
    }

    #[test]
    fn leading_slash_is_anchored() {
        let s = set(&["/root.json"]);
        assert!(s.is_ignored("root.json"));
        assert!(!s.is_ignored("sub/root.json"));
    }

    #[test]
    fn double_star_pattern() {
        let s = set(&["**/*_debug_*"]);
        assert!(s.is_ignored("a_debug_1.json"));
        assert!(s.is_ignored("x/y/a_debug_1.json"));
        assert!(!s.is_ignored("x/a.json"));
    }

    #[test]
    fn directory_pattern_ignores_files_below() {
        let s = set(&["tmp/"]);
        assert!(s.is_ignored("tmp/a.png"));
        assert!(s.is_ignored("x/tmp/deep/a.png"));
        assert!(!s.is_ignored("tmp.png"));
    }

    #[test]
    fn negation_after_pattern() {
        let s = set(&["*.json", "!keep.json"]);
        assert!(s.is_ignored("a.json"));
        assert!(!s.is_ignored("keep.json"));
    }

    #[test]
    fn case_insensitive() {
        assert!(set(&["*.LOG"]).is_ignored("a.log"));
        assert!(set(&["*.log"]).is_ignored("A.LOG"));
    }

    #[test]
    fn empty_set_ignores_nothing() {
        let s = IgnoreSet::empty();
        assert!(s.is_empty());
        assert!(!s.is_ignored("a.log"));
        assert!(IgnoreSet::default().is_empty());
        assert!(IgnoreSet::new(&[]).unwrap().is_empty());
        assert!(!set(&["*.log"]).is_empty());
    }
}
