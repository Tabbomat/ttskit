//! `ttskit image resize` end to end. Needs the `image` feature.
#![cfg(feature = "image")]

use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let mut full = vec!["image", "resize"];
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

fn picture(path: &Path, w: u32, h: u32) {
    image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 50])
    })
    .save(path)
    .unwrap();
}

fn dims(path: &Path) -> (u32, u32) {
    let (w, h) = image::image_dimensions(path).unwrap();
    (w, h)
}

#[test]
fn needs_exactly_one_size_option() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    picture(&png, 40, 20);
    let none = run(&[s(&png), "--dry-run"]);
    assert!(!none.status.success());
    assert!(stderr(&none).contains("--max-side"), "{}", stderr(&none));
    let two = run(&[s(&png), "--max-side", "10", "--width", "10", "--dry-run"]);
    assert!(!two.status.success());
    assert!(
        !run(&[s(&png), "--scale", "0", "--dry-run"])
            .status
            .success()
    );
}

#[test]
fn without_a_destination_it_refuses_but_dry_run_works() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    picture(&png, 40, 20);
    let before = std::fs::read(&png).unwrap();

    let output = run(&[s(&png), "--max-side", "10"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--out-dir"), "{}", stderr(&output));

    let dry = run(&[s(&png), "--max-side", "10", "--dry-run"]);
    assert_eq!(dry.status.code(), Some(0));
    assert!(stdout(&dry).contains("40x20 -> 10x5"), "{}", stdout(&dry));
    assert!(stdout(&dry).contains("dry run"));
    assert_eq!(std::fs::read(&png).unwrap(), before);
}

#[test]
fn out_writes_a_smaller_copy_and_keeps_the_input() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    let out = dir.path().join("small.png");
    picture(&png, 40, 20);
    let before = std::fs::read(&png).unwrap();
    let output = run(&[s(&png), "--max-side", "10", "--out", s(&out)]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(dims(&out), (10, 5));
    assert_eq!(std::fs::read(&png).unwrap(), before);
}

#[test]
fn refuses_to_overwrite_without_yes() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    let out = dir.path().join("small.png");
    picture(&png, 40, 20);
    std::fs::write(&out, b"precious").unwrap();
    let refused = run(&[s(&png), "--max-side", "10", "--out", s(&out)]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
    assert_eq!(std::fs::read(&out).unwrap(), b"precious");
    let allowed = run(&[s(&png), "--max-side", "10", "--out", s(&out), "--yes"]);
    assert_eq!(allowed.status.code(), Some(0));
    assert_eq!(dims(&out), (10, 5));
}

#[test]
fn in_place_needs_yes_and_replaces_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    picture(&png, 40, 20);
    let refused = run(&[s(&png), "--width", "20", "--in-place"]);
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(dims(&png), (40, 20));
    let ok = run(&[s(&png), "--width", "20", "--in-place", "--yes"]);
    assert_eq!(ok.status.code(), Some(0), "{}", stdout(&ok));
    assert_eq!(dims(&png), (20, 10));
}

#[test]
fn never_enlarges_unless_asked() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    let out_dir = dir.path().join("out");
    picture(&png, 40, 20);
    let before = std::fs::read(&png).unwrap();

    // In place: not touched at all.
    let output = run(&[s(&png), "--max-side", "80", "--in-place", "--yes", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let v = json(&output);
    assert_eq!(v["files"][0]["status"], "unchanged");
    assert_eq!(v["files"][0]["code"], "would-enlarge");
    assert_eq!(std::fs::read(&png).unwrap(), before);

    // Separate output: the original bytes are copied.
    let output = run(&[s(&png), "--scale", "2", "--out-dir", s(&out_dir)]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(std::fs::read(out_dir.join("a.png")).unwrap(), before);

    // Asked for: it grows.
    let big = dir.path().join("big.png");
    let output = run(&[
        s(&png),
        "--max-side",
        "80",
        "--allow-enlarge",
        "--out",
        s(&big),
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(dims(&big), (80, 40));
}

#[test]
fn batch_reports_every_file_and_skips_other_formats() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    picture(&src.join("a.png"), 40, 20);
    picture(&src.join("b.jpg"), 60, 30);
    image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
        .save(src.join("c.bmp"))
        .unwrap();
    std::fs::write(src.join("bad.png"), b"not an image").unwrap();
    let out_dir = dir.path().join("out");
    let output = run(&[
        s(&src),
        "--max-side",
        "10",
        "--out-dir",
        s(&out_dir),
        "--json",
    ]);
    // The unreadable file fails the run, the others are still done.
    assert_eq!(output.status.code(), Some(1));
    let v = json(&output);
    let files = v["files"].as_array().unwrap();
    let status = |name: &str| {
        files
            .iter()
            .find(|f| f["input"].as_str().unwrap().ends_with(name))
            .map(|f| f["status"].as_str().unwrap().to_owned())
    };
    assert_eq!(status("a.png").as_deref(), Some("resized"));
    assert_eq!(status("b.jpg").as_deref(), Some("resized"));
    assert_eq!(status("c.bmp").as_deref(), Some("skipped"));
    assert_eq!(v["errors"].as_array().unwrap().len(), 1);
    assert_eq!(dims(&out_dir.join("a.png")), (10, 5));
    assert_eq!(dims(&out_dir.join("b.jpg")), (10, 5));
    assert!(!out_dir.join("c.bmp").exists());
    assert_eq!(
        image::guess_format(&std::fs::read(out_dir.join("b.jpg")).unwrap()).unwrap(),
        image::ImageFormat::Jpeg
    );
}

#[test]
fn two_runs_give_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    picture(&png, 90, 50);
    let (one, two) = (dir.path().join("one.png"), dir.path().join("two.png"));
    for out in [&one, &two] {
        let output = run(&[s(&png), "--scale", "0.3", "--out", s(out)]);
        assert_eq!(output.status.code(), Some(0));
    }
    assert_eq!(std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap());
}

#[test]
fn two_inputs_for_one_output_name_are_refused_up_front() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    picture(&a.join("x.png"), 40, 20);
    picture(&b.join("x.png"), 40, 20);
    let out_dir = dir.path().join("out");
    let output = run(&[
        s(&a.join("x.png")),
        s(&b.join("x.png")),
        "--max-side",
        "10",
        "--out-dir",
        s(&out_dir),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!out_dir.exists());
}
