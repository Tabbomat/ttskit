//! `ttskit image resize`: scale PNG and JPEG images down.

use std::io::Write;
use std::path::PathBuf;

#[cfg(feature = "image")]
use super::{Destinations, plan_targets, write_atomic};
#[cfg(feature = "image")]
use serde::Serialize;
#[cfg(feature = "image")]
use std::path::Path;

/// Resampling filter of `image resize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum FilterKind {
    Lanczos3,
    CatmullRom,
    Triangle,
    Nearest,
}

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct ResizeArgs {
    pub inputs: Vec<PathBuf>,
    pub max_side: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub scale: Option<f64>,
    pub allow_enlarge: bool,
    pub filter: FilterKind,
    pub quality: Option<u8>,
    pub out: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub in_place: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

/// Parses `--scale`: a positive number.
pub fn parse_scale(text: &str) -> Result<f64, String> {
    match text.trim().parse::<f64>() {
        Ok(f) if f.is_finite() && f > 0.0 => Ok(f),
        _ => Err(format!(
            "invalid scale '{text}': use a positive number, e.g. 0.5"
        )),
    }
}

#[cfg(not(feature = "image"))]
pub fn resize(_args: &ResizeArgs, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
pub fn resize(args: &ResizeArgs, out: &mut dyn Write) -> Result<bool, String> {
    use ttskit_image::{Filter, ResizeOptions, ResizeOutcome, ResizeTarget};

    let target = match (args.max_side, args.width, args.height, args.scale) {
        (Some(n), None, None, None) => ResizeTarget::MaxSide(n),
        (None, Some(n), None, None) => ResizeTarget::Width(n),
        (None, None, Some(n), None) => ResizeTarget::Height(n),
        (None, None, None, Some(f)) => ResizeTarget::Scale(f),
        _ => {
            return Err("give exactly one of --max-side, --width, --height or --scale".to_owned());
        }
    };
    if args.out.is_none() && args.out_dir.is_none() && !args.in_place && !args.dry_run {
        return Err(
            "say where the result goes: --out FILE, --out-dir DIR or --in-place --yes (or use --dry-run to only see the new sizes)"
                .to_owned(),
        );
    }
    if args.in_place && !args.yes && !args.dry_run {
        return Err("--in-place replaces the input files; pass --yes to confirm".to_owned());
    }
    let mut options = ResizeOptions::new(target);
    options.allow_enlarge = args.allow_enlarge;
    options.filter = match args.filter {
        FilterKind::Lanczos3 => Filter::Lanczos3,
        FilterKind::CatmullRom => Filter::CatmullRom,
        FilterKind::Triangle => Filter::Triangle,
        FilterKind::Nearest => Filter::Nearest,
    };
    if let Some(quality) = args.quality {
        options.jpeg_quality = quality;
    }

    let files = ttskit_image::collect_inputs(&args.inputs).map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err("no image files found in the given inputs".to_owned());
    }
    // Every output path is decided, and the batch refused, before any file
    // is resized. Files that are neither PNG nor JPEG are skipped and have
    // no target.
    let candidates: Vec<PathBuf> = files
        .iter()
        .filter(|f| !is_other_format(f))
        .cloned()
        .collect();
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

    let mut report = ResizeReport {
        dry_run: args.dry_run,
        files: Vec::new(),
        errors: Vec::new(),
    };
    for file in &files {
        let target = plan.iter().find(|(f, _)| f == file).map(|(_, t)| t);
        let outcome = match ttskit_image::resize(file, &options) {
            Ok(o) => o,
            Err(error) => {
                report.errors.push(error.to_string());
                continue;
            }
        };
        let input = file.display().to_string();
        let same = target.is_some_and(|t| t == file);
        let output = target.filter(|_| !same).map(|t| t.display().to_string());
        let mut entry = ResizedFile {
            input,
            output,
            status: "skipped",
            code: None,
            original_width: None,
            original_height: None,
            width: None,
            height: None,
            original_bytes: None,
            new_bytes: None,
            written: false,
            notes: Vec::new(),
            note: None,
        };
        match outcome {
            ResizeOutcome::Skipped(s) => {
                entry.code = Some(s.code);
                entry.note = Some(s.message);
            }
            ResizeOutcome::Unchanged(u) => {
                // In place the file is not touched; elsewhere the original
                // bytes are copied, so the output always exists.
                let copy = !args.dry_run && !same && target.is_some();
                if copy
                    && let Some(target) = target
                    && let Err(error) =
                        std::fs::read(file).and_then(|bytes| write_atomic(target, &bytes))
                {
                    report.errors.push(format!("{}: {error}", target.display()));
                    continue;
                }
                entry.status = "unchanged";
                entry.code = Some(u.code);
                entry.note = Some(u.message);
                entry.original_width = Some(u.width);
                entry.original_height = Some(u.height);
                entry.width = Some(u.width);
                entry.height = Some(u.height);
                entry.original_bytes = Some(u.original_bytes);
                entry.new_bytes = Some(u.original_bytes);
                entry.written = copy;
            }
            ResizeOutcome::Resized(r) => {
                let write = !args.dry_run && target.is_some();
                if write
                    && let Some(target) = target
                    && let Err(error) = write_atomic(target, &r.bytes)
                {
                    report.errors.push(format!("{}: {error}", target.display()));
                    continue;
                }
                entry.status = "resized";
                entry.original_width = Some(r.original_width);
                entry.original_height = Some(r.original_height);
                entry.width = Some(r.width);
                entry.height = Some(r.height);
                entry.original_bytes = Some(r.original_bytes);
                entry.new_bytes = Some(r.bytes.len() as u64);
                entry.written = write;
                entry.notes = r.notes;
            }
        }
        report.files.push(entry);
    }
    report.write(args.json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty())
}

/// True when the file can be read and is neither a PNG nor a JPEG.
#[cfg(feature = "image")]
fn is_other_format(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)) {
        Ok(()) => {
            let png = head == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
            let jpeg = head[..3] == [0xFF, 0xD8, 0xFF];
            !(png || jpeg)
        }
        Err(error) => error.kind() == std::io::ErrorKind::UnexpectedEof,
    }
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct ResizedFile {
    input: String,
    /// Where the result goes; absent when resizing in place or skipped.
    output: Option<String>,
    /// `resized`, `unchanged` or `skipped`.
    status: &'static str,
    /// Why a file was not resized: `already-that-size`, `would-enlarge` or
    /// `unsupported-format`.
    code: Option<&'static str>,
    original_width: Option<u32>,
    original_height: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    original_bytes: Option<u64>,
    new_bytes: Option<u64>,
    written: bool,
    notes: Vec<ttskit_image::Problem>,
    note: Option<String>,
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct ResizeReport {
    dry_run: bool,
    files: Vec<ResizedFile>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl ResizeReport {
    fn write(&self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        if json {
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        for f in &self.files {
            let to = f
                .output
                .as_ref()
                .map(|o| format!(" -> {o}"))
                .unwrap_or_default();
            match (
                f.status,
                f.original_width,
                f.original_height,
                f.width,
                f.height,
            ) {
                ("resized", Some(ow), Some(oh), Some(w), Some(h)) => writeln!(
                    out,
                    "{}{to}: {ow}x{oh} -> {w}x{h}, {} -> {} bytes",
                    f.input,
                    f.original_bytes.unwrap_or(0),
                    f.new_bytes.unwrap_or(0)
                )?,
                _ => writeln!(
                    out,
                    "{}: {}, {}",
                    f.input,
                    f.status,
                    f.note.as_deref().unwrap_or("")
                )?,
            }
            for n in &f.notes {
                let label = match n.severity {
                    ttskit_image::Severity::Error => "error",
                    ttskit_image::Severity::Warning => "warning",
                    ttskit_image::Severity::Note => "note",
                };
                writeln!(out, "  {label}: {}", n.message)?;
            }
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        let count = |status: &str| self.files.iter().filter(|f| f.status == status).count();
        let verb = if self.dry_run {
            "would be resized (dry run, nothing written)"
        } else {
            "resized"
        };
        writeln!(
            out,
            "{} {verb}, {} unchanged, {} skipped, {} failed",
            count("resized"),
            count("unchanged"),
            count("skipped"),
            self.errors.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::parse_scale;

    #[test]
    fn scale_must_be_positive() {
        assert_eq!(parse_scale("0.5"), Ok(0.5));
        assert_eq!(parse_scale(" 2 "), Ok(2.0));
        for bad in ["", "0", "-1", "abc", "nan", "inf"] {
            assert!(parse_scale(bad).is_err(), "{bad}");
        }
    }
}
