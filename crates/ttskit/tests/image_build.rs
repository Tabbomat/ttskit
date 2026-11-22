//! `ttskit image build` end to end. Needs the `image` feature.
#![cfg(feature = "image")]

use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let mut full = vec!["image", "build"];
    full.extend_from_slice(args);
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(&full)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn png(path: &Path, size: u32, seed: u8) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbaImage::from_fn(size, size, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            image::Rgba([200, seed, 30, 255])
        } else {
            image::Rgba([10, 10, seed, 255])
        }
    })
    .save(path)
    .unwrap();
}

const BOARDS: &str = r#"
[[image.pipeline]]
src = "art/boards/*.png"
out = "assets/Boards"
steps = [{ op = "resize", max-side = 32 }, { op = "optimize" }]
"#;

const DECK: &str = r#"
[[image.pipeline]]
src = "art/cards/*.png"
out = "assets/Cards"
steps = [{ op = "optimize" }]
deck = { name = "base", columns = 2, rows = 2, card-size = "16x16" }
"#;

/// A project with two boards, written with `rules` as its config.
fn project(rules: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    png(&dir.path().join("art/boards/a.png"), 64, 100);
    png(&dir.path().join("art/boards/b.png"), 64, 150);
    std::fs::write(dir.path().join("ttskit.toml"), rules).unwrap();
    dir
}

fn config(dir: &tempfile::TempDir) -> String {
    s(&dir.path().join("ttskit.toml")).to_owned()
}

#[test]
fn dry_run_prints_the_plan_and_writes_nothing() {
    let dir = project(BOARDS);
    let output = run(&["--config", &config(&dir), "--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("build  assets/Boards/a.png  <- art/boards/a.png (new)"),
        "{text}"
    );
    assert!(text.contains("nothing was written"), "{text}");
    assert!(!dir.path().join("assets").exists());
    assert!(!dir.path().join("ttskit-image.lock.json").exists());
}

#[test]
fn first_build_writes_outputs_and_cache_and_second_build_changes_nothing() {
    let dir = project(BOARDS);
    let cfg = config(&dir);
    let a = dir.path().join("assets/Boards/a.png");
    let lock = dir.path().join("ttskit-image.lock.json");

    let output = run(&["--config", &cfg]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("2 built, 0 up to date, 0 failed"),
        "{}",
        stdout(&output)
    );
    assert_eq!(image::open(&a).unwrap().width(), 32);
    assert!(dir.path().join("assets/Boards/b.png").exists());
    assert!(lock.exists());
    let bytes = std::fs::read(&a).unwrap();
    let lock_bytes = std::fs::read(&lock).unwrap();

    let output = run(&["--config", &cfg]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("0 built, 2 up to date, 0 failed"),
        "{}",
        stdout(&output)
    );
    assert_eq!(std::fs::read(&a).unwrap(), bytes);
    assert_eq!(std::fs::read(&lock).unwrap(), lock_bytes);

    // A hand-edited output is rebuilt.
    std::fs::write(&a, b"junk").unwrap();
    let output = run(&["--config", &cfg]);
    assert!(
        stdout(&output).contains("1 built, 1 up to date"),
        "{}",
        stdout(&output)
    );
    assert_eq!(std::fs::read(&a).unwrap(), bytes);
}

#[test]
fn a_deck_rule_builds_sheets_and_a_description() {
    let dir = project(DECK);
    for (i, name) in ["c1", "c2", "c3"].iter().enumerate() {
        png(
            &dir.path().join(format!("art/cards/{name}.png")),
            16,
            i as u8 * 40,
        );
    }
    let output = run(&["--config", &config(&dir)]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(dir.path().join("assets/Cards/base.png").exists());
    assert!(dir.path().join("assets/Cards/base.json").exists());
    assert!(
        stdout(&output).contains("deck base, 3 cards"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn json_output_has_the_four_keys() {
    let dir = project(BOARDS);
    let cfg = config(&dir);

    let output = run(&["--config", &cfg, "--dry-run", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["dry_run"], true);
    assert!(value["plan"]["jobs"].is_array());
    assert!(value["build"].is_null());
    assert!(value["prune"].is_null());

    let output = run(&["--config", &cfg, "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", stdout(&output)));
    assert_eq!(value["dry_run"], false);
    assert_eq!(value["build"]["jobs"].as_array().unwrap().len(), 2);
    assert!(value["prune"].is_null());
    assert!(value.get("plan").is_some());
}

#[test]
fn prune_needs_yes_and_removes_only_stale_outputs_then() {
    let dir = project(&format!("{BOARDS}{DECK}"));
    png(&dir.path().join("art/cards/c1.png"), 16, 1);
    png(&dir.path().join("art/cards/c2.png"), 16, 2);
    let cfg = config(&dir);
    assert_eq!(run(&["--config", &cfg]).status.code(), Some(0));
    let sheet = dir.path().join("assets/Cards/base.png");
    assert!(sheet.exists());

    // Delete the deck rule.
    std::fs::write(dir.path().join("ttskit.toml"), BOARDS).unwrap();
    let lock = std::fs::read(dir.path().join("ttskit-image.lock.json")).unwrap();

    let output = run(&["--config", &cfg, "--prune"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"), "{}", stderr(&output));
    assert!(sheet.exists());
    assert_eq!(
        std::fs::read(dir.path().join("ttskit-image.lock.json")).unwrap(),
        lock
    );

    // Without --prune the stale outputs are only reported.
    let output = run(&["--config", &cfg]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("stale  assets/Cards/base.png (remove with --prune --yes)"),
        "{}",
        stdout(&output)
    );
    assert!(sheet.exists());

    let output = run(&["--config", &cfg, "--prune", "--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(sheet.exists());

    let output = run(&["--config", &cfg, "--prune", "--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!sheet.exists());
    assert!(!dir.path().join("assets/Cards/base.json").exists());
    assert!(dir.path().join("assets/Boards/a.png").exists());
}

#[test]
fn conflicts_fail_with_all_of_them_listed_and_write_nothing() {
    let rules = r#"
[[image.pipeline]]
src = "art/boards/*.png"
out = "assets"

[[image.pipeline]]
src = "art/boards/*.png"
out = "assets"
"#;
    let dir = project(rules);
    let output = run(&["--config", &config(&dir)]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("conflicting outputs"), "{err}");
    assert!(err.contains("assets/a.png"), "{err}");
    assert!(err.contains("assets/b.png"), "{err}");
    assert!(!dir.path().join("assets").exists());
    assert!(!dir.path().join("ttskit-image.lock.json").exists());
}

#[test]
fn missing_config_and_config_without_rules_are_clear_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = s(&dir.path().join("ttskit.toml")).to_owned();
    let output = run(&["--config", &cfg]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("ttskit.toml"),
        "{}",
        stderr(&output)
    );

    std::fs::write(&cfg, "[cloud]\nfolder = \"x\"\n").unwrap();
    let output = run(&["--config", &cfg]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no [[image.pipeline]] rules in"),
        "{}",
        stderr(&output)
    );

    std::fs::write(&cfg, "[[image.pipeline]]\nsrc = \"a\"\n").unwrap();
    let output = run(&["--config", &cfg]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("out"), "{}", stderr(&output));
}
