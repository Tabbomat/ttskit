//! `ttskit image`: image inspection. Needs the `image` cargo feature.

use std::io::Write;
use std::path::PathBuf;

#[cfg(feature = "image")]
use serde::Serialize;
#[cfg(feature = "image")]
use std::path::Path;

#[cfg(feature = "image")]
pub fn info(inputs: &[PathBuf], json: bool, out: &mut dyn Write) -> Result<bool, String> {
    let files = ttskit_image::collect_inputs(inputs).map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err("no image files found in the given inputs".to_owned());
    }
    let mut report = Report::default();
    for file in &files {
        match ttskit_image::inspect(file) {
            Ok(info) => report.images.push(info),
            Err(error) => report.errors.push(error.to_string()),
        }
    }
    report.write(json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty() && report.images.iter().all(|i| i.compatible))
}

#[cfg(not(feature = "image"))]
pub fn info(_inputs: &[PathBuf], _json: bool, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
#[derive(Default, Serialize)]
struct Report {
    images: Vec<ttskit_image::ImageInfo>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl Report {
    fn write(&self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        if json {
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        for i in &self.images {
            let anim = if i.animated {
                format!(", {} frames", i.frames)
            } else {
                String::new()
            };
            let icc = match i.icc_profile_bytes {
                Some(n) => format!(", ICC profile ({n} bytes)"),
                None => String::new(),
            };
            writeln!(
                out,
                "{}: {} {}x{}, {} {}-bit{anim}{icc}, {} bytes",
                i.path, i.format, i.width, i.height, i.color, i.bit_depth, i.file_bytes
            )?;
            for p in &i.problems {
                writeln!(out, "  {}: {}", severity_label(p.severity), p.message)?;
            }
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        let incompatible = self.images.iter().filter(|i| !i.compatible).count();
        writeln!(
            out,
            "{} inspected, {} not usable in the game, {} unreadable",
            self.images.len(),
            incompatible,
            self.errors.len()
        )
    }
}

#[cfg(feature = "image")]
fn severity_label(severity: ttskit_image::Severity) -> &'static str {
    match severity {
        ttskit_image::Severity::Error => "error",
        ttskit_image::Severity::Warning => "warning",
        ttskit_image::Severity::Note => "note",
    }
}

/// Output format of `image convert`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Target {
    Png,
    #[value(alias = "jpeg")]
    Jpg,
}

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct ConvertArgs {
    pub inputs: Vec<PathBuf>,
    pub to: Target,
    pub quality: Option<u8>,
    pub background: Option<String>,
    pub keep_metadata: bool,
    pub out: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

#[cfg(not(feature = "image"))]
pub fn convert(_args: &ConvertArgs, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
pub fn convert(args: &ConvertArgs, out: &mut dyn Write) -> Result<bool, String> {
    use ttskit_image::{ConvertOptions, DEFAULT_JPEG_QUALITY, OutputFormat};

    let format = match args.to {
        Target::Png => OutputFormat::Png,
        Target::Jpg => OutputFormat::Jpeg,
    };
    if format == OutputFormat::Png {
        if args.quality.is_some() {
            return Err("--quality only applies to --to jpg".to_owned());
        }
        if args.background.is_some() {
            return Err("--background only applies to --to jpg".to_owned());
        }
    }
    let mut options = ConvertOptions::new(format);
    options.quality = args.quality.unwrap_or(DEFAULT_JPEG_QUALITY);
    options.keep_metadata = args.keep_metadata;
    if let Some(color) = &args.background {
        options.background = Some(ttskit_image::parse_color(color)?);
    }

    let files = ttskit_image::collect_inputs(&args.inputs).map_err(|e| e.to_string())?;
    if files.is_empty() {
        return Err("no image files found in the given inputs".to_owned());
    }
    let dest = Destinations {
        out: args.out.as_deref(),
        out_dir: args.out_dir.as_deref(),
        yes: args.yes,
    };
    let plan = plan_targets(&files, &dest, Some(format.extension()))?;

    let mut report = ConvertReport {
        dry_run: args.dry_run,
        ..ConvertReport::default()
    };
    for (file, target) in &plan {
        let converted = match ttskit_image::convert(file, &options) {
            Ok(c) => c,
            Err(error) => {
                report.errors.push(error.to_string());
                continue;
            }
        };
        let overwrote = target.exists();
        if !args.dry_run
            && let Err(error) = write_atomic(target, &converted.bytes)
        {
            report.errors.push(format!("{}: {error}", target.display()));
            continue;
        }
        report.conversions.push(Conversion {
            input: file.display().to_string(),
            output: target.display().to_string(),
            from: converted.from,
            to: format.name().to_owned(),
            width: converted.width,
            height: converted.height,
            bytes: converted.bytes.len() as u64,
            overwrote,
            notes: converted.notes,
        });
    }
    report.write(args.json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty())
}

/// Where the output of a command goes.
#[cfg(feature = "image")]
struct Destinations<'a> {
    out: Option<&'a Path>,
    out_dir: Option<&'a Path>,
    yes: bool,
}

/// Decides every output path and refuses the whole batch, before anything is
/// decoded or written, when two inputs would write the same file, an output
/// is another input of the batch, or a target exists and `--yes` is missing.
#[cfg(feature = "image")]
fn plan_targets(
    files: &[PathBuf],
    args: &Destinations,
    extension: Option<&str>,
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    if args.out.is_some() && files.len() != 1 {
        return Err(format!(
            "--out takes exactly one input, but {} image files were found; use --out-dir",
            files.len()
        ));
    }
    let mut plan = Vec::new();
    for file in files {
        let target = match args.out {
            Some(out) => out.to_path_buf(),
            None => {
                let not_a_name = || format!("{}: not a file name", file.display());
                let name = match extension {
                    Some(ext) => format!(
                        "{}.{ext}",
                        file.file_stem().ok_or_else(not_a_name)?.to_string_lossy()
                    ),
                    None => file
                        .file_name()
                        .ok_or_else(not_a_name)?
                        .to_string_lossy()
                        .into_owned(),
                };
                let dir = match args.out_dir {
                    Some(dir) => dir.to_path_buf(),
                    None => file.parent().map(PathBuf::from).unwrap_or_default(),
                };
                dir.join(name)
            }
        };
        plan.push((file.clone(), target));
    }

    let key = |p: &Path| p.to_string_lossy().to_lowercase().replace('\\', "/");
    let mut problems = Vec::new();
    let mut seen: std::collections::BTreeMap<String, &Path> = std::collections::BTreeMap::new();
    for (file, target) in &plan {
        if let Some(first) = seen.insert(key(target), file) {
            problems.push(format!(
                "{} and {} would both write {}",
                first.display(),
                file.display(),
                target.display()
            ));
        }
        if plan
            .iter()
            .any(|(other, _)| other != file && key(other) == key(target))
        {
            problems.push(format!(
                "{} would overwrite {}, another input of this run",
                file.display(),
                target.display()
            ));
        }
        if target.is_dir() {
            problems.push(format!("{} is a directory", target.display()));
        }
    }
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    if !args.yes {
        let existing: Vec<String> = plan
            .iter()
            .filter(|(_, t)| t.exists())
            .map(|(_, t)| t.display().to_string())
            .collect();
        if !existing.is_empty() {
            return Err(format!(
                "refusing to overwrite existing files (pass --yes to overwrite):\n{}",
                existing.join("\n")
            ));
        }
    }
    Ok(plan)
}

/// Writes next to the target and renames, so a failure never leaves a
/// half-written output and an existing file is replaced in one step.
#[cfg(feature = "image")]
fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".ttskit-tmp");
    let tmp = target.with_file_name(name);
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, target));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct Conversion {
    input: String,
    output: String,
    from: String,
    to: String,
    width: u32,
    height: u32,
    bytes: u64,
    overwrote: bool,
    notes: Vec<ttskit_image::Problem>,
}

#[cfg(feature = "image")]
#[derive(Default, Serialize)]
struct ConvertReport {
    dry_run: bool,
    conversions: Vec<Conversion>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl ConvertReport {
    fn write(&self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        if json {
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        for c in &self.conversions {
            let replaces = if c.overwrote { ", overwrites" } else { "" };
            writeln!(
                out,
                "{} -> {} ({} {}x{} to {}, {} bytes{replaces})",
                c.input, c.output, c.from, c.width, c.height, c.to, c.bytes
            )?;
            for n in &c.notes {
                writeln!(out, "  {}: {}", severity_label(n.severity), n.message)?;
            }
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        let verb = if self.dry_run {
            "would be converted (dry run, nothing written)"
        } else {
            "converted"
        };
        writeln!(
            out,
            "{} {verb}, {} failed",
            self.conversions.len(),
            self.errors.len()
        )
    }
}

mod optimize;
pub use optimize::{OptimizeArgs, Strip, optimize};
mod quantize;
pub use quantize::{EngineKind, QuantizeArgs, parse_dither, parse_quality, quantize};
mod check;
pub use check::{CheckArgs, CheckStatus, check, parse_bytes};
mod resize;
pub use resize::{FilterKind, ResizeArgs, parse_scale, resize};
mod deck;
#[cfg(feature = "image")]
mod deck_grid;
#[cfg(feature = "image")]
mod deck_object;
pub use deck::{
    DeckBuildArgs, DeckObjectArgs, DeckSplitArgs, FitKind, deck_build, deck_object, deck_split,
    parse_card_size,
};
