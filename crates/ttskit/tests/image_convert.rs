//! `ttskit image convert` end to end. Needs the `image` feature.
#![cfg(feature = "image")]

use std::path::Path;
use std::process::{Command, Output};

fn ttskit(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn save(path: &Path, w: u32, h: u32, alpha: u8) {
    image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 9, alpha]))
        .save(path)
        .unwrap();
}

fn run(args: &[&str]) -> Output {
    let mut full = vec!["image", "convert"];
    full.extend_from_slice(args);
    ttskit(&full)
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn converts_next_to_the_input_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let bmp = dir.path().join("a.bmp");
    save(&bmp, 3, 2, 255);
    let output = run(&[s(&bmp), "--to", "png"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let png = dir.path().join("a.png");
    assert_eq!(image::open(&png).unwrap().to_rgba8().dimensions(), (3, 2));
    assert!(stdout(&output).contains("1 converted, 0 failed"));
    assert!(bmp.exists(), "the input is kept");
}

#[test]
fn out_dir_names_files_after_inputs_and_is_created() {
    let dir = tempfile::tempdir().unwrap();
    save(&dir.path().join("b.png"), 2, 2, 255);
    save(&dir.path().join("a.bmp"), 2, 2, 255);
    let out = dir.path().join("new").join("sub");
    let output = run(&[s(dir.path()), "--to", "jpg", "--out-dir", s(&out)]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let jpg = std::fs::read(out.join("a.jpg")).unwrap();
    assert_eq!(&jpg[..2], &[0xFF, 0xD8]);
    assert!(out.join("b.jpg").exists());
    let text = stdout(&output);
    assert!(
        text.find("a.bmp").unwrap() < text.find("b.png").unwrap(),
        "{text}"
    );
}

#[test]
fn out_names_the_file_for_a_single_input() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.png");
    save(&src, 2, 2, 255);
    let target = dir.path().join("renamed.dat");
    let output = run(&[s(&src), "--to", "png", "--out", s(&target)]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(target.exists());
}

#[test]
fn out_with_several_inputs_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    save(&dir.path().join("a.png"), 2, 2, 255);
    save(&dir.path().join("b.png"), 2, 2, 255);
    let target = dir.path().join("x.jpg");
    let output = run(&[s(dir.path()), "--to", "jpg", "--out", s(&target)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--out takes exactly one input"));
    assert!(!target.exists());
}

#[test]
fn out_and_out_dir_conflict() {
    let output = run(&["a.png", "--to", "png", "--out", "x", "--out-dir", "y"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn existing_output_is_not_overwritten_without_yes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.bmp");
    save(&src, 2, 2, 255);
    let existing = dir.path().join("a.png");
    std::fs::write(&existing, b"precious").unwrap();
    let output = run(&[s(&src), "--to", "png"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"));
    assert_eq!(std::fs::read(&existing).unwrap(), b"precious");

    let output = run(&[s(&src), "--to", "png", "--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("overwrites"));
    assert_ne!(std::fs::read(&existing).unwrap(), b"precious");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !leftovers.iter().any(|n| n.contains("tmp")),
        "{leftovers:?}"
    );
}

#[test]
fn refusal_writes_nothing_for_the_other_files() {
    let dir = tempfile::tempdir().unwrap();
    save(&dir.path().join("a.bmp"), 2, 2, 255);
    save(&dir.path().join("b.bmp"), 2, 2, 255);
    std::fs::write(dir.path().join("b.png"), b"x").unwrap();
    let output = run(&[s(dir.path()), "--to", "png"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!dir.path().join("a.png").exists());
}

#[test]
fn converting_a_file_onto_itself_needs_yes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.png");
    save(&src, 2, 2, 255);
    let before = std::fs::read(&src).unwrap();
    let output = run(&[s(&src), "--to", "png"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read(&src).unwrap(), before);
    let output = run(&[s(&src), "--to", "png", "--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

#[test]
fn two_inputs_with_the_same_output_name_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    save(&dir.path().join("a.png"), 2, 2, 255);
    save(&dir.path().join("a.bmp"), 2, 2, 255);
    let out = dir.path().join("out");
    let output = run(&[s(dir.path()), "--to", "png", "--out-dir", s(&out)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("would both write"));
    assert!(!out.exists());
}

#[test]
fn an_output_that_is_another_input_is_refused_even_with_yes() {
    let dir = tempfile::tempdir().unwrap();
    save(&dir.path().join("a.png"), 2, 2, 255);
    // A BMP named .jpg: the format is decided by content.
    image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3]))
        .save_with_format(dir.path().join("a.jpg"), image::ImageFormat::Bmp)
        .unwrap();
    let output = run(&[s(dir.path()), "--to", "jpg", "--yes"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("another input"));
}

#[test]
fn dry_run_writes_nothing_but_reports_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.bmp");
    save(&src, 2, 2, 255);
    let out = dir.path().join("out");
    let output = run(&[s(&src), "--to", "png", "--out-dir", s(&out), "--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("a.png"), "{text}");
    assert!(text.contains("dry run, nothing written"), "{text}");
    assert!(!out.exists());
}

#[test]
fn dry_run_still_refuses_existing_files_and_lost_alpha() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.png");
    save(&src, 2, 2, 100);
    let output = run(&[s(&src), "--to", "jpg", "--dry-run"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("--background"));
    std::fs::write(dir.path().join("a.jpg"), b"x").unwrap();
    let output = run(&[s(&src), "--to", "jpg", "--dry-run"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"));
}

#[test]
fn transparency_to_jpg_is_refused_and_background_allows_it() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.png");
    save(&src, 4, 4, 100);
    let output = run(&[s(&src), "--to", "jpg"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert!(text.contains("--background"), "{text}");
    assert!(text.contains("0 converted, 1 failed"), "{text}");
    assert!(!dir.path().join("a.jpg").exists());

    let output = run(&[s(&src), "--to", "jpeg", "--background", "#ffffff"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("note: 16 transparent or translucent pixels flattened"));
    assert!(dir.path().join("a.jpg").exists());
}

#[test]
fn one_failure_does_not_stop_the_other_files() {
    let dir = tempfile::tempdir().unwrap();
    save(&dir.path().join("a_alpha.png"), 2, 2, 100);
    save(&dir.path().join("b_ok.bmp"), 2, 2, 255);
    let output = run(&[s(dir.path()), "--to", "jpg"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(dir.path().join("b_ok.jpg").exists());
    assert!(!dir.path().join("a_alpha.jpg").exists());
}

#[test]
fn jpeg_only_options_and_bad_values_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.bmp");
    save(&src, 2, 2, 255);
    let output = run(&[s(&src), "--to", "png", "--quality", "50"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--quality only applies"));
    let output = run(&[s(&src), "--to", "png", "--background", "white"]);
    assert_eq!(output.status.code(), Some(1));
    let output = run(&[s(&src), "--to", "jpg", "--quality", "0"]);
    assert_eq!(output.status.code(), Some(2));
    let output = run(&[s(&src), "--to", "jpg", "--background", "red"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("invalid colour"));
    let output = run(&[s(&src), "--to", "gif"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!dir.path().join("a.png").exists());
}

#[test]
fn quality_changes_the_jpeg_size() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.png");
    image::RgbImage::from_fn(64, 64, |x, y| {
        image::Rgb([(x * x) as u8, (y * 7) as u8, (x ^ y) as u8])
    })
    .save(&src)
    .unwrap();
    let low = dir.path().join("low.jpg");
    let high = dir.path().join("high.jpg");
    let a = run(&[s(&src), "--to", "jpg", "--quality", "10", "--out", s(&low)]);
    let b = run(&[s(&src), "--to", "jpg", "--quality", "95", "--out", s(&high)]);
    assert_eq!((a.status.code(), b.status.code()), (Some(0), Some(0)));
    assert!(std::fs::metadata(&low).unwrap().len() < std::fs::metadata(&high).unwrap().len());
}

#[test]
fn json_report_and_failures() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("a.bmp");
    save(&good, 3, 2, 255);
    let bad = dir.path().join("bad.bin");
    std::fs::write(&bad, b"nope").unwrap();
    let output = run(&["--json", s(&good), s(&bad), "--to", "png"]);
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["dry_run"], false);
    assert_eq!(value["conversions"].as_array().unwrap().len(), 1);
    let c = &value["conversions"][0];
    assert_eq!(c["from"], "bmp");
    assert_eq!(c["to"], "png");
    assert_eq!(
        (c["width"].as_u64(), c["height"].as_u64()),
        (Some(3), Some(2))
    );
    assert_eq!(c["overwrote"], false);
    assert_eq!(value["errors"].as_array().unwrap().len(), 1);
}

#[test]
fn output_is_identical_between_runs() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.png");
    save(&src, 8, 8, 100);
    let one = dir.path().join("one.jpg");
    let two = dir.path().join("two.jpg");
    for target in [&one, &two] {
        let output = run(&[
            s(&src),
            "--to",
            "jpg",
            "--background",
            "#336699",
            "--out",
            s(target),
        ]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    }
    assert_eq!(std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap());
}

#[test]
fn keep_metadata_flag_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.bmp");
    save(&src, 2, 2, 255);
    let output = run(&[s(&src), "--to", "png", "--keep-metadata"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}
