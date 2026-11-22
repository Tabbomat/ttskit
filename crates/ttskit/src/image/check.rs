//! `ttskit image check`: finds images that are too large for the game.

use std::io::Write;
use std::path::PathBuf;

#[cfg(feature = "image")]
use serde::Serialize;

/// How a `check` run ended; decides the exit status.
#[cfg_attr(not(feature = "image"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Every file was read and none has a finding.
    Clean,
    /// At least one finding, no errors.
    Findings,
    /// A file could not be read.
    Errors,
}

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct CheckArgs {
    pub inputs: Vec<PathBuf>,
    pub max_side: u32,
    pub max_bytes: Option<u64>,
    pub json: bool,
}

/// Parses a byte count: a whole number with an optional `k`, `m` or `g`
/// suffix (case does not matter; 1024-based, so `2m` is 2 097 152 bytes).
pub fn parse_bytes(text: &str) -> Result<u64, String> {
    let lower = text.trim().to_ascii_lowercase();
    let (digits, factor) = match lower.chars().last() {
        Some('k') => (&lower[..lower.len() - 1], 1u64 << 10),
        Some('m') => (&lower[..lower.len() - 1], 1 << 20),
        Some('g') => (&lower[..lower.len() - 1], 1 << 30),
        _ => (lower.as_str(), 1),
    };
    let bad = || format!("invalid size '{text}': use a number of bytes, optionally with k, m or g");
    let n: u64 = digits.parse().map_err(|_| bad())?;
    n.checked_mul(factor).ok_or_else(bad)
}

#[cfg(not(feature = "image"))]
pub fn check(_args: &CheckArgs, _out: &mut dyn Write) -> Result<CheckStatus, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
pub fn check(args: &CheckArgs, out: &mut dyn Write) -> Result<CheckStatus, String> {
    let options = ttskit_image::CheckOptions {
        max_side: args.max_side,
        max_bytes: args.max_bytes,
    };
    let files = ttskit_image::collect_inputs(&args.inputs).map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err("no image files found in the given inputs".to_owned());
    }
    let mut report = CheckOutput {
        max_side: options.max_side,
        max_bytes: options.max_bytes,
        images: Vec::new(),
        errors: Vec::new(),
    };
    for file in &files {
        match ttskit_image::check(file, &options) {
            Ok(r) => report.images.push(r),
            Err(error) => report.errors.push(error.to_string()),
        }
    }
    report.write(args.json, out).map_err(|e| e.to_string())?;
    Ok(if !report.errors.is_empty() {
        CheckStatus::Errors
    } else if report.images.iter().any(|i| !i.findings.is_empty()) {
        CheckStatus::Findings
    } else {
        CheckStatus::Clean
    })
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct CheckOutput {
    max_side: u32,
    max_bytes: Option<u64>,
    images: Vec<ttskit_image::CheckReport>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl CheckOutput {
    fn write(&self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        if json {
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        for i in &self.images {
            let alpha = if i.has_alpha { ", alpha" } else { "" };
            writeln!(
                out,
                "{}: {} {}x{}{alpha}, {} bytes",
                i.path, i.format, i.width, i.height, i.file_bytes
            )?;
            for f in &i.findings {
                writeln!(out, "  {}: {}", f.rule, f.message)?;
            }
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        let flagged = self
            .images
            .iter()
            .filter(|i| !i.findings.is_empty())
            .count();
        writeln!(
            out,
            "{} checked, {flagged} with findings, {} unreadable",
            self.images.len(),
            self.errors.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::parse_bytes;

    #[test]
    fn byte_sizes() {
        assert_eq!(parse_bytes("0"), Ok(0));
        assert_eq!(parse_bytes("1500"), Ok(1500));
        assert_eq!(parse_bytes("2k"), Ok(2048));
        assert_eq!(parse_bytes("2M"), Ok(2 << 20));
        assert_eq!(parse_bytes(" 1g "), Ok(1 << 30));
        for bad in ["", "k", "-1", "1.5m", "12x", "99999999999g"] {
            assert!(parse_bytes(bad).is_err(), "{bad}");
        }
    }
}
