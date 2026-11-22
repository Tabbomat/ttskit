//! `ttskit image check` end to end. Needs the `image` feature.
#![cfg(feature = "image")]

use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let mut full = vec!["image", "check"];
    full.extend_from_slice(args);
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(&full)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

fn rgb(path: &Path, w: u32, h: u32) {
    image::RgbImage::from_pixel(w, h, image::Rgb([1, 2, 3]))
        .save(path)
        .unwrap();
}

#[test]
fn clean_files_exit_0() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    rgb(&png, 16, 8);
    let output = run(&[s(&png)]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("png 16x8"), "{text}");
    assert!(text.contains("1 checked, 0 with findings"), "{text}");
}

#[test]
fn findings_exit_2_and_name_the_rule() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("big.png");
    rgb(&png, 40, 8);
    let output = run(&[s(&png), "--max-side", "32"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stdout(&output).contains("side-too-long"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn unreadable_file_exits_1_even_with_findings() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.png");
    rgb(&big, 40, 8);
    let bad = dir.path().join("bad.png");
    std::fs::write(&bad, b"not an image").unwrap();
    let output = run(&[s(&big), s(&bad), "--max-side", "32"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert!(
        text.contains("side-too-long") && text.contains("error:"),
        "{text}"
    );
}

#[test]
fn json_has_limits_images_and_findings() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.jpg");
    image::RgbImage::from_pixel(40, 8, image::Rgb([1, 2, 3]))
        .save_with_format(&png, image::ImageFormat::Png)
        .unwrap();
    let output = run(&[s(&png), "--max-bytes", "1", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    let v = json(&output);
    assert_eq!(v["max_side"], 4096);
    assert_eq!(v["max_bytes"], 1);
    let image = &v["images"][0];
    assert_eq!(image["format"], "png");
    assert_eq!(image["width"], 40);
    let rules: Vec<&str> = image["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert_eq!(rules, ["file-too-large", "extension-mismatch"]);
    assert_eq!(v["errors"].as_array().unwrap().len(), 0);
}

#[test]
fn directories_are_scanned_and_sizes_take_suffixes() {
    let dir = tempfile::tempdir().unwrap();
    rgb(&dir.path().join("a.png"), 8, 8);
    rgb(&dir.path().join("b.png"), 8, 8);
    let output = run(&[s(dir.path()), "--max-bytes", "1k", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json(&output)["max_bytes"], 1024);
    assert_eq!(json(&output)["images"].as_array().unwrap().len(), 2);
    let bad = run(&[s(dir.path()), "--max-bytes", "lots"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("invalid size"));
}
