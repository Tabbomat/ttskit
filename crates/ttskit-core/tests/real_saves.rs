//! Local-only check against real Tabletop Simulator saves.
//!
//! Set `TTSKIT_REAL_SAVES` to a directory (the test panics if it is set but not an
//! existing directory); every `*.json` file below it is read
//! (read only, nothing is ever written there) and round-tripped through the
//! writer. Without the variable the test returns early, so it is skipped in CI.

use serde_json::Value;
use ttskit_core::save::write::to_tts_vec;
use ttskit_testutil::{collect_json, first_diff, real_saves_dir, same_structure};

#[test]
fn real_saves_round_trip() {
    let Some(dir) = real_saves_dir() else {
        eprintln!("TTSKIT_REAL_SAVES not set; skipping real save check");
        return;
    };
    let files = collect_json(&dir);

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
        if !same_structure(&value, &reparsed) {
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

/// Unpack, then pack with `--keep-eol`: byte-exact for saves written by
/// Tabletop Simulator, unless a field has mixed line endings.
#[test]
fn real_saves_pack_round_trip() {
    use ttskit_core::{Eol, PackOptions, pack, unpack};

    let Some(dir) = real_saves_dir() else {
        eprintln!("TTSKIT_REAL_SAVES not set; skipping real save pack check");
        return;
    };
    let mut failures = Vec::new();
    for path in collect_json(&dir) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let input = std::fs::read(&path).unwrap();
        let Ok(unpacked) = unpack(&input, Eol::native()) else {
            continue;
        };
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().join("tree");
        unpacked.write_to_dir(&tree).unwrap();
        let tts_written = input.starts_with(b"{\r\n") && !input.ends_with(b"\n");
        match pack(&tree, &PackOptions::new(true)) {
            Err(e) => failures.push(format!("{name}: {e}")),
            Ok(out)
                if tts_written && unpacked.report.mixed_eol.is_empty() && out.bytes != input =>
            {
                failures.push(format!("{name}: {}", first_diff(&input, &out.bytes)));
            }
            Ok(_) => {}
        }
    }
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
