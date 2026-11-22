//! Building the planned jobs and pruning stale outputs.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::cache::{Cache, CacheEntry, rel_string, sha256_hex, write_atomic};
use super::plan::{Context, Job, JobStatus, Plan, StaleState, resolve_program};
use super::{Pipeline, Rule, Step};
use crate::{
    Engine, ImageError, Outcome, QuantizeOutcome, ResizeOutcome, build_deck, convert_bytes,
    optimize_bytes, quantize_bytes, resize_bytes,
};

/// Something a step changed or left alone. `code` is stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub code: String,
    pub message: String,
}

impl Note {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl From<crate::Problem> for Note {
    fn from(p: crate::Problem) -> Self {
        Self::new(p.code, p.message)
    }
}

/// An output file that was written.
#[derive(Debug, Clone, Serialize)]
pub struct OutputReport {
    /// Relative to the config directory, `/` separated.
    pub path: String,
    pub bytes: u64,
    /// Notes of the steps that made it. The notes of a deck itself (cards
    /// that were fitted, a large sheet) are on its description file.
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobResult {
    /// Not rebuilt; its outputs were up to date.
    UpToDate,
    Built,
    /// Something failed; `error` says what.
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobReport {
    /// Index into `Plan::jobs`.
    pub job: usize,
    pub src: String,
    pub result: JobResult,
    /// Outputs written (also when the job failed half way).
    pub outputs: Vec<OutputReport>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BuildReport {
    pub jobs: Vec<JobReport>,
}

impl BuildReport {
    pub fn built(&self) -> usize {
        self.count(&JobResult::Built)
    }

    pub fn up_to_date(&self) -> usize {
        self.count(&JobResult::UpToDate)
    }

    pub fn failed(&self) -> usize {
        self.count(&JobResult::Failed)
    }

    fn count(&self, result: &JobResult) -> usize {
        self.jobs.iter().filter(|j| j.result == *result).count()
    }
}

struct Produced {
    path: String,
    bytes: Vec<u8>,
    notes: Vec<Note>,
}

/// Runs the steps on `data` in memory. `label` is the source path, used in
/// error messages.
fn run_steps(
    config_dir: &Path,
    steps: &[Step],
    label: &Path,
    mut data: Vec<u8>,
) -> Result<(Vec<u8>, Vec<Note>), ImageError> {
    let mut notes = Vec::new();
    for step in steps {
        match step {
            Step::Convert(options) => {
                let done = convert_bytes(label, &data, options)?;
                notes.extend(done.notes.into_iter().map(Note::from));
                data = done.bytes;
            }
            Step::Resize(options) => match resize_bytes(label, &data, options)? {
                ResizeOutcome::Resized(done) => {
                    notes.extend(done.notes.into_iter().map(Note::from));
                    data = done.bytes;
                }
                ResizeOutcome::Unchanged(_) => {}
                ResizeOutcome::Skipped(s) => notes.push(Note::new(s.code, s.message)),
            },
            Step::Optimize(options) => match optimize_bytes(label, &data, options)? {
                Outcome::Optimized(done) => data = done.bytes,
                Outcome::Skipped(s) => notes.push(Note::new(s.code, s.message)),
            },
            Step::Quantize(options) => {
                let mut options = options.clone();
                if let Engine::Pngquant { program } = &options.engine {
                    options.engine = Engine::Pngquant {
                        program: resolve_program(config_dir, program.as_deref()),
                    };
                }
                match quantize_bytes(label, &data, &options)? {
                    QuantizeOutcome::Quantized(done) => data = done.bytes,
                    QuantizeOutcome::Skipped(s) => notes.push(Note::new(s.code, s.message)),
                }
            }
        }
    }
    Ok((data, notes))
}

fn produce(ctx: &Context, rule: &Rule, job: &Job) -> Result<Vec<Produced>, String> {
    let config_dir = ctx.config_dir;
    let Some(deck_rule) = &rule.deck else {
        let rel = &job.src;
        let data = std::fs::read(config_dir.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
        let (bytes, notes) =
            run_steps(config_dir, &rule.steps, Path::new(rel), data).map_err(|e| e.to_string())?;
        return Ok(vec![Produced {
            path: job.outputs[0].clone(),
            bytes,
            notes,
        }]);
    };

    let card_count = job.inputs.len() - usize::from(deck_rule.hidden.is_some());
    let cards: Vec<PathBuf> = job.inputs[..card_count]
        .iter()
        .map(|rel| config_dir.join(rel))
        .collect();
    let mut options = deck_rule.options.clone();
    options.hidden = deck_rule.hidden.as_ref().map(|h| config_dir.join(h));
    let mut deck = build_deck(&cards, &options).map_err(|e| e.to_string())?;
    let sheets = job.outputs.len() - 1;
    if deck.sheets.len() != sheets {
        return Err(format!(
            "the deck has {} sheets but {sheets} were planned",
            deck.sheets.len()
        ));
    }
    // The description must not depend on where the project lives.
    for sheet in &mut deck.sheets {
        for card in &mut sheet.cards {
            if let Ok(rel) = card.source.strip_prefix(config_dir) {
                card.source = PathBuf::from(rel_string(rel));
            }
        }
    }
    let file_names: Vec<String> = job.outputs[..sheets]
        .iter()
        .map(|o| o.rsplit('/').next().unwrap_or(o).to_owned())
        .collect();
    let name = job.deck.as_deref().unwrap_or_default();
    let mut json = serde_json::to_string_pretty(&deck.description(name, &file_names))
        .map_err(|e| e.to_string())?;
    json.push('\n');

    let mut produced = Vec::new();
    for (sheet, path) in deck.sheets.iter().zip(&job.outputs) {
        let (bytes, notes) = run_steps(
            config_dir,
            &rule.steps,
            Path::new(path),
            sheet.bytes.clone(),
        )
        .map_err(|e| e.to_string())?;
        produced.push(Produced {
            path: path.clone(),
            bytes,
            notes,
        });
    }
    produced.push(Produced {
        path: job.outputs[sheets].clone(),
        bytes: json.into_bytes(),
        notes: deck.notes.into_iter().map(Note::from).collect(),
    });
    Ok(produced)
}

/// Runs the jobs of `plan` that are not up to date and records them in
/// `cache`. Existing output files are overwritten. A job that fails is
/// reported and the others still run; its old cache entries stay. Entries of
/// outputs that no longer exist are dropped. The caller saves the cache.
pub fn build(pipeline: &Pipeline, ctx: &Context, plan: &Plan, cache: &mut Cache) -> BuildReport {
    for stale in &plan.stale {
        if stale.state == StaleState::Gone {
            cache.remove(&stale.path);
        }
    }
    let mut report = BuildReport::default();
    for (index, job) in plan.jobs.iter().enumerate() {
        let mut entry = JobReport {
            job: index,
            src: job.src.clone(),
            result: JobResult::UpToDate,
            outputs: Vec::new(),
            error: None,
        };
        if matches!(job.status, JobStatus::UpToDate) {
            report.jobs.push(entry);
            continue;
        }
        entry.result = JobResult::Built;
        match produce(ctx, &pipeline.rules[job.rule], job) {
            Err(error) => {
                entry.result = JobResult::Failed;
                entry.error = Some(error);
            }
            Ok(produced) => {
                for item in produced {
                    let target = ctx.config_dir.join(&item.path);
                    if let Err(e) = write_atomic(&target, &item.bytes) {
                        entry.result = JobResult::Failed;
                        entry.error = Some(format!("{}: {e}", item.path));
                        break;
                    }
                    cache.insert(
                        item.path.clone(),
                        CacheEntry {
                            inputs: job.inputs_hash.clone(),
                            options: job.options_hash.clone(),
                            rule: pipeline.rules[job.rule].src.clone(),
                            sha256: sha256_hex(&item.bytes),
                            version: ctx.version.to_owned(),
                        },
                    );
                    entry.outputs.push(OutputReport {
                        path: item.path,
                        bytes: item.bytes.len() as u64,
                        notes: item.notes,
                    });
                }
            }
        }
        report.jobs.push(entry);
    }
    report
}

/// A stale output that prune did not delete.
#[derive(Debug, Clone, Serialize)]
pub struct KeptOutput {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PruneReport {
    /// Files deleted.
    pub removed: Vec<String>,
    /// Entries dropped because the file was already gone.
    pub gone: Vec<String>,
    /// Files left alone because they were changed since the build; their
    /// entries are dropped.
    pub kept: Vec<KeptOutput>,
    /// Files that could not be deleted; their entries stay.
    pub errors: Vec<String>,
}

/// Deletes the stale outputs of `plan` that are still what was built and
/// drops their cache entries. Files changed since the build are kept (and
/// their entries dropped), files that are already gone just lose their
/// entry. Files that are not in the cache are never touched, and no
/// directory is removed. The state is checked again before each deletion.
pub fn prune(ctx: &Context, plan: &Plan, cache: &mut Cache) -> PruneReport {
    let mut report = PruneReport::default();
    for stale in &plan.stale {
        let Some(entry) = cache.get(&stale.path) else {
            continue;
        };
        let target = ctx.config_dir.join(&stale.path);
        let current = std::fs::read(&target).ok().map(|d| sha256_hex(&d));
        match current {
            None => {
                report.gone.push(stale.path.clone());
                cache.remove(&stale.path);
            }
            Some(hash) if hash != entry.sha256 => {
                report.kept.push(KeptOutput {
                    path: stale.path.clone(),
                    reason: "changed since the build".to_owned(),
                });
                cache.remove(&stale.path);
            }
            Some(_) => match std::fs::remove_file(&target) {
                Ok(()) => {
                    report.removed.push(stale.path.clone());
                    cache.remove(&stale.path);
                }
                Err(e) => report.errors.push(format!("{}: {e}", stale.path)),
            },
        }
    }
    report
}
