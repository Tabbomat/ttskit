//! `ttskit image quantize`: lossy PNG colour reduction.

use std::io::Write;
use std::path::PathBuf;

use super::Strip;
#[cfg(feature = "image")]
use super::optimize::{is_not_png, percent};
#[cfg(feature = "image")]
use super::{Destinations, plan_targets, write_atomic};
#[cfg(feature = "image")]
use serde::Serialize;

/// Which quantizer `image quantize` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum EngineKind {
    Builtin,
    Pngquant,
}

/// Parses `MIN-MAX`, each 0 to 100.
pub fn parse_quality(text: &str) -> Result<(u8, u8), String> {
    let bad = || format!("`{text}` is not a quality range; use MIN-MAX, e.g. 60-90");
    let (min, max) = text.split_once('-').ok_or_else(bad)?;
    let min: u8 = min.trim().parse().map_err(|_| bad())?;
    let max: u8 = max.trim().parse().map_err(|_| bad())?;
    if min > 100 || max > 100 {
        return Err(format!("quality values must be 0 to 100, not `{text}`"));
    }
    if min > max {
        return Err(format!("quality minimum is above the maximum in `{text}`"));
    }
    Ok((min, max))
}

/// Parses a dithering strength, 0.0 to 1.0.
pub fn parse_dither(text: &str) -> Result<f32, String> {
    let value: f32 = text
        .parse()
        .map_err(|_| format!("`{text}` is not a number"))?;
    if !(0.0..=1.0).contains(&value) {
        return Err(format!("dither must be between 0 and 1, not `{text}`"));
    }
    Ok(value)
}

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct QuantizeArgs {
    pub inputs: Vec<PathBuf>,
    pub colors: u16,
    pub quality: Option<(u8, u8)>,
    pub dither: f32,
    pub level: u8,
    pub strip: Strip,
    /// `None` means the built-in engine unless `pngquant` is given.
    pub engine: Option<EngineKind>,
    pub pngquant: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub in_place: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

#[cfg(not(feature = "image"))]
pub fn quantize(_args: &QuantizeArgs, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
pub fn quantize(args: &QuantizeArgs, out: &mut dyn Write) -> Result<bool, String> {
    use ttskit_image::{Engine, ImageError, QuantizeOptions, QuantizeOutcome, StripMode};

    if args.engine == Some(EngineKind::Builtin) && args.pngquant.is_some() {
        return Err("--pngquant needs the pngquant engine; drop `--engine builtin`".to_owned());
    }
    if args.out.is_none() && args.out_dir.is_none() && !args.in_place && !args.dry_run {
        return Err(
            "say where the result goes: --out FILE, --out-dir DIR or --in-place --yes (quantizing is lossy; or use --dry-run to only see the savings)"
                .to_owned(),
        );
    }
    if args.in_place && !args.yes && !args.dry_run {
        return Err(
            "--in-place replaces the input files with a lossy result; pass --yes to confirm"
                .to_owned(),
        );
    }
    let use_pngquant = args.engine == Some(EngineKind::Pngquant) || args.pngquant.is_some();
    let options = QuantizeOptions {
        colors: args.colors,
        quality: args.quality,
        dither: args.dither,
        level: args.level,
        strip: match args.strip {
            Strip::Safe => StripMode::Safe,
            Strip::All => StripMode::All,
            Strip::None => StripMode::None,
        },
        engine: if use_pngquant {
            Engine::Pngquant {
                program: args.pngquant.clone(),
            }
        } else {
            Engine::Builtin
        },
    };

    let files = ttskit_image::collect_inputs(&args.inputs).map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err("no image files found in the given inputs".to_owned());
    }
    // Every output path is decided, and the batch refused, before any file
    // is processed. Files that are not PNG are skipped and have no target.
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

    let mut report = QuantizeReport {
        dry_run: args.dry_run,
        lossy: true,
        engine: if use_pngquant { "pngquant" } else { "builtin" },
        colors: args.colors,
        quality: args.quality.map(|(min, max)| format!("{min}-{max}")),
        dither: args.dither,
        level: args.level,
        strip: options.strip,
        files: Vec::new(),
        totals: None,
        errors: Vec::new(),
    };
    for file in &files {
        let target = plan.iter().find(|(f, _)| f == file).map(|(_, t)| t);
        let outcome = match ttskit_image::quantize(file, &options) {
            Ok(o) => o,
            // Bad options or a missing pngquant: the same for every file, so
            // stop before anything is written.
            Err(error @ ImageError::Options(_)) => return Err(error.to_string()),
            Err(error) => {
                report.errors.push(error.to_string());
                continue;
            }
        };
        let input = file.display().to_string();
        match outcome {
            QuantizeOutcome::Skipped(s) => report.files.push(QuantizedFile {
                input,
                output: None,
                status: "skipped",
                code: Some(s.code),
                original_bytes: None,
                quantized_bytes: None,
                saved_bytes: None,
                colors: None,
                quality: None,
                written: false,
                note: Some(s.message),
            }),
            QuantizeOutcome::Quantized(q) => {
                // As in optimize: in place, a file that did not shrink is
                // never touched; with a separate output the original bytes
                // are copied there.
                let same = target.is_some_and(|t| t == file);
                let write = !args.dry_run && target.is_some() && !(same && !q.smaller);
                if write
                    && let Some(target) = target
                    && let Err(error) = write_atomic(target, &q.bytes)
                {
                    report.errors.push(format!("{}: {error}", target.display()));
                    continue;
                }
                report.files.push(QuantizedFile {
                    input,
                    output: target.filter(|_| !same).map(|t| t.display().to_string()),
                    status: if q.smaller { "quantized" } else { "unchanged" },
                    code: None,
                    original_bytes: Some(q.original_bytes),
                    quantized_bytes: Some(q.quantized_bytes),
                    saved_bytes: Some(q.original_bytes - q.quantized_bytes),
                    colors: Some(q.colors_used),
                    quality: Some(q.quality),
                    written: write,
                    note: None,
                });
            }
        }
    }
    report.write(args.json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty())
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct QuantizedFile {
    input: String,
    /// Where the result goes; absent when working in place or skipped.
    output: Option<String>,
    /// `quantized`, `unchanged` (not smaller) or `skipped`.
    status: &'static str,
    /// Why a file was skipped, e.g. `quality-too-low`, `not-png`, `animated`.
    code: Option<&'static str>,
    original_bytes: Option<u64>,
    quantized_bytes: Option<u64>,
    saved_bytes: Option<u64>,
    colors: Option<u16>,
    quality: Option<u8>,
    written: bool,
    note: Option<String>,
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct Totals {
    /// PNG files that were processed (not skipped).
    files: usize,
    original_bytes: u64,
    quantized_bytes: u64,
    saved_bytes: u64,
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct QuantizeReport {
    dry_run: bool,
    lossy: bool,
    engine: &'static str,
    colors: u16,
    quality: Option<String>,
    dither: f32,
    level: u8,
    strip: ttskit_image::StripMode,
    files: Vec<QuantizedFile>,
    totals: Option<Totals>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl QuantizeReport {
    fn totals(&self) -> Totals {
        let (mut files, mut original, mut quantized) = (0, 0, 0);
        for f in &self.files {
            if let (Some(old), Some(new)) = (f.original_bytes, f.quantized_bytes) {
                files += 1;
                original += old;
                quantized += new;
            }
        }
        Totals {
            files,
            original_bytes: original,
            quantized_bytes: quantized,
            saved_bytes: original - quantized,
        }
    }

    fn write(&mut self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        let t = self.totals();
        if json {
            self.totals = Some(t);
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        writeln!(
            out,
            "note: quantize is lossy; pixels change, colour profile and other chunks are not kept"
        )?;
        for f in &self.files {
            let to = f
                .output
                .as_ref()
                .map(|o| format!(" -> {o}"))
                .unwrap_or_default();
            match (f.original_bytes, f.quantized_bytes, f.saved_bytes) {
                (Some(old), Some(new), Some(saved)) => {
                    let detail = if f.status == "unchanged" {
                        format!("{old} bytes, not smaller, kept as is")
                    } else {
                        format!(
                            "{old} -> {new} bytes, saved {saved} ({}), {} colors, quality {}",
                            percent(saved, old),
                            f.colors.unwrap_or(0),
                            f.quality.unwrap_or(0)
                        )
                    };
                    writeln!(out, "{}{to}: {detail}", f.input)?;
                }
                _ => writeln!(
                    out,
                    "{}: skipped ({}), {}",
                    f.input,
                    f.code.unwrap_or("unknown"),
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
            "quantized"
        };
        let changed = self
            .files
            .iter()
            .filter(|f| f.status == "quantized")
            .count();
        let skipped = self.files.iter().filter(|f| f.status == "skipped").count();
        writeln!(
            out,
            "{changed} of {} PNG files {verb}, {skipped} skipped, {} failed; total {} -> {} bytes, saved {} ({})",
            t.files,
            self.errors.len(),
            t.original_bytes,
            t.quantized_bytes,
            t.saved_bytes,
            percent(t.saved_bytes, t.original_bytes)
        )
    }
}
