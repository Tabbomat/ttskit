//! Asset URL walker.
//!
//! [`Save::asset_urls`] finds every `http://` or `https://` string anywhere in
//! a save and aggregates the occurrences per unique URL. Key names are never
//! used to decide whether a string is a URL; they only provide the label.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::Save;

/// Key holding the list of custom UI assets at the top level of a save.
const CUSTOM_UI_ASSETS: &str = "CustomUIAssets";

/// Where an asset URL is hosted, by coarse class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostClass {
    /// `imgur.com` or any of its subdomains.
    Imgur,
    /// A host containing `steamusercontent` (Steam Cloud / UGC).
    SteamUserContent,
    /// Any other host.
    Other,
}

impl HostClass {
    /// Stable lowercase name: `imgur`, `steamusercontent` or `other`.
    pub fn as_str(self) -> &'static str {
        match self {
            HostClass::Imgur => "imgur",
            HostClass::SteamUserContent => "steamusercontent",
            HostClass::Other => "other",
        }
    }
}

/// One unique asset URL together with where and how often it occurs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetUrl {
    /// The URL text, exactly as stored in the save.
    pub url: String,
    /// The labels (holding object keys) it occurs under, sorted and unique.
    pub labels: Vec<String>,
    /// Number of occurrences in the save.
    pub count: usize,
    /// Every occurrence in document order; `count` equals its length.
    pub occurrences: Vec<Occurrence>,
}

/// One place where an asset URL occurs in a save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// JSON path: dot-joined object keys with `[i]` for array indexes, for
    /// example `ObjectStates[3].CustomDeck.1.FaceURL`.
    pub path: String,
    /// `Nickname` of the owning object, if it is a non-empty string.
    pub nickname: Option<String>,
    /// `GUID` of the owning object: the nearest enclosing JSON object with a
    /// string `GUID` key. `None` when no enclosing object has one.
    pub guid: Option<String>,
}

impl AssetUrl {
    /// Classifies the host of this URL.
    pub fn host_class(&self) -> HostClass {
        let host = host_of(&self.url);
        if host == "imgur.com" || host.ends_with(".imgur.com") {
            HostClass::Imgur
        } else if host.contains("steamusercontent") {
            HostClass::SteamUserContent
        } else {
            HostClass::Other
        }
    }
}

fn has_url_scheme(s: &str) -> bool {
    let starts = |prefix: &str| {
        s.get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
    };
    starts("http://") || starts("https://")
}

/// Lowercased host of `url` without userinfo and port.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host_port.starts_with('[') {
        // IPv6 literal: keep the bracketed part, drop a trailing port.
        host_port
            .find(']')
            .map_or(host_port, |end| &host_port[..=end])
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    host.to_ascii_lowercase()
}

/// Aggregated data for one URL while walking.
#[derive(Default)]
struct FoundUrl {
    labels: BTreeSet<String>,
    occurrences: Vec<Occurrence>,
}

type Found = BTreeMap<String, FoundUrl>;

/// Owner of the object being walked: `(guid, nickname)`.
type Owner<'a> = Option<(&'a str, Option<&'a str>)>;

fn walk(
    value: &Value,
    label: Option<&str>,
    ui_asset_item: bool,
    path: &mut String,
    owner: Owner<'_>,
    found: &mut Found,
) {
    match value {
        Value::String(s) if has_url_scheme(s) => {
            let entry = found.entry(s.clone()).or_default();
            if let Some(label) = label {
                entry.labels.insert(label.to_owned());
            }
            entry.occurrences.push(Occurrence {
                path: path.clone(),
                nickname: owner.and_then(|(_, n)| n).map(str::to_owned),
                guid: owner.map(|(g, _)| g.to_owned()),
            });
        }
        Value::Array(items) => {
            let items_are_ui_assets = label == Some(CUSTOM_UI_ASSETS);
            for (i, item) in items.iter().enumerate() {
                let len = path.len();
                path.push_str(&format!("[{i}]"));
                walk(item, label, items_are_ui_assets, path, owner, found);
                path.truncate(len);
            }
        }
        Value::Object(map) => {
            let owner = match map.get("GUID") {
                Some(Value::String(guid)) => Some((
                    guid.as_str(),
                    map.get("Nickname")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty()),
                )),
                _ => owner,
            };
            for (key, child) in map {
                let child_label = if ui_asset_item && key == "URL" {
                    CUSTOM_UI_ASSETS
                } else {
                    key.as_str()
                };
                let len = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
                walk(child, Some(child_label), false, path, owner, found);
                path.truncate(len);
            }
        }
        _ => {}
    }
}

impl Save {
    /// Every unique asset URL in the save, sorted by URL in byte order.
    ///
    /// Any string value starting with `http://` or `https://` (scheme matched
    /// case-insensitively) counts, wherever it is. The label is the object key
    /// holding the string; for array items it is the nearest enclosing key.
    /// The `URL` key of a `CustomUIAssets` element is labelled
    /// `CustomUIAssets`.
    pub fn asset_urls(&self) -> Vec<AssetUrl> {
        let mut found = Found::new();
        walk(
            self.value(),
            None,
            false,
            &mut String::new(),
            None,
            &mut found,
        );
        found
            .into_iter()
            .map(|(url, f)| AssetUrl {
                url,
                labels: f.labels.into_iter().collect(),
                count: f.occurrences.len(),
                occurrences: f.occurrences,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parsing() {
        assert_eq!(
            host_of("https://User:pw@Example.COM:8080/a?b#c"),
            "example.com"
        );
        assert_eq!(host_of("http://example.com?x=1"), "example.com");
        assert_eq!(host_of("http://example.com#f"), "example.com");
        assert_eq!(host_of("http://[::1]:80/x"), "[::1]");
    }

    #[test]
    fn scheme_is_case_insensitive_and_char_boundary_safe() {
        assert!(has_url_scheme("HTTPS://x"));
        assert!(has_url_scheme("Http://x"));
        assert!(!has_url_scheme("ftp://x"));
        assert!(!has_url_scheme("http:/x"));
        assert!(!has_url_scheme("héllo wörld"));
    }
}
