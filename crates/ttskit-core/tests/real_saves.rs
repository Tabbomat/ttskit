//! Local-only check against real Tabletop Simulator saves.
//!
//! Set `TTSKIT_REAL_SAVES` to a directory; every `*.json` file below it is read
//! (read only, nothing is ever written there) and round-tripped through the
//! writer. Without the variable the test returns early, so it is skipped in CI.

mod common;

use std::path::{Path, PathBuf};

use serde_json::Value;
use ttskit_core::save::write::to_tts_vec;

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            out.push(path);
        }
    }
}

fn first_diff(a: &[u8], b: &[u8]) -> String {
    let pos = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let ctx = |s: &[u8]| {
        let from = pos.saturating_sub(40);
        let to = (pos + 40).min(s.len());
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

#[test]
fn real_saves_round_trip() {
    let Some(dir) = std::env::var_os("TTSKIT_REAL_SAVES") else {
        eprintln!("TTSKIT_REAL_SAVES not set; skipping real save check");
        return;
    };
    let mut files = Vec::new();
    collect_json(Path::new(&dir), &mut files);
    files.sort();

    let mut failures = Vec::new();
    let mut byte_exact = 0usize;
    let mut structural_only = 0usize;

    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let input = std::fs::read(path).unwrap();
        let body = input.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&input);
        let value: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{name}: parse error: {e}"));
                continue;
            }
        };
        let out = to_tts_vec(&value);

        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        if !common::same_structure(&value, &reparsed) {
            failures.push(format!(
                "{name}: output does not re-parse to the same structure"
            ));
            continue;
        }
        let again = to_tts_vec(&reparsed);
        if again != out {
            failures.push(format!(
                "{name}: second write differs: {}",
                first_diff(&out, &again)
            ));
            continue;
        }

        let tts_written =
            (input.starts_with(b"{\r\n") || input.starts_with(b"[\r\n")) && !input.ends_with(b"\n");
        if tts_written {
            if out == input {
                byte_exact += 1;
            } else {
                failures.push(format!(
                    "{name}: not byte-exact: {}",
                    first_diff(&input, &out)
                ));
            }
        } else {
            structural_only += 1;
        }
    }

    println!(
        "real saves: {} files checked, {} byte-exact, {} structural-only, {} failed",
        files.len(),
        byte_exact,
        structural_only,
        failures.len()
    );
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
