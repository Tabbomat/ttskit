//! Local-only sanity check against real Tabletop Simulator saves.
//!
//! Set `TTSKIT_REAL_SAVES` to a directory (the test panics if it is set but not
//! an existing directory); every `*.json` file below it is linted (read only).
//! The test only asserts that linting does not panic and prints per-rule
//! finding counts. Without the variable it returns early, so it is skipped in
//! CI.

use std::collections::BTreeMap;
use std::time::Instant;

use ttskit_core::Save;
use ttskit_lint::{LintConfig, lint};
use ttskit_testutil::{collect_json, real_saves_dir};

#[test]
fn real_saves_lint_without_panic() {
    let Some(dir) = real_saves_dir() else {
        eprintln!("TTSKIT_REAL_SAVES not set; skipping real save lint check");
        return;
    };
    let config = LintConfig::default();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut examples: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut linted = 0usize;
    let mut elapsed = std::time::Duration::ZERO;
    for path in collect_json(&dir) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(save) = Save::from_slice(&bytes) else {
            continue;
        };
        let start = Instant::now();
        let findings = lint(&save, &config);
        elapsed += start.elapsed();
        linted += 1;
        for f in findings {
            *counts.entry(f.rule).or_default() += 1;
            let list = examples.entry(f.rule).or_default();
            if list.len() < 5 && !list.contains(&f.message) {
                list.push(f.message);
            }
        }
    }
    eprintln!("linted {linted} saves in {elapsed:?} (lint time only)");
    for (rule, n) in &counts {
        eprintln!("  {rule}: {n}");
        for message in &examples[rule] {
            eprintln!("      e.g. {message}");
        }
    }
}
