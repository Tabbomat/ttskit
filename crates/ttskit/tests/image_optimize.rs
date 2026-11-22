//! `ttskit image optimize` end to end. Needs the `image` feature.
#![cfg(feature = "image")]

use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let mut full = vec!["image", "optimize"];
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

/// A wasteful PNG: two colours stored as 8-bit RGBA.
fn fat_png(path: &Path) {
    image::RgbaImage::from_fn(64, 64, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            image::Rgba([200, 30, 30, 255])
        } else {
            image::Rgba([10, 10, 200, 255])
        }
    })
    .save(path)
    .unwrap();
}

fn pixels(path: &Path) -> Vec<u8> {
    image::open(path).unwrap().to_rgba8().into_raw()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

#[test]
fn without_a_destination_it_refuses_but_dry_run_works() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    fat_png(&png);
    let before = std::fs::read(&png).unwrap();

    let output = run(&[s(&png)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--out-dir"), "{}", stderr(&output));
    assert_eq!(std::fs::read(&png).unwrap(), before);

    let output = run(&[s(&png), "--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("dry run"), "{}", stdout(&output));
    assert_eq!(std::fs::read(&png).unwrap(), before);
}

#[test]
fn in_place_needs_yes_then_shrinks_and_keeps_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    fat_png(&png);
    let before = std::fs::read(&png).unwrap();
    let pixels_before = pixels(&png);

    let output = run(&[s(&png), "--in-place"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"));
    assert_eq!(std::fs::read(&png).unwrap(), before);

    let output = run(&[s(&png), "--in-place", "--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let after = std::fs::read(&png).unwrap();
    assert!(after.len() < before.len());
    assert_eq!(pixels(&png), pixels_before);
    assert!(stdout(&output).contains("1 of 1 PNG files optimized"));

    // A second run is stable: the file is not touched.
    let output = run(&[s(&png), "--in-place", "--yes", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(std::fs::read(&png).unwrap(), after);
    let v = json(&output);
    assert_eq!(v["files"][0]["status"], "unchanged");
    assert_eq!(v["files"][0]["written"], false);
    assert_eq!(v["totals"]["saved_bytes"], 0);
}

#[test]
fn dry_run_reports_savings_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    fat_png(&png);
    let before = std::fs::read(&png).unwrap();
    let output = run(&[s(&png), "--dry-run", "--in-place", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let v = json(&output);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["level"], 2);
    assert_eq!(v["strip"], "safe");
    let f = &v["files"][0];
    assert_eq!(f["status"], "optimized");
    assert_eq!(f["original_bytes"], before.len());
    assert!(f["optimized_bytes"].as_u64().unwrap() < before.len() as u64);
    assert_eq!(
        f["saved_bytes"].as_u64().unwrap(),
        before.len() as u64 - f["optimized_bytes"].as_u64().unwrap()
    );
    assert_eq!(f["written"], false);
    assert_eq!(v["totals"]["files"], 1);
    assert_eq!(v["totals"]["saved_bytes"], f["saved_bytes"]);
    assert_eq!(std::fs::read(&png).unwrap(), before);
}

#[test]
fn out_dir_mirrors_inputs_and_copies_files_that_do_not_shrink() {
    let dir = tempfile::tempdir().unwrap();
    let fat = dir.path().join("fat.png");
    fat_png(&fat);
    // Already optimized: cannot shrink further.
    let lean = dir.path().join("lean.png");
    std::fs::copy(&fat, &lean).unwrap();
    let output = run(&[s(&lean), "--in-place", "--yes", "--level", "6"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let lean_bytes = std::fs::read(&lean).unwrap();

    let out = dir.path().join("new").join("sub");
    let output = run(&[
        s(&fat),
        s(&lean),
        "--out-dir",
        s(&out),
        "--level",
        "6",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(std::fs::read(out.join("lean.png")).unwrap(), lean_bytes);
    assert!(
        std::fs::metadata(out.join("fat.png")).unwrap().len()
            < std::fs::metadata(&fat).unwrap().len()
    );
    assert_eq!(pixels(&out.join("fat.png")), pixels(&fat));
    let v = json(&output);
    let statuses: Vec<_> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["status"].as_str().unwrap().to_owned())
        .collect();
    assert!(statuses.contains(&"optimized".to_owned()));
    assert!(statuses.contains(&"unchanged".to_owned()));
}

#[test]
fn out_names_one_file_and_refuses_several_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");
    fat_png(&a);
    fat_png(&b);
    let target = dir.path().join("x.png");
    let output = run(&[s(dir.path()), "--out", s(&target)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--out takes exactly one input"));
    assert!(!target.exists());

    let output = run(&[s(&a), "--out", s(&target)]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(target.exists());
    assert!(a.exists(), "the input is kept");
}

#[test]
fn existing_output_is_not_overwritten_without_yes() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    fat_png(&a);
    let target = dir.path().join("x.png");
    std::fs::write(&target, b"precious").unwrap();
    let output = run(&[s(&a), "--out", s(&target)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"));
    assert_eq!(std::fs::read(&target).unwrap(), b"precious");
    let output = run(&[s(&a), "--out", s(&target), "--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_ne!(std::fs::read(&target).unwrap(), b"precious");
}

#[test]
fn non_png_files_are_skipped_with_a_note_and_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    fat_png(&dir.path().join("a.png"));
    image::RgbImage::new(4, 4)
        .save(dir.path().join("b.jpg"))
        .unwrap();
    let out = dir.path().join("out");
    let output = run(&[s(dir.path()), "--out-dir", s(&out), "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let v = json(&output);
    let skipped: Vec<_> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["status"] == "skipped")
        .collect();
    assert_eq!(skipped.len(), 1);
    assert!(skipped[0]["input"].as_str().unwrap().ends_with("b.jpg"));
    assert!(skipped[0]["note"].as_str().unwrap().contains("not a PNG"));
    assert!(!out.join("b.jpg").exists());
    assert!(out.join("a.png").exists());
    assert_eq!(v["totals"]["files"], 1);
}

#[test]
fn a_corrupt_png_fails_but_the_others_are_still_optimized() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("a.png");
    fat_png(&good);
    let bad = dir.path().join("b.png");
    let mut bytes = std::fs::read(&good).unwrap();
    bytes.truncate(bytes.len() / 2);
    std::fs::write(&bad, &bytes).unwrap();
    let before = std::fs::read(&good).unwrap();
    let output = run(&[s(dir.path()), "--in-place", "--yes"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("1 failed"), "{}", stdout(&output));
    assert!(std::fs::read(&good).unwrap().len() < before.len());
    assert_eq!(
        std::fs::read(&bad).unwrap(),
        bytes,
        "a failed file is untouched"
    );
}

#[test]
fn strip_and_level_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    fat_png(&a);
    assert_eq!(
        run(&[s(&a), "--dry-run", "--level", "7"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(&[s(&a), "--dry-run", "--strip", "some"]).status.code(),
        Some(2)
    );
    for strip in ["safe", "all", "none"] {
        let output = run(&[s(&a), "--dry-run", "--strip", strip, "--level", "0"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{strip}: {}",
            stderr(&output)
        );
    }
    assert_eq!(
        run(&[s(&a), "--in-place", "--out-dir", "x"]).status.code(),
        Some(2)
    );
}

#[test]
fn optimizing_the_same_file_twice_gives_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    fat_png(&a);
    let one = dir.path().join("one.png");
    let two = dir.path().join("two.png");
    assert_eq!(
        run(&[s(&a), "--out", s(&one), "--level", "6"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(&[s(&a), "--out", s(&two), "--level", "6"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap());
}
