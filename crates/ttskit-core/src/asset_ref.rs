//! Asset placeholders (`asset:{PATH}`) and the map that resolves them.
//!
//! Unpacked sources may name an asset by a placeholder instead of its URL.
//! `PATH` is relative to the project's local directory and uses `/`. On pack,
//! every placeholder in a string value is replaced by the URL of the asset map.

use std::collections::BTreeMap;

/// The literal text every placeholder starts with.
pub const PLACEHOLDER_PREFIX: &str = "asset:{";

/// The placeholder for the asset at `path` (`asset:{path}`).
pub fn placeholder(path: &str) -> String {
    format!("{PLACEHOLDER_PREFIX}{path}}}")
}

/// What the asset map knows about one local asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    /// The URL the asset was pushed to.
    pub url: String,
    /// The local file changed since it was pushed.
    pub stale: bool,
}

/// Maps local asset paths to the URLs they were pushed to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetMap {
    entries: BTreeMap<String, AssetEntry>,
}

impl AssetMap {
    /// An empty map.
    pub fn new() -> AssetMap {
        AssetMap::default()
    }

    /// Adds or replaces the entry for `path`.
    pub fn insert(&mut self, path: impl Into<String>, url: impl Into<String>, stale: bool) {
        self.entries.insert(
            path.into(),
            AssetEntry {
                url: url.into(),
                stale,
            },
        );
    }

    /// The entry for `path` (matched exactly).
    pub fn get(&self, path: &str) -> Option<&AssetEntry> {
        self.entries.get(path)
    }

    /// All entries as `(path, entry)`, ordered by path.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &AssetEntry)> {
        self.entries.iter().map(|(p, e)| (p.as_str(), e))
    }

    /// True when the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Calls `f(start, end, path)` for every placeholder in `text`, in order.
/// `start..end` is the byte range of the whole placeholder.
fn scan<'a>(text: &'a str, mut f: impl FnMut(usize, usize, &'a str)) {
    let mut pos = 0;
    while let Some(found) = text[pos..].find(PLACEHOLDER_PREFIX) {
        let start = pos + found;
        let body = start + PLACEHOLDER_PREFIX.len();
        let Some(close) = text[body..].find('}') else {
            return;
        };
        let path = &text[body..body + close];
        if path.is_empty() || path.contains('\n') {
            pos = body;
            continue;
        }
        let end = body + close + 1;
        f(start, end, path);
        pos = end;
    }
}

/// The paths of all placeholders in `text`, in order, with repeats.
pub fn find_placeholders(text: &str) -> Vec<&str> {
    let mut paths = Vec::new();
    scan(text, |_, _, path| paths.push(path));
    paths
}

/// Replaces every placeholder of `text` for which `lookup` returns a value.
/// Other placeholders and all other text stay as they are.
pub fn replace_placeholders<'a>(
    text: &str,
    mut lookup: impl FnMut(&str) -> Option<&'a str>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    scan(text, |start, end, path| {
        if let Some(url) = lookup(path) {
            out.push_str(&text[copied..start]);
            out.push_str(url);
            copied = end;
        }
    });
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_placeholder() {
        assert_eq!(placeholder("Boards/a.png"), "asset:{Boards/a.png}");
    }

    #[test]
    fn finds_several() {
        let text = "a asset:{x/1.png} b asset:{y.png} asset:{x/1.png}";
        assert_eq!(find_placeholders(text), ["x/1.png", "y.png", "x/1.png"]);
    }

    #[test]
    fn ignores_malformed() {
        for text in [
            "asset:{",
            "asset:{abc",
            "asset:{}",
            "asset:{\nabc}",
            "asset:",
            "Asset:{a}",
        ] {
            assert!(find_placeholders(text).is_empty(), "{text:?}");
        }
        assert_eq!(find_placeholders("asset:{\nasset:{a}"), ["a"]);
        assert_eq!(find_placeholders("asset:{} asset:{b}"), ["b"]);
    }

    #[test]
    fn replaces_known_only() {
        let out = replace_placeholders("1 asset:{a} 2 asset:{b} 3 asset:{a}", |p| {
            (p == "a").then_some("URL")
        });
        assert_eq!(out, "1 URL 2 asset:{b} 3 URL");
    }

    #[test]
    fn replace_keeps_text_without_placeholders() {
        assert_eq!(
            replace_placeholders("héllo asset:{", |_| Some("x")),
            "héllo asset:{"
        );
    }

    #[test]
    fn map_basics() {
        let mut map = AssetMap::new();
        assert!(map.is_empty());
        map.insert("a.png", "http://x/a", false);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("a.png").unwrap().url, "http://x/a");
        assert!(map.get("A.png").is_none());
    }
}
