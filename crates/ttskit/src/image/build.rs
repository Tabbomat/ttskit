//! `ttskit image build`: runs the `[[image.pipeline]]` rules of `ttskit.toml`.

use std::io::Write;
use std::path::PathBuf;

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct BuildArgs {
    pub config: PathBuf,
    pub dry_run: bool,
    pub prune: bool,
    pub yes: bool,
    pub json: bool,
}

#[cfg(not(feature = "image"))]
pub fn build(_args: &BuildArgs, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
pub fn build(args: &BuildArgs, out: &mut dyn Write) -> Result<bool, String> {
    use ttskit_image::pipeline::{Cache, Context};

    if args.prune && !args.yes && !args.dry_run {
        return Err(
            "--prune deletes outputs that are no longer made; pass --yes to confirm (or use --dry-run to only see which)"
                .to_owned(),
        );
    }
    let text = std::fs::read_to_string(&args.config)
        .map_err(|e| format!("{}: {e}", args.config.display()))?;
    let pipeline = ttskit_image::pipeline::parse_pipeline(&text)
        .map_err(|e| format!("{}: {e}", args.config.display()))?;
    if pipeline.rules.is_empty() {
        return Err(format!(
            "no [[image.pipeline]] rules in {}",
            args.config.display()
        ));
    }
    let config_dir = match args.config.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let ctx = Context {
        config_dir: &config_dir,
        version: env!("CARGO_PKG_VERSION"),
    };
    let mut cache = Cache::load(&config_dir).map_err(|e| e.to_string())?;
    let plan = ttskit_image::pipeline::plan(&pipeline, &ctx, &cache).map_err(|e| e.to_string())?;

    let io = |e: std::io::Error| e.to_string();
    if !args.json {
        write_plan(&plan, &pipeline, out).map_err(io)?;
    }
    if args.dry_run {
        if args.json {
            let value = serde_json::json!({
                "dry_run": true,
                "plan": plan,
                "build": null,
                "prune": null,
            });
            serde_json::to_writer(&mut *out, &value).map_err(|e| e.to_string())?;
            writeln!(out).map_err(io)?;
        } else {
            writeln!(
                out,
                "dry run: {} to build, {} up to date, {} stale; nothing was written",
                plan.to_build(),
                plan.jobs.len() - plan.to_build(),
                plan.stale.len()
            )
            .map_err(io)?;
        }
        return Ok(true);
    }

    let report = ttskit_image::pipeline::build(&pipeline, &ctx, &plan, &mut cache);
    let pruned =
        (args.prune && args.yes).then(|| ttskit_image::pipeline::prune(&ctx, &plan, &mut cache));
    // Also after a partial failure, so that the outputs that were written are
    // recorded.
    let saved = cache.save(&config_dir);

    let ok = report.failed() == 0 && pruned.as_ref().is_none_or(|p| p.errors.is_empty());
    if args.json {
        let value = serde_json::json!({
            "dry_run": false,
            "plan": plan,
            "build": report,
            "prune": pruned,
        });
        serde_json::to_writer(&mut *out, &value).map_err(|e| e.to_string())?;
        writeln!(out).map_err(io)?;
    } else {
        write_results(&plan, &report, pruned.as_ref(), out).map_err(io)?;
    }
    saved.map_err(|e| e.to_string())?;
    Ok(ok)
}

#[cfg(feature = "image")]
use ttskit_image::pipeline::{BuildReport, JobResult, JobStatus, Plan, PruneReport, StaleState};

#[cfg(feature = "image")]
fn write_plan(
    plan: &Plan,
    pipeline: &ttskit_image::pipeline::Pipeline,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    for job in &plan.jobs {
        let JobStatus::Build { reason } = job.status else {
            continue;
        };
        let rule = &pipeline.rules[job.rule];
        let cards =
            job.inputs.len() - usize::from(rule.deck.as_ref().is_some_and(|d| d.hidden.is_some()));
        match &job.deck {
            Some(name) => writeln!(
                out,
                "build  {}  <- {} (deck {name}, {cards} cards; {})",
                job.outputs.join(", "),
                job.src,
                reason.text()
            )?,
            None => writeln!(
                out,
                "build  {}  <- {} ({})",
                job.outputs.join(", "),
                job.src,
                reason.text()
            )?,
        }
    }
    let up_to_date = plan.jobs.len() - plan.to_build();
    writeln!(out, "{up_to_date} up to date")?;
    for stale in &plan.stale {
        let what = match stale.state {
            StaleState::Present => "remove with --prune --yes",
            StaleState::Changed => "changed since it was built; never removed",
            StaleState::Gone => "already deleted; forgotten",
        };
        writeln!(out, "stale  {} ({what})", stale.path)?;
    }
    for note in &plan.notes {
        writeln!(out, "note: {note}")?;
    }
    Ok(())
}

#[cfg(feature = "image")]
fn write_results(
    plan: &Plan,
    report: &BuildReport,
    pruned: Option<&PruneReport>,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    for job in &report.jobs {
        for o in &job.outputs {
            writeln!(out, "wrote  {} ({} bytes)", o.path, o.bytes)?;
            for n in &o.notes {
                writeln!(out, "  note: {}", n.message)?;
            }
        }
        if job.result == JobResult::Failed {
            writeln!(
                out,
                "error: {}: {}",
                job.src,
                job.error.as_deref().unwrap_or("failed")
            )?;
        }
    }
    let mut summary = format!(
        "{} built, {} up to date, {} failed",
        report.built(),
        report.up_to_date(),
        report.failed()
    );
    match pruned {
        None => {
            if !plan.stale.is_empty() {
                summary.push_str(&format!("; {} stale", plan.stale.len()));
            }
        }
        Some(p) => {
            for path in &p.removed {
                writeln!(out, "removed  {path}")?;
            }
            for path in &p.gone {
                writeln!(out, "forgot  {path} (already deleted)")?;
            }
            for k in &p.kept {
                writeln!(out, "kept  {} ({})", k.path, k.reason)?;
            }
            for e in &p.errors {
                writeln!(out, "error: {e}")?;
            }
            summary.push_str(&format!(
                "; {} removed, {} forgotten, {} kept, {} prune errors",
                p.removed.len(),
                p.gone.len(),
                p.kept.len(),
                p.errors.len()
            ));
        }
    }
    writeln!(out, "{summary}")
}
