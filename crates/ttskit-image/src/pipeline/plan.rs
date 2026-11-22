//! Planning: which sources a pipeline has, which outputs they make, and which
//! of those need a build. Planning reads files but never writes anything.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::cache::{BuildError, CACHE_FILE_NAME, CONFIG_FILE_NAME, Cache, rel_string, sha256_hex};
use super::{Pipeline, Rule, Step};
use crate::{Engine, natural_cmp, plan_sheets, pngquant_version};

/// What a plan or a build needs to know about the project.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The directory of `ttskit.toml`. All paths are relative to it.
    pub config_dir: &'a Path,
    /// The tool version recorded in the cache; a different one rebuilds
    /// everything.
    pub version: &'a str,
}

/// Why a job is rebuilt. Ordered by precedence: when several apply, the
/// first one is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildReason {
    /// An output has no cache entry.
    New,
    /// An output file does not exist.
    OutputMissing,
    /// An output file differs from what was built (edited by hand).
    OutputChanged,
    VersionChanged,
    InputsChanged,
    OptionsChanged,
}

impl BuildReason {
    pub fn text(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::OutputMissing => "output missing",
            Self::OutputChanged => "output changed",
            Self::VersionChanged => "version changed",
            Self::InputsChanged => "inputs changed",
            Self::OptionsChanged => "options changed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    UpToDate,
    Build { reason: BuildReason },
}

/// All outputs of one source file, or of one deck.
#[derive(Debug, Clone, Serialize)]
pub struct Job {
    /// Index of the rule in the pipeline.
    pub rule: usize,
    /// The source file (relative to the config directory, `/` separated); for
    /// a deck the `src` glob of the rule.
    pub src: String,
    /// The deck name, for a deck job.
    pub deck: Option<String>,
    /// Input files in order: the one source, or the cards and then the
    /// hidden image.
    pub inputs: Vec<String>,
    /// Output files, relative to the config directory. For a deck: the
    /// sheets, then the description.
    pub outputs: Vec<String>,
    /// Hash of the inputs (paths and contents), as recorded in the cache.
    pub inputs_hash: String,
    /// Hash of the options, as recorded in the cache.
    pub options_hash: String,
    pub status: JobStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StaleState {
    /// The file exists and is what was built.
    Present,
    /// The file no longer exists.
    Gone,
    /// The file exists but differs from what was built.
    Changed,
}

/// A cache entry whose output the current plan does not produce.
#[derive(Debug, Clone, Serialize)]
pub struct StaleOutput {
    pub path: String,
    /// The `src` of the rule that made it.
    pub rule: String,
    pub state: StaleState,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub jobs: Vec<Job>,
    pub stale: Vec<StaleOutput>,
    pub notes: Vec<String>,
}

impl Plan {
    /// Jobs that a build would run.
    pub fn to_build(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Build { .. }))
            .count()
    }
}

/// The program of a pngquant engine as it must be run: a path with more than
/// one component is relative to the config directory, a bare name is looked
/// up on `PATH` by the system.
pub(super) fn resolve_program(config_dir: &Path, program: Option<&Path>) -> Option<PathBuf> {
    program.map(|p| {
        if p.components().count() > 1 {
            config_dir.join(p)
        } else {
            p.to_path_buf()
        }
    })
}

fn is_cache_or_config(rel: &str) -> bool {
    rel.eq_ignore_ascii_case(CACHE_FILE_NAME) || rel.eq_ignore_ascii_case(CONFIG_FILE_NAME)
}

fn io_error(path: &Path, source: std::io::Error) -> BuildError {
    BuildError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Source files of a rule, as `/` separated paths relative to the config
/// directory, sorted by path.
fn discover(config_dir: &Path, rule: &Rule) -> Result<Vec<String>, BuildError> {
    let base = rel_string(&rule.base);
    let top = config_dir.join(&rule.base);
    let mut files = Vec::new();
    let mut stack = vec![(top, base)];
    while let Some((dir, rel_dir)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_error(&dir, e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| io_error(&dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if rel_dir.is_empty() {
                name
            } else {
                format!("{rel_dir}/{name}")
            };
            let kind = entry.file_type().map_err(|e| io_error(&entry.path(), e))?;
            if kind.is_dir() {
                stack.push((entry.path(), rel));
                continue;
            }
            let is_file = kind.is_file()
                || (kind.is_symlink()
                    && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file()));
            if is_file && !is_cache_or_config(&rel) && rule.matches(&rel) {
                files.push(rel);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Content hashes of input files, read once each.
struct Hashes<'a> {
    config_dir: &'a Path,
    memo: HashMap<String, String>,
}

impl Hashes<'_> {
    fn of(&mut self, rel: &str) -> Result<String, BuildError> {
        if let Some(hash) = self.memo.get(rel) {
            return Ok(hash.clone());
        }
        let path = self.config_dir.join(rel);
        let data = std::fs::read(&path).map_err(|e| io_error(&path, e))?;
        let hash = sha256_hex(&data);
        self.memo.insert(rel.to_owned(), hash.clone());
        Ok(hash)
    }

    fn inputs_hash(&mut self, inputs: &[String]) -> Result<String, BuildError> {
        let mut buf = Vec::new();
        for rel in inputs {
            buf.extend_from_slice(rel.as_bytes());
            buf.push(0);
            buf.extend_from_slice(self.of(rel)?.as_bytes());
            buf.push(0);
        }
        Ok(sha256_hex(&buf))
    }
}

fn options_hash(parts: &[String]) -> String {
    let mut buf = Vec::new();
    for part in parts {
        buf.extend_from_slice(part.as_bytes());
        buf.push(0);
    }
    sha256_hex(&buf)
}

fn file_hash(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|data| sha256_hex(&data))
}

/// The extension of the last `convert` step's format, if the rule converts.
fn converted_extension(rule: &Rule) -> Option<&'static str> {
    rule.steps.iter().rev().find_map(|s| match s {
        Step::Convert(o) => Some(o.to.extension()),
        _ => None,
    })
}

/// Fingerprints of a rule's steps and deck, plus the pngquant versions its
/// quantize steps use. Problems are added to `problems`.
fn rule_options(
    config_dir: &Path,
    rule: &Rule,
    index: usize,
    versions: &mut HashMap<Option<PathBuf>, Result<String, String>>,
    problems: &mut Vec<String>,
) -> Vec<String> {
    let mut parts: Vec<String> = rule.steps.iter().map(Step::fingerprint).collect();
    if let Some(deck) = &rule.deck {
        parts.push(deck.fingerprint());
    }
    let mut seen = HashSet::new();
    for step in &rule.steps {
        let Step::Quantize(options) = step else {
            continue;
        };
        let Engine::Pngquant { program } = &options.engine else {
            continue;
        };
        let program = resolve_program(config_dir, program.as_deref());
        if !seen.insert(program.clone()) {
            continue;
        }
        let result = versions
            .entry(program.clone())
            .or_insert_with(|| pngquant_version(program.as_deref()).map_err(|e| e.to_string()))
            .clone();
        match result {
            Ok(version) => parts.push(format!("pngquant {version}")),
            Err(message) => problems.push(format!(
                "image.pipeline[{index}] (`{}`): {message}",
                rule.src
            )),
        }
    }
    parts
}

fn out_path(rule: &Rule, rest: &str) -> String {
    let out = rel_string(&rule.out);
    if out.is_empty() {
        rest.to_owned()
    } else {
        format!("{out}/{rest}")
    }
}

/// Plans the pipeline against the files on disk and the cache. Reads sources
/// (to hash them) and existing outputs, writes nothing. Fails, listing every
/// problem, when outputs conflict or a rule cannot be planned.
pub fn plan(pipeline: &Pipeline, ctx: &Context, cache: &Cache) -> Result<Plan, BuildError> {
    let mut hashes = Hashes {
        config_dir: ctx.config_dir,
        memo: HashMap::new(),
    };
    let mut versions = HashMap::new();
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let mut jobs = Vec::new();

    for (index, rule) in pipeline.rules.iter().enumerate() {
        let mut sources = discover(ctx.config_dir, rule)?;
        if sources.is_empty() {
            if rule.deck.is_some() {
                notes.push(format!(
                    "image.pipeline[{index}] (`{}`): no cards match, so no deck is built",
                    rule.src
                ));
            }
            continue;
        }
        let mut options = rule_options(ctx.config_dir, rule, index, &mut versions, &mut problems);
        if let Some(deck) = &rule.deck {
            sources.sort_by(|a, b| natural_cmp(a, b));
            let mut inputs = sources;
            let hidden = deck.hidden.as_ref().map(|h| rel_string(h));
            inputs.extend(hidden.clone());
            let sheets = match plan_sheets(
                inputs.len() - usize::from(hidden.is_some()),
                deck.options.columns,
                deck.options.rows,
                deck.options.hidden_slot,
            ) {
                Ok(sheets) => sheets,
                Err(e) => {
                    problems.push(format!("image.pipeline[{index}] (`{}`): {e}", rule.src));
                    continue;
                }
            };
            let ext = deck.options.format.extension();
            let mut outputs: Vec<String> = if sheets.len() == 1 {
                vec![out_path(rule, &format!("{}.{ext}", deck.name))]
            } else {
                (1..=sheets.len())
                    .map(|n| out_path(rule, &format!("{}_{n}.{ext}", deck.name)))
                    .collect()
            };
            outputs.push(out_path(rule, &format!("{}.json", deck.name)));
            jobs.push(Job {
                rule: index,
                src: rule.src.clone(),
                deck: Some(deck.name.clone()),
                inputs_hash: hashes.inputs_hash(&inputs)?,
                options_hash: options_hash(&options),
                inputs,
                outputs,
                status: JobStatus::UpToDate,
            });
            continue;
        }
        let ext = converted_extension(rule);
        if let Some(ext) = ext {
            options.push(format!("format {ext}"));
        }
        let base = rel_string(&rule.base);
        for rel in sources {
            let rest = if base.is_empty() {
                rel.as_str()
            } else {
                &rel[base.len() + 1..]
            };
            let rest = match ext {
                Some(ext) => Path::new(rest).with_extension(ext),
                None => PathBuf::from(rest),
            };
            let output = out_path(rule, &rel_string(&rest));
            let inputs = vec![rel.clone()];
            jobs.push(Job {
                rule: index,
                src: rel,
                deck: None,
                inputs_hash: hashes.inputs_hash(&inputs)?,
                options_hash: options_hash(&options),
                inputs,
                outputs: vec![output],
                status: JobStatus::UpToDate,
            });
        }
    }
    if !problems.is_empty() {
        return Err(BuildError::Rules(problems));
    }

    let conflicts = find_conflicts(pipeline, ctx, &jobs);
    if !conflicts.is_empty() {
        return Err(BuildError::Conflicts(conflicts));
    }

    for job in &mut jobs {
        job.status = job_status(ctx, cache, job);
    }
    let stale = find_stale(ctx, cache, &jobs);
    Ok(Plan { jobs, stale, notes })
}

fn find_conflicts(pipeline: &Pipeline, ctx: &Context, jobs: &[Job]) -> Vec<String> {
    let mut conflicts = Vec::new();
    let mut by_path: BTreeMap<String, Vec<(&str, &Job)>> = BTreeMap::new();
    for job in jobs {
        for output in &job.outputs {
            by_path
                .entry(output.to_lowercase())
                .or_default()
                .push((output, job));
        }
    }
    let hidden: Vec<String> = pipeline
        .rules
        .iter()
        .filter_map(|r| r.deck.as_ref()?.hidden.as_ref())
        .map(|h| rel_string(h).to_lowercase())
        .collect();
    for writers in by_path.values() {
        let (path, _) = writers[0];
        if writers.len() > 1 {
            let who: Vec<String> = writers
                .iter()
                .map(|(p, j)| format!("`{}` (rule {}) as `{p}`", j.src, j.rule))
                .collect();
            conflicts.push(format!(
                "more than one output is written to `{path}`: {}",
                who.join(", ")
            ));
        }
        for (index, rule) in pipeline.rules.iter().enumerate() {
            if rule.matches(path) {
                conflicts.push(format!(
                    "output `{path}` is a source of image.pipeline[{index}] (`{}`); outputs must never become or overwrite sources",
                    rule.src
                ));
            }
        }
        if hidden.iter().any(|h| *h == path.to_lowercase()) {
            conflicts.push(format!(
                "output `{path}` would overwrite a hidden-card image"
            ));
        }
        if is_cache_or_config(path) {
            conflicts.push(format!(
                "output `{path}` would overwrite the cache file or the config"
            ));
        }
        if ctx.config_dir.join(path).is_dir() {
            conflicts.push(format!("output `{path}` is an existing directory"));
        }
    }
    conflicts
}

fn job_status(ctx: &Context, cache: &Cache, job: &Job) -> JobStatus {
    let reason = job
        .outputs
        .iter()
        .filter_map(|output| {
            let Some(entry) = cache.get(output) else {
                return Some(BuildReason::New);
            };
            match file_hash(&ctx.config_dir.join(output)) {
                None => return Some(BuildReason::OutputMissing),
                Some(hash) if hash != entry.sha256 => return Some(BuildReason::OutputChanged),
                Some(_) => {}
            }
            if entry.version != ctx.version {
                Some(BuildReason::VersionChanged)
            } else if entry.inputs != job.inputs_hash {
                Some(BuildReason::InputsChanged)
            } else if entry.options != job.options_hash {
                Some(BuildReason::OptionsChanged)
            } else {
                None
            }
        })
        .min();
    match reason {
        Some(reason) => JobStatus::Build { reason },
        None => JobStatus::UpToDate,
    }
}

fn find_stale(ctx: &Context, cache: &Cache, jobs: &[Job]) -> Vec<StaleOutput> {
    let produced: HashSet<String> = jobs
        .iter()
        .flat_map(|j| j.outputs.iter().map(|o| o.to_lowercase()))
        .collect();
    cache
        .entries()
        .filter(|(path, _)| !produced.contains(&path.to_lowercase()))
        .map(|(path, entry)| {
            let state = match file_hash(&ctx.config_dir.join(path)) {
                None => StaleState::Gone,
                Some(hash) if hash == entry.sha256 => StaleState::Present,
                Some(_) => StaleState::Changed,
            };
            StaleOutput {
                path: path.to_owned(),
                rule: entry.rule.clone(),
                state,
            }
        })
        .collect()
}
