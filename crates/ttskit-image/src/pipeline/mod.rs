//! The declarative image pipeline of the project config (`ttskit.toml`).
//!
//! `[[image.pipeline]]` rules say which source images become which output
//! files and which steps run on them. `parse_pipeline` validates the rules.
//! The engine then works in four calls: `Cache::load`, `plan` (reads, never
//! writes), `build` and optionally `prune`, then `Cache::save`.

mod build;
mod cache;
mod config;
mod plan;
mod values;

#[cfg(test)]
mod build_tests;
#[cfg(test)]
mod tests;

pub use build::{
    BuildReport, JobReport, JobResult, KeptOutput, Note, OutputReport, PruneReport, build, prune,
};
pub use cache::{BuildError, CACHE_FILE_NAME, Cache, CacheEntry};
pub use config::{DeckRule, Pipeline, PipelineError, Rule, Step, parse_pipeline};
pub use plan::{BuildReason, Context, Job, JobStatus, Plan, StaleOutput, StaleState, plan};
pub use values::{parse_card_size, parse_dither, parse_quality, parse_scale};
