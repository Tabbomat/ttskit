//! `ttskit save ...` commands (local only, no Steam needed).

use std::io::Write;
use std::path::Path;

use serde_json::json;
use ttskit_core::{Eol, UnpackError, WriteOptions, WritePlan};

/// Flags shared by `unpack` and `pack` that control overwriting.
#[derive(Debug, Clone, Copy, Default)]
pub struct Safety {
    /// Allow overwriting what exists.
    pub force: bool,
    /// Remove stale files of a previous unpack (unpack only).
    pub clean: bool,
    /// Print the plan and write nothing.
    pub dry_run: bool,
}

/// Lists what is created (dry run only, a real run would be too noisy),
/// overwritten and removed.
fn write_plan_lines(plan: &WritePlan, dry_run: bool, out: &mut dyn Write) -> std::io::Result<()> {
    if dry_run {
        for path in &plan.create {
            writeln!(out, "create {path}")?;
        }
    }
    for path in &plan.change {
        writeln!(out, "change {path}")?;
    }
    for path in &plan.remove {
        writeln!(out, "remove {path}")?;
    }
    Ok(())
}

/// Unpacks `save` into `output` and prints a summary to `out`.
pub fn unpack(
    save: &Path,
    output: &Path,
    safety: Safety,
    json: bool,
    out: &mut dyn Write,
) -> Result<(), String> {
    let bytes = std::fs::read(save).map_err(|e| format!("cannot read {}: {e}", save.display()))?;
    let unpacked = ttskit_core::unpack(&bytes, Eol::native())
        .map_err(|e| format!("{}: {e}", save.display()))?;
    let options = WriteOptions {
        force: safety.force,
        clean: safety.clean,
    };
    let plan = unpacked.plan(output, options).map_err(|e| {
        let hint = match e {
            UnpackError::NotEmpty(_) => {
                "; use --force to write into it (unknown files stay), or --dry-run to preview"
            }
            _ => "",
        };
        format!("{e}{hint}")
    })?;
    if !safety.dry_run {
        unpacked.apply(output, &plan).map_err(|e| e.to_string())?;
    }

    let report = &unpacked.report;
    let io = |e: std::io::Error| e.to_string();
    if json {
        let value = json!({
            "output": output.display().to_string(),
            "objects": report.objects,
            "files": report.files,
            "mixed_eol": report.mixed_eol,
            "dry_run": safety.dry_run,
            "create": plan.create,
            "change": plan.change,
            "remove": plan.remove,
            "unchanged": plan.unchanged.len(),
        });
        let mut text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        text.push('\n');
        out.write_all(text.as_bytes()).map_err(io)?;
    } else {
        if safety.dry_run {
            writeln!(out, "Dry run: nothing was written.").map_err(io)?;
        }
        write_plan_lines(&plan, safety.dry_run, out).map_err(io)?;
        let verb = if safety.dry_run {
            "Would unpack"
        } else {
            "Unpacked"
        };
        writeln!(
            out,
            "{verb} {} objects into {} files in {} ({} created, {} changed, {} unchanged, {} removed)",
            report.objects,
            report.files,
            output.display(),
            plan.create.len(),
            plan.change.len(),
            plan.unchanged.len(),
            plan.remove.len(),
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

/// What packing would do to the output file.
enum Target {
    Create,
    Change,
    Unchanged,
}

impl Target {
    fn name(&self) -> &'static str {
        match self {
            Target::Create => "create",
            Target::Change => "change",
            Target::Unchanged => "unchanged",
        }
    }
}

/// Looks at the output path. An existing file needs `force`; anything that is
/// not a regular file is always an error.
fn inspect_target(output: &Path, bytes: &[u8], force: bool) -> Result<Target, String> {
    match std::fs::symlink_metadata(output) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Target::Create),
        Err(e) => Err(format!("cannot inspect {}: {e}", output.display())),
        Ok(meta) if !meta.is_file() => Err(format!(
            "output path {} exists and is not a regular file",
            output.display()
        )),
        Ok(_) if !force => Err(format!(
            "output file {} already exists; nothing was written; use --force to overwrite it, or --dry-run to preview",
            output.display()
        )),
        Ok(_) => {
            let current = std::fs::read(output)
                .map_err(|e| format!("cannot read {}: {e}", output.display()))?;
            Ok(if current == bytes {
                Target::Unchanged
            } else {
                Target::Change
            })
        }
    }
}

/// Writes `bytes` to `output`: a new file, or with `replace` through a
/// temporary file next to it, so a failure leaves the old file intact.
fn write_output(output: &Path, bytes: &[u8], replace: bool) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("cannot write {}: {e}", output.display());
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    if !replace {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .map_err(fail)?;
        return file.write_all(bytes).map_err(fail);
    }
    let mut tmp_name = output.as_os_str().to_owned();
    tmp_name.push(".ttskit-tmp");
    let tmp = std::path::PathBuf::from(tmp_name);
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        drop(file);
        std::fs::rename(&tmp, output)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(fail(e));
    }
    Ok(())
}

/// Packs the tree in `dir` into the save file `output`. Warnings go to `err`,
/// the summary to `out`. An existing `output` is only overwritten with
/// `safety.force`; with `safety.dry_run` nothing is written.
pub fn pack(
    dir: &Path,
    output: &Path,
    safety: Safety,
    keep_eol: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    // Cheap refusal before the tree is read; repeated below with the content.
    if !safety.force && output.exists() {
        inspect_target(output, &[], false)?;
    }
    let packed = ttskit_core::pack(dir, &ttskit_core::PackOptions::new(keep_eol))
        .map_err(|e| e.to_string())?;
    let target = inspect_target(output, &packed.bytes, safety.force)?;
    if !safety.dry_run {
        let replace = !matches!(target, Target::Create);
        write_output(output, &packed.bytes, replace)?;
    }

    for warning in &packed.warnings {
        writeln!(err, "warning: {warning}").map_err(io)?;
    }
    if json {
        let value = json!({
            "output": output.display().to_string(),
            "objects": packed.objects,
            "bytes": packed.bytes.len(),
            "warnings": packed.warnings,
            "dry_run": safety.dry_run,
            "action": target.name(),
        });
        let mut text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        text.push('\n');
        out.write_all(text.as_bytes()).map_err(io)?;
    } else {
        if safety.dry_run {
            writeln!(out, "Dry run: nothing was written.").map_err(io)?;
            if !matches!(target, Target::Unchanged) {
                writeln!(out, "{} {}", target.name(), output.display()).map_err(io)?;
            }
        }
        let verb = if safety.dry_run {
            "Would pack"
        } else {
            "Packed"
        };
        writeln!(
            out,
            "{verb} {} objects from {} into {}{}",
            packed.objects,
            dir.display(),
            output.display(),
            if matches!(target, Target::Unchanged) {
                " (unchanged)"
            } else {
                ""
            },
        )
        .map_err(io)?;
    }
    Ok(())
}
