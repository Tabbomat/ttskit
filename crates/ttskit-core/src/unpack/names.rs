//! Directory and file names: sanitizing, truncation, reserved names and
//! collisions. See `docs/pack-layout.md`, "Directory names".

use std::collections::HashSet;

use serde_json::{Map, Value};

/// Longest `<name>` or `<title>` part, in Unicode scalar values.
const MAX_NAME_CHARS: usize = 24;

/// Replaces characters that are invalid in Windows file names and control
/// characters with `_`. Other non-ASCII characters are kept.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                || c <= '\u{1f}'
                || c == '\u{7f}'
            {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Sanitizes `s` and truncates it to 24 characters.
fn name_part(s: &str) -> String {
    sanitize(s).chars().take(MAX_NAME_CHARS).collect()
}

/// Whether `head` is a Windows reserved device name (case-insensitive).
pub fn is_reserved(head: &str) -> bool {
    let upper = head.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = upper.as_bytes();
    bytes.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && (b'1'..=b'9').contains(&bytes[3])
}

/// Appends `_` to the part before the first dot if it is a reserved name.
fn fix_reserved(name: String) -> String {
    let head_len = name.find('.').unwrap_or(name.len());
    if is_reserved(&name[..head_len]) {
        let mut fixed = String::with_capacity(name.len() + 1);
        fixed.push_str(&name[..head_len]);
        fixed.push('_');
        fixed.push_str(&name[head_len..]);
        fixed
    } else {
        name
    }
}

/// Steps 3 and 4: strips trailing dots and spaces, then fixes reserved names.
/// An empty result becomes `fallback`.
fn finish(name: &str, fallback: &str) -> String {
    let stripped = name.trim_end_matches(['.', ' ']);
    if stripped.is_empty() {
        return fallback.to_owned();
    }
    fix_reserved(stripped.to_owned())
}

fn non_empty_str<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    match map.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// The directory name (before collision handling) of an object.
///
/// `<name>.<GUID>`, with `<key>.` in front for a state. `name` is the
/// `Nickname`, else the `Name`.
pub fn object_dir_name(object: &Map<String, Value>, state_key: Option<&str>) -> String {
    let name = non_empty_str(object, "Nickname").or_else(|| non_empty_str(object, "Name"));
    let guid = non_empty_str(object, "GUID");
    let mut parts: Vec<String> = Vec::new();
    if let Some(key) = state_key {
        parts.push(sanitize(key));
    }
    if let Some(name) = name {
        parts.push(name_part(name));
    }
    if let Some(guid) = guid {
        parts.push(sanitize(guid));
    }
    if name.is_none() && guid.is_none() {
        parts.push("object".to_owned());
    }
    finish(&parts.join("."), "object")
}

/// The file stem (without `.txt`, before collision handling) of a notebook
/// page: `<key>.<title>`, or `<key>` without a title.
pub fn notebook_stem(key: &str, title: Option<&str>) -> String {
    let mut name = sanitize(key);
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        name.push('.');
        name.push_str(&name_part(title));
    }
    finish(&name, "notebook")
}

/// Names already taken in one directory; comparison is case-insensitive.
pub struct Siblings {
    used: HashSet<String>,
}

impl Siblings {
    /// An object-holding directory: `index.json` is reserved.
    pub fn with_index() -> Siblings {
        let mut used = HashSet::new();
        used.insert("index.json".to_owned());
        Siblings { used }
    }

    /// A directory without reserved names.
    pub fn empty() -> Siblings {
        Siblings {
            used: HashSet::new(),
        }
    }

    /// Takes `stem` + `ext`, or `stem-2` + `ext`, `stem-3` + `ext`, ... if
    /// that is taken.
    pub fn claim(&mut self, stem: &str, ext: &str) -> String {
        let mut candidate = format!("{stem}{ext}");
        let mut n = 1;
        while self.used.contains(&candidate.to_lowercase()) {
            n += 1;
            candidate = format!("{stem}-{n}{ext}");
        }
        self.used.insert(candidate.to_lowercase());
        candidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn sanitize_replaces_invalid_and_control_chars() {
        assert_eq!(sanitize("a<b>c:d\"e/f\\g|h?i*j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize("a\u{0}b\u{1f}c\u{7f}d"), "a_b_c_d");
        assert_eq!(sanitize("Tüte 日本語 🎲"), "Tüte 日本語 🎲");
        // C1 controls are not in the list and stay.
        assert_eq!(sanitize("a\u{85}b"), "a\u{85}b");
    }

    #[test]
    fn name_is_truncated_by_chars_not_bytes() {
        let name = object_dir_name(
            &obj(json!({"Nickname": "🎲".repeat(30), "GUID": "g1"})),
            None,
        );
        assert_eq!(name, format!("{}.g1", "🎲".repeat(24)));
        let long_guid = "g".repeat(40);
        let name = object_dir_name(&obj(json!({"Nickname": "x", "GUID": long_guid})), None);
        assert_eq!(name, format!("x.{}", "g".repeat(40)));
        let name = object_dir_name(&obj(json!({"Nickname": "x"})), Some(&"k".repeat(30)));
        assert_eq!(name, format!("{}.x", "k".repeat(30)));
    }

    #[test]
    fn name_fallbacks() {
        let n = |v: Value, k| object_dir_name(&obj(v), k);
        assert_eq!(
            n(json!({"Nickname": "A", "Name": "B", "GUID": "g"}), None),
            "A.g"
        );
        assert_eq!(
            n(json!({"Nickname": "", "Name": "B", "GUID": "g"}), None),
            "B.g"
        );
        assert_eq!(n(json!({"Name": "B"}), None), "B");
        assert_eq!(n(json!({"GUID": "g"}), None), "g");
        assert_eq!(n(json!({}), None), "object");
        assert_eq!(
            n(json!({"Nickname": 5, "Name": "", "GUID": 7}), None),
            "object"
        );
        assert_eq!(n(json!({}), Some("2")), "2.object");
        assert_eq!(n(json!({"GUID": "g"}), Some("2")), "2.g");
        assert_eq!(n(json!({"Nickname": "S", "GUID": "g"}), Some("2")), "2.S.g");
    }

    #[test]
    fn trailing_dots_and_spaces_are_stripped() {
        let n = |nick: &str| object_dir_name(&obj(json!({"Nickname": nick})), None);
        assert_eq!(n("abc. . "), "abc");
        assert_eq!(n("..."), "object");
        // With a GUID the dot-space tail is in the middle and stays.
        let name = object_dir_name(&obj(json!({"Nickname": "abc. ", "GUID": "g"})), None);
        assert_eq!(name, "abc. .g");
    }

    #[test]
    fn reserved_names() {
        for r in [
            "CON", "con", "Prn", "AUX", "nul", "COM1", "com9", "LPT1", "lpt9",
        ] {
            assert!(is_reserved(r), "{r}");
        }
        for r in ["COM0", "COM10", "CONX", "LPT", "", "CO"] {
            assert!(!is_reserved(r), "{r}");
        }
        let n = |nick: &str, guid: Option<&str>| {
            let mut v = json!({"Nickname": nick});
            if let Some(g) = guid {
                v["GUID"] = json!(g);
            }
            object_dir_name(&obj(v), None)
        };
        assert_eq!(n("con", Some("a1b2c3")), "con_.a1b2c3");
        assert_eq!(n("CON", None), "CON_");
        assert_eq!(n("Console", Some("g")), "Console.g");
        assert_eq!(notebook_stem("1", Some("x")), "1.x");
        assert_eq!(notebook_stem("nul", None), "nul_");
    }

    #[test]
    fn notebook_names() {
        assert_eq!(notebook_stem("0", Some("Rules")), "0.Rules");
        assert_eq!(notebook_stem("1", None), "1");
        assert_eq!(notebook_stem("1", Some("")), "1");
        assert_eq!(notebook_stem("2", Some("a/b?")), "2.a_b_");
        assert_eq!(
            notebook_stem("3", Some(&"t".repeat(40))),
            format!("3.{}", "t".repeat(24))
        );
        assert_eq!(notebook_stem("4", Some("end. ")), "4.end");
    }

    #[test]
    fn collisions_are_case_insensitive_in_order() {
        let mut s = Siblings::with_index();
        assert_eq!(s.claim("Card.c00001", ""), "Card.c00001");
        assert_eq!(s.claim("Card.c00001", ""), "Card.c00001-2");
        assert_eq!(s.claim("CARD.C00001", ""), "CARD.C00001-3");
        assert_eq!(s.claim("index", ".json"), "index-2.json");
        let mut t = Siblings::empty();
        assert_eq!(t.claim("0.Rules", ".txt"), "0.Rules.txt");
        assert_eq!(t.claim("0.rules", ".txt"), "0.rules-2.txt");
    }
}
