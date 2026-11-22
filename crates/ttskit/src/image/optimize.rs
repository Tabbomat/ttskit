//! `ttskit image optimize`: lossless PNG optimization.

use std::io::Write;
use std::path::PathBuf;

#[cfg(feature = "image")]
use super::{Destinations, plan_targets, write_atomic};
#[cfg(feature = "image")]
use serde::Serialize;
#[cfg(feature = "image")]
use std::path::Path;

/// Metadata chunks `image optimize` removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Strip {
    Safe,
    All,
    None,
}

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct OptimizeArgs {
    pub inputs: Vec<PathBuf>,
    pub level: u8,
    pub strip: Strip,
    pub out: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub in_place: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

#[cfg(not(feature = "image"))]
pub fn optimize(_args: &OptimizeArgs, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
pub fn optimize(args: &OptimizeArgs, out: &mut dyn Write) -> Result<bool, String> {
    use ttskit_image::{OptimizeOptions, Outcome, StripMode};

    if args.out.is_none() && args.out_dir.is_none() && !args.in_place && !args.dry_run {
        return Err(
            "say where the result goes: --out FILE, --out-dir DIR or --in-place --yes (or use --dry-run to only see the savings)"
                .to_owned(),
        );
    }
    if args.in_place && !args.yes && !args.dry_run {
        return Err("--in-place replaces the input files; pass --yes to confirm".to_owned());
    }
    let options = OptimizeOptions {
        level: args.level,
        strip: match args.strip {
            Strip::Safe => StripMode::Safe,
            Strip::All => StripMode::All,
            Strip::None => StripMode::None,
        },
    };

    let files = ttskit_image::collect_inputs(&args.inputs).map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err("no image files found in the given inputs".to_owned());
    }
    // Every output path is decided, and the batch refused, before any file
    // is optimized. Files that are not PNG are skipped and have no target.
    let candidates: Vec<PathBuf> = files.iter().filter(|f| !is_not_png(f)).cloned().collect();
    let plan: Vec<(PathBuf, PathBuf)> = if args.in_place {
        candidates.iter().map(|f| (f.clone(), f.clone())).collect()
    } else if args.out.is_none() && args.out_dir.is_none() {
        Vec::new() // dry run without a destination
    } else {
        let dest = Destinations {
            out: args.out.as_deref(),
            out_dir: args.out_dir.as_deref(),
            yes: args.yes,
        };
        plan_targets(&candidates, &dest, None)?
    };

    let mut report = OptimizeReport {
        dry_run: args.dry_run,
        level: args.level,
        strip: options.strip,
        files: Vec::new(),
        totals: None,
        errors: Vec::new(),
    };
    for file in &files {
        let target = plan.iter().find(|(f, _)| f == file).map(|(_, t)| t);
        let outcome = match ttskit_image::optimize(file, &options) {
            Ok(o) => o,
            Err(error) => {
                report.errors.push(error.to_string());
                continue;
            }
        };
        let input = file.display().to_string();
        match outcome {
            Outcome::Skipped(s) => report.files.push(OptimizedFile {
                input,
                output: None,
                status: "skipped",
                original_bytes: None,
                optimized_bytes: None,
                saved_bytes: None,
                written: false,
                note: Some(s.message),
            }),
            Outcome::Optimized(o) => {
                // In place, a file that did not shrink is never touched. With
                // a separate output, the original bytes are copied there,
                // unless that would write a file onto itself.
                let same = target.is_some_and(|t| t == file);
                let write = !args.dry_run && target.is_some() && !(same && !o.smaller);
                if write
                    && let Some(target) = target
                    && let Err(error) = write_atomic(target, &o.bytes)
                {
                    report.errors.push(format!("{}: {error}", target.display()));
                    continue;
                }
                report.files.push(OptimizedFile {
                    input,
                    output: target.filter(|_| !same).map(|t| t.display().to_string()),
                    status: if o.smaller { "optimized" } else { "unchanged" },
                    original_bytes: Some(o.original_bytes),
                    optimized_bytes: Some(o.optimized_bytes),
                    saved_bytes: Some(o.original_bytes - o.optimized_bytes),
                    written: write,
                    note: None,
                });
            }
        }
    }
    report.write(args.json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty())
}

/// True when the file can be read and does not start with the PNG signature.
#[cfg(feature = "image")]
fn is_not_png(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)) {
        Ok(()) => head != [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        Err(error) => error.kind() == std::io::ErrorKind::UnexpectedEof,
    }
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct OptimizedFile {
    input: String,
    /// Where the result goes; absent when optimizing in place or skipped.
    output: Option<String>,
    /// `optimized`, `unchanged` (not smaller) or `skipped`.
    status: &'static str,
    original_bytes: Option<u64>,
    optimized_bytes: Option<u64>,
    saved_bytes: Option<u64>,
    written: bool,
    note: Option<String>,
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct Totals {
    /// PNG files that were processed (not skipped).
    files: usize,
    original_bytes: u64,
    optimized_bytes: u64,
    saved_bytes: u64,
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct OptimizeReport {
    dry_run: bool,
    level: u8,
    strip: ttskit_image::StripMode,
    files: Vec<OptimizedFile>,
    totals: Option<Totals>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl OptimizeReport {
    fn totals(&self) -> Totals {
        let (mut files, mut original, mut optimized) = (0, 0, 0);
        for f in &self.files {
            if let (Some(old), Some(new)) = (f.original_bytes, f.optimized_bytes) {
                files += 1;
                original += old;
                optimized += new;
            }
        }
        Totals {
            files,
            original_bytes: original,
            optimized_bytes: optimized,
            saved_bytes: original - optimized,
        }
    }

    fn write(&mut self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        let t = self.totals();
        if json {
            self.totals = Some(t);
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        for f in &self.files {
            let to = f
                .output
                .as_ref()
                .map(|o| format!(" -> {o}"))
                .unwrap_or_default();
            match (f.original_bytes, f.optimized_bytes, f.saved_bytes) {
                (Some(old), Some(new), Some(saved)) => {
                    let detail = if f.status == "unchanged" {
                        format!("{old} bytes, not smaller, kept as is")
                    } else {
                        format!(
                            "{old} -> {new} bytes, saved {saved} ({})",
                            percent(saved, old)
                        )
                    };
                    writeln!(out, "{}{to}: {detail}", f.input)?;
                }
                _ => writeln!(
                    out,
                    "{}: skipped, {}",
                    f.input,
                    f.note.as_deref().unwrap_or("")
                )?,
            }
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        let verb = if self.dry_run {
            "would be smaller (dry run, nothing written)"
        } else {
            "optimized"
        };
        let changed = self
            .files
            .iter()
            .filter(|f| f.status == "optimized")
            .count();
        let skipped = self.files.iter().filter(|f| f.status == "skipped").count();
        writeln!(
            out,
            "{changed} of {} PNG files {verb}, {skipped} skipped, {} failed; total {} -> {} bytes, saved {} ({})",
            t.files,
            self.errors.len(),
            t.original_bytes,
            t.optimized_bytes,
            t.saved_bytes,
            percent(t.saved_bytes, t.original_bytes)
        )
    }
}

#[cfg(feature = "image")]
fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0.0%".to_owned();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}
