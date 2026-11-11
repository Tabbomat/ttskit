//! Test helpers shared by the ttskit crates (not published).
//!
//! Use this crate as a dev-dependency from any workspace crate. Paths are
//! resolved from this crate's own `env!("CARGO_MANIFEST_DIR")`, so the result
//! is the same whichever crate calls the helpers.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Environment variable naming a directory of real saves for local-only checks.
pub const REAL_SAVES_ENV: &str = "TTSKIT_REAL_SAVES";

/// The shared fixtures directory, `<workspace root>/tests/fixtures`.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

/// Path of the fixture called `name` inside [`fixtures_dir`].
pub fn fixture_path(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

/// Reads the fixture called `name`.
///
/// # Panics
///
/// Panics, naming the path, if the file cannot be read.
pub fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture_path(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()))
}

fn is_json(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

/// All `*.json` files directly in [`fixtures_dir`] (extension compared
/// case-insensitively), sorted.
pub fn fixture_saves() -> Vec<PathBuf> {
    let dir = fixtures_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read fixtures dir {}: {e}", dir.display()));
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_json(p))
        .collect();
    files.sort();
    files
}

/// Structural equality that, unlike `Value`'s `PartialEq`, also compares the
/// order of object keys.
pub fn same_structure(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && same_structure(v1, v2))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_structure(p, q))
        }
        _ => a == b,
    }
}

/// Directory of real saves from [`REAL_SAVES_ENV`], or `None` when the
/// variable is unset or empty.
///
/// # Panics
///
/// Panics if the variable is set but does not name an existing directory, so a
/// typo does not make a check silently pass.
pub fn real_saves_dir() -> Option<PathBuf> {
    let value = std::env::var_os(REAL_SAVES_ENV).filter(|v| !v.is_empty())?;
    let dir = PathBuf::from(value);
    assert!(
        dir.is_dir(),
        "{REAL_SAVES_ENV} is set to {}, which is not an existing directory",
        dir.display()
    );
    Some(dir)
}

/// All `*.json` files below `dir`, recursively, sorted. Unreadable
/// directories are skipped.
pub fn collect_json(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if is_json(&path) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// Describes where two byte strings first differ, with lossy-UTF-8 context
/// around that position from each.
pub fn first_diff(a: &[u8], b: &[u8]) -> String {
    let pos = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let ctx = |s: &[u8]| {
        let from = pos.saturating_sub(40).min(s.len());
        let to = pos.saturating_add(40).min(s.len());
        String::from_utf8_lossy(&s[from..to]).into_owned()
    };
    format!(
        "first difference at byte {pos} (lengths {} vs {})\n  input:  {:?}\n  output: {:?}",
        a.len(),
        b.len(),
        ctx(a),
        ctx(b)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_dir_contains_minimal_save() {
        assert!(fixtures_dir().is_dir());
        assert!(fixture_path("minimal_save.json").is_file());
        assert!(!read_fixture("minimal_save.json").is_empty());
    }

    #[test]
    fn fixture_saves_non_empty_and_sorted() {
        let saves = fixture_saves();
        assert!(!saves.is_empty());
        let mut sorted = saves.clone();
        sorted.sort();
        assert_eq!(saves, sorted);
    }

    #[test]
    fn same_structure_detects_key_order() {
        let a: Value = serde_json::from_str(r#"{"a":1,"b":[1,2]}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"b":[1,2],"a":1}"#).unwrap();
        assert!(same_structure(&a, &a.clone()));
        assert!(!same_structure(&a, &b));
    }

    #[test]
    fn first_diff_reports_position() {
        let s = first_diff(b"abcdef", b"abcXef");
        assert!(s.contains("byte 3"), "{s}");
        assert!(s.contains("lengths 6 vs 6"), "{s}");
        // Prefix case and empty input must not panic.
        assert!(first_diff(b"abc", b"abcdef").contains("byte 3"));
        let _ = first_diff(b"", b"xyz");
    }

    #[test]
    fn collect_json_recurses_and_sorts() {
        let root = std::env::temp_dir().join(format!("ttskit-testutil-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("b.json"), "{}").unwrap();
        std::fs::write(root.join("c.txt"), "").unwrap();
        std::fs::write(sub.join("a.JSON"), "{}").unwrap();

        let found = collect_json(&root);
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(found, vec![root.join("b.json"), sub.join("a.JSON")]);
    }
}
