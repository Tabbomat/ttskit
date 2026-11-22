//! `ttskit image quantize` end to end. Needs the `image` feature.
#![cfg(feature = "image")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let mut full = vec!["image", "quantize"];
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

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

/// A smooth RGBA gradient with far more than 256 colours.
fn gradient(path: &Path) {
    image::RgbaImage::from_fn(96, 96, |x, y| {
        image::Rgba([
            (x * 2) as u8,
            (y * 2) as u8,
            ((x + y) % 256) as u8,
            255 - x as u8,
        ])
    })
    .save(path)
    .unwrap();
}

/// Four distinct colours, stored wastefully as RGBA.
fn few_colors(path: &Path) {
    image::RgbaImage::from_fn(64, 64, |x, y| match (x / 32, y / 32) {
        (0, 0) => image::Rgba([200, 30, 30, 255]),
        (1, 0) => image::Rgba([10, 10, 200, 255]),
        (0, 1) => image::Rgba([10, 200, 10, 255]),
        _ => image::Rgba([250, 250, 0, 255]),
    })
    .save(path)
    .unwrap();
}

fn decode(path: &Path) -> image::RgbaImage {
    image::open(path).unwrap().to_rgba8()
}

fn distinct_colors(path: &Path) -> usize {
    let mut all: Vec<[u8; 4]> = decode(path).pixels().map(|p| p.0).collect();
    all.sort_unstable();
    all.dedup();
    all.len()
}

/// The colour type in the IHDR chunk is 3 for an indexed PNG.
fn is_indexed(path: &Path) -> bool {
    std::fs::read(path).unwrap()[25] == 3
}

#[test]
fn without_a_destination_it_refuses_but_dry_run_works() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    gradient(&png);
    let before = std::fs::read(&png).unwrap();

    let output = run(&[s(&png)]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("--out-dir") && err.contains("lossy"), "{err}");
    assert_eq!(std::fs::read(&png).unwrap(), before);

    let output = run(&[s(&png), "--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("dry run"), "{}", stdout(&output));
    assert_eq!(std::fs::read(&png).unwrap(), before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn out_writes_a_smaller_indexed_png_with_at_most_n_colors() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    gradient(&png);
    let target = dir.path().join("q.png");
    let output = run(&[s(&png), "--out", s(&target), "--colors", "32"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("lossy"), "{text}");
    assert!(text.contains("colors, quality"), "{text}");
    assert!(text.contains("1 of 1 PNG files quantized"), "{text}");
    assert!(is_indexed(&target));
    assert!(distinct_colors(&target) <= 32);
    assert!(std::fs::metadata(&target).unwrap().len() < std::fs::metadata(&png).unwrap().len());
    assert_eq!(decode(&target).dimensions(), (96, 96));
    assert!(png.exists(), "the input is kept");
}

#[test]
fn out_dir_mirrors_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");
    gradient(&a);
    few_colors(&b);
    let out = dir.path().join("new").join("sub");
    let output = run(&[s(&a), s(&b), "--out-dir", s(&out), "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(out.join("a.png").exists());
    assert!(out.join("b.png").exists());
    assert!(distinct_colors(&out.join("a.png")) <= 256);
    assert_eq!(distinct_colors(&out.join("b.png")), 4);
    assert_eq!(json(&output)["totals"]["files"], 2);
}

#[test]
fn in_place_needs_yes_then_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    gradient(&png);
    let before = std::fs::read(&png).unwrap();

    let output = run(&[s(&png), "--in-place"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"));
    assert_eq!(std::fs::read(&png).unwrap(), before);

    let output = run(&[s(&png), "--in-place", "--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let after = std::fs::read(&png).unwrap();
    assert!(after.len() < before.len());
    assert!(is_indexed(&png));

    // Quantizing the result again keeps every pixel (it already fits the
    // palette) and never makes the file larger.
    let pixels = decode(&png);
    let output = run(&[s(&png), "--in-place", "--yes", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(decode(&png), pixels);
    assert!(std::fs::read(&png).unwrap().len() <= after.len());
    let f = &json(&output)["files"][0];
    assert_eq!(f["written"], f["status"] == "quantized");
}

#[test]
fn existing_output_is_not_overwritten_without_yes() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    gradient(&a);
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
fn an_unreachable_quality_skips_the_file_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    gradient(&a);
    let before = std::fs::read(&a).unwrap();
    let target = dir.path().join("x.png");
    let output = run(&[
        s(&a),
        "--out",
        s(&target),
        "--colors",
        "4",
        "--quality",
        "100-100",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let v = json(&output);
    assert_eq!(v["quality"], "100-100");
    let f = &v["files"][0];
    assert_eq!(f["status"], "skipped");
    assert_eq!(f["code"], "quality-too-low");
    assert_eq!(f["written"], false);
    assert!(f["output"].is_null());
    assert!(!target.exists());
    assert_eq!(std::fs::read(&a).unwrap(), before);
    assert_eq!(v["totals"]["files"], 0);
}

#[test]
fn invalid_options_are_rejected_by_the_parser() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    gradient(&a);
    let a = s(&a);
    for bad in [
        &["--quality", "90"][..],
        &["--quality", "90-60"],
        &["--quality", "0-101"],
        &["--quality", "a-b"],
        &["--dither", "1.5"],
        &["--dither", "-0.5"],
        &["--dither", "NaN"],
        &["--dither", "x"],
        &["--colors", "1"],
        &["--colors", "257"],
        &["--level", "7"],
        &["--strip", "some"],
        &["--engine", "other"],
    ] {
        let mut args = vec![a, "--dry-run"];
        args.extend_from_slice(bad);
        let output = run(&args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{bad:?}: {}",
            stderr(&output)
        );
    }
    let output = run(&[a, "--dry-run", "--quality", "90"]);
    assert!(stderr(&output).contains("MIN-MAX"), "{}", stderr(&output));
    for ok in [
        &["--quality", "0-100"][..],
        &["--dither", "0"],
        &["--dither", "0.5"],
        &["--colors", "2"],
    ] {
        let mut args = vec![a, "--dry-run"];
        args.extend_from_slice(ok);
        let output = run(&args);
        assert_eq!(output.status.code(), Some(0), "{ok:?}: {}", stderr(&output));
    }
    // Conflicting destinations.
    assert_eq!(
        run(&[a, "--in-place", "--out-dir", "x"]).status.code(),
        Some(2)
    );
}

#[test]
fn non_png_files_are_skipped_without_a_target() {
    let dir = tempfile::tempdir().unwrap();
    gradient(&dir.path().join("a.png"));
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
    assert_eq!(skipped[0]["code"], "not-png");
    assert!(skipped[0]["note"].as_str().unwrap().contains("not a PNG"));
    assert!(!out.join("b.jpg").exists());
    assert!(out.join("a.png").exists());
}

#[test]
fn json_has_the_documented_shape() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    gradient(&a);
    let before = std::fs::metadata(&a).unwrap().len();
    let output = run(&[
        s(&a),
        "--dry-run",
        "--json",
        "--colors",
        "16",
        "--dither",
        "0.5",
        "--level",
        "1",
        "--strip",
        "all",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let v = json(&output);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["lossy"], true);
    assert_eq!(v["engine"], "builtin");
    assert_eq!(v["colors"], 16);
    assert!(v["quality"].is_null());
    assert_eq!(v["dither"], 0.5);
    assert_eq!(v["level"], 1);
    assert_eq!(v["strip"], "all");
    assert!(v["errors"].as_array().unwrap().is_empty());
    let f = &v["files"][0];
    assert_eq!(f["status"], "quantized");
    assert!(f["code"].is_null());
    assert!(f["output"].is_null());
    assert_eq!(f["original_bytes"], before);
    let new = f["quantized_bytes"].as_u64().unwrap();
    assert!(new < before);
    assert_eq!(f["saved_bytes"].as_u64().unwrap(), before - new);
    assert!(f["colors"].as_u64().unwrap() <= 16);
    assert!(f["quality"].as_u64().unwrap() <= 100);
    assert_eq!(f["written"], false);
    assert!(f["note"].is_null());
    assert_eq!(v["totals"]["files"], 1);
    assert_eq!(v["totals"]["saved_bytes"], f["saved_bytes"]);
    assert_eq!(v["totals"]["original_bytes"], before);
    assert_eq!(v["totals"]["quantized_bytes"], new);
}

#[test]
fn output_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    gradient(&a);
    let one = dir.path().join("one.png");
    let two = dir.path().join("two.png");
    for target in [&one, &two] {
        let output = run(&[s(&a), "--out", s(target), "--colors", "64"]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    }
    assert_eq!(std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap());
}

// The pngquant engine, against a fake executable.

/// A fake `pngquant` in `dir` that logs its arguments to `args.txt`, ignores
/// stdin and writes `canned.png` to stdout.
fn fake_pngquant(dir: &Path, canned: &[u8], code: i32) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("canned.png"), canned).unwrap();
    #[cfg(windows)]
    {
        let script = format!(
            "@echo off\r\necho %* > \"%~dp0args.txt\"\r\ntype \"%~dp0canned.png\"\r\nexit /b {code}\r\n"
        );
        let path = dir.join("pngquant.cmd");
        std::fs::write(&path, script).unwrap();
        path
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/bin/sh\nd=\"$(dirname \"$0\")\"\necho \"$*\" > \"$d/args.txt\"\ncat \"$d/canned.png\"\nexit {code}\n"
        );
        let path = dir.join("pngquant");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

/// The logged line; Windows quotes arguments with `=` for `.cmd` files.
fn logged_args(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("args.txt"))
        .unwrap()
        .trim()
        .replace('"', "")
}

#[test]
fn pngquant_engine_runs_the_given_program_without_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    gradient(&a);
    // The canned result is a 16-colour version made by the built-in engine.
    let canned_path = dir.path().join("canned-src.png");
    let output = run(&[s(&a), "--out", s(&canned_path), "--colors", "16"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let canned = std::fs::read(&canned_path).unwrap();

    // A directory with a space proves the path is not split by a shell.
    let bin = dir.path().join("fake bin");
    let program = fake_pngquant(&bin, &canned, 0);
    let target = dir.path().join("q.png");
    let output = run(&[
        s(&a),
        "--out",
        s(&target),
        "--engine",
        "pngquant",
        "--pngquant",
        s(&program),
        "--colors",
        "16",
        "--quality",
        "40-80",
        "--dither",
        "0.5",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let v = json(&output);
    assert_eq!(v["engine"], "pngquant");
    assert_eq!(v["files"][0]["status"], "quantized");
    assert_eq!(decode(&target), decode(&canned_path));
    assert_eq!(
        logged_args(&bin),
        "--quality 40-80 --floyd=0.5 --strip --output - 16 -"
    );

    // --pngquant alone implies the engine.
    let output = run(&[s(&a), "--dry-run", "--pngquant", s(&program), "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(json(&output)["engine"], "pngquant");
}

#[test]
fn pngquant_below_quality_is_a_skip() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    gradient(&a);
    let program = fake_pngquant(&dir.path().join("bin"), b"", 99);
    let target = dir.path().join("q.png");
    let output = run(&[
        s(&a),
        "--out",
        s(&target),
        "--pngquant",
        s(&program),
        "--quality",
        "50-90",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(json(&output)["files"][0]["code"], "quality-too-low");
    assert!(!target.exists());
}

#[test]
fn a_missing_pngquant_fails_the_run_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");
    gradient(&a);
    gradient(&b);
    let out = dir.path().join("out");
    let missing = dir.path().join("no such dir").join("pngquant");
    let output = run(&[
        s(dir.path()),
        "--out-dir",
        s(&out),
        "--pngquant",
        s(&missing),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("pngquant"), "{}", stderr(&output));
    assert!(!out.exists());

    // `--engine pngquant` alone: a pngquant that is not on PATH is the same
    // kind of failure, if there is none on this machine.
    let output = run(&[s(&a), "--dry-run", "--engine", "builtin", "--pngquant", "x"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("--pngquant"),
        "{}",
        stderr(&output)
    );
}
