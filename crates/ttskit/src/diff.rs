//! `ttskit save diff`: which objects changed between two saves (local only).

use std::io::Write;
use std::path::Path;

use ttskit_core::Save;
use ttskit_diff::{DiffOptions, render_text, to_json};

/// What the command was asked to do.
pub struct Options<'a> {
    pub old: &'a Path,
    pub new: &'a Path,
    pub tolerance: f64,
    pub scripts: bool,
    pub verbose: bool,
    pub json: bool,
}

/// Outcome of a successful run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No differences.
    Equal,
    /// At least one difference.
    Different,
}

fn load(path: &Path) -> Result<Save, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Save::from_slice(&bytes).map_err(|e| format!("{} is not a valid save: {e}", path.display()))
}

/// Parses a `--tolerance` value: a finite, non-negative number.
pub fn parse_tolerance(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => Ok(v),
        _ => Err(format!(
            "invalid tolerance `{text}` (expected a non-negative number)"
        )),
    }
}

/// Compares the two saves and prints the result to `out`.
pub fn diff(options: &Options<'_>, out: &mut dyn Write) -> Result<Status, String> {
    let old = load(options.old)?;
    let new = load(options.new)?;
    let result = ttskit_diff::diff(
        &old,
        &new,
        &DiffOptions {
            tolerance: options.tolerance,
            scripts: options.scripts,
        },
    );
    if options.json {
        let mut text =
            serde_json::to_string_pretty(&to_json(&result)).map_err(|e| e.to_string())?;
        text.push('\n');
        out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    } else {
        out.write_all(render_text(&result, options.verbose).as_bytes())
            .map_err(|e| e.to_string())?;
    }
    Ok(if result.is_empty() {
        Status::Equal
    } else {
        Status::Different
    })
}
