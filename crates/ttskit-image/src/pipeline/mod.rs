//! The declarative image pipeline of the project config (`ttskit.toml`).
//!
//! `[[image.pipeline]]` rules say which source images become which output
//! files and which steps run on them. This module only parses and validates
//! the rules; it neither reads images nor plans or runs anything.

mod config;
mod values;

#[cfg(test)]
mod tests;

pub use config::{DeckRule, Pipeline, PipelineError, Rule, Step, parse_pipeline};
pub use values::{parse_card_size, parse_dither, parse_quality, parse_scale};
