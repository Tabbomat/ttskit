//! Lint rules for Tabletop Simulator saves.
//!
//! [`lint`] walks a [`ttskit_core::Save`] once and runs every enabled rule on
//! each scene object (an entry of the top-level `ObjectStates`, of any
//! object's `ContainedObjects`, or a value of any object's `States` map).
//! Rules live in `src/rules/<name>.rs` and are listed in [`RULES`]. Severities
//! and switches come from a [`LintConfig`], usually read from the `[lint]`
//! table of a `ttskit.toml`.

mod config;
mod ctx;
mod rules;
mod walk;

#[cfg(test)]
mod tests;

pub use config::{LintConfig, LintError, Setting, Severity};
pub use ctx::Finding;
pub use rules::{RULES, RuleInfo};

use ttskit_core::Save;

/// Lints `save`. Findings are in document order; rules set to off produce
/// nothing and every finding carries the severity from `config`.
pub fn lint(save: &Save, config: &LintConfig) -> Vec<Finding> {
    walk::run(save, config)
}
