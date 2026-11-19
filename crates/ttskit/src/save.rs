//! `ttskit save ...` commands (local only, no Steam needed).

use std::io::Write;
use std::path::Path;

use serde_json::json;
use ttskit_core::Eol;

/// Unpacks `save` into `output` and prints a summary to `out`.
pub fn unpack(save: &Path, output: &Path, json: bool, out: &mut dyn Write) -> Result<(), String> {
    let bytes = std::fs::read(save).map_err(|e| format!("cannot read {}: {e}", save.display()))?;
    let unpacked = ttskit_core::unpack(&bytes, Eol::native())
        .map_err(|e| format!("{}: {e}", save.display()))?;
    unpacked.write_to_dir(output).map_err(|e| e.to_string())?;

    let report = &unpacked.report;
    let io = |e: std::io::Error| e.to_string();
    if json {
        let value = json!({
            "output": output.display().to_string(),
            "objects": report.objects,
            "files": report.files,
            "mixed_eol": report.mixed_eol,
        });
        let mut text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        text.push('\n');
        out.write_all(text.as_bytes()).map_err(io)?;
    } else {
        writeln!(
            out,
            "Unpacked {} objects into {} files in {}",
            report.objects,
            report.files,
            output.display()
        )
        .map_err(io)?;
        for path in &report.mixed_eol {
            writeln!(
                out,
                "warning: {path} has mixed line endings; the majority style is recorded"
            )
            .map_err(io)?;
        }
    }
    Ok(())
}
