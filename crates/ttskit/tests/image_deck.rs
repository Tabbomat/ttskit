//! `ttskit image deck build` end to end. Needs the `image` feature.
#![cfg(feature = "image")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let mut full = vec!["image", "deck", "build"];
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

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn colour(k: u32) -> [u8; 3] {
    [(k * 3 + 1) as u8, (250 - k * 2) as u8, (k * 7 % 251) as u8]
}

fn solid(path: &Path, (w, h): (u32, u32), rgb: [u8; 3]) {
    image::RgbImage::from_pixel(w, h, image::Rgb(rgb))
        .save(path)
        .unwrap();
}

/// `n` solid 3x4 cards `card0.png`... in `dir`.
fn cards(dir: &Path, n: u32) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    (0..n)
        .map(|k| {
            let path = dir.join(format!("card{k}.png"));
            solid(&path, (3, 4), colour(k));
            path
        })
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn builds_a_sheet_and_its_description() {
    let dir = tempfile::tempdir().unwrap();
    let cards_dir = dir.path().join("cards");
    cards(&cards_dir, 3);
    let out = dir.path().join("out");
    let output = run(&[s(&cards_dir), "--out-dir", s(&out), "--name", "mydeck"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(names(&out), ["mydeck.json", "mydeck.png"]);

    let text = stdout(&output);
    assert!(text.contains("2x2 grid, 3 cards, 6x8 px"), "{text}");
    assert!(
        text.contains("3 cards on 1 sheet (card size 3x4 px)"),
        "{text}"
    );

    let sheet = image::open(out.join("mydeck.png")).unwrap().to_rgb8();
    assert_eq!(sheet.dimensions(), (6, 8));
    assert_eq!(sheet.get_pixel(0, 0).0, colour(0));
    assert_eq!(sheet.get_pixel(3, 0).0, colour(1));
    assert_eq!(sheet.get_pixel(0, 4).0, colour(2));
    assert_eq!(sheet.get_pixel(5, 7).0, [0x80; 3]);

    let raw = std::fs::read_to_string(out.join("mydeck.json")).unwrap();
    assert!(
        raw.ends_with("}\n") && raw.contains("\n  \"name\": \"mydeck\""),
        "{raw}"
    );
    let d = read_json(&out.join("mydeck.json"));
    assert_eq!(d["format_version"], 1);
    assert_eq!(d["name"], "mydeck");
    assert_eq!(
        (d["card_width"].as_u64(), d["card_height"].as_u64()),
        (Some(3), Some(4))
    );
    assert_eq!(d["back_is_hidden"], false);
    let sheet = &d["sheets"][0];
    assert_eq!(sheet["file"], "mydeck.png");
    assert_eq!(
        (sheet["columns"].as_u64(), sheet["rows"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(
        (sheet["width"].as_u64(), sheet["height"].as_u64()),
        (Some(6), Some(8))
    );
    assert_eq!(sheet["card_count"], 3);
    assert_eq!(sheet["hidden_slot"], 3);
    let card = &sheet["cards"][2];
    assert_eq!(card["index"], 2);
    assert_eq!(card["slot"], 2);
    assert_eq!(
        (card["column"].as_u64(), card["row"].as_u64()),
        (Some(0), Some(1))
    );
    let source = card["source"].as_str().unwrap();
    assert!(
        source.ends_with("cards/card2.png") && !source.contains('\\'),
        "{source}"
    );
}

#[test]
fn seventy_cards_make_two_sheets_or_one_without_the_hidden_slot() {
    let dir = tempfile::tempdir().unwrap();
    let cards_dir = dir.path().join("cards");
    cards(&cards_dir, 70);
    let out = dir.path().join("out");
    let output = run(&[s(&cards_dir), "--out-dir", s(&out)]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(names(&out), ["deck.json", "deck_1.png", "deck_2.png"]);
    let d = read_json(&out.join("deck.json"));
    assert_eq!(d["sheets"][0]["file"], "deck_1.png");
    assert_eq!(d["sheets"][0]["card_count"], 69);
    assert_eq!(d["sheets"][0]["hidden_slot"], 69);
    assert_eq!(d["sheets"][1]["card_count"], 1);
    assert_eq!(d["sheets"][1]["columns"], 2);
    assert_eq!(d["sheets"][1]["cards"][0]["index"], 69);
    assert_eq!(d["sheets"][1]["cards"][0]["slot"], 0);
    // Card 10 sorts after card 9: natural order, not 0, 1, 10, 11, ...
    assert!(
        d["sheets"][0]["cards"][10]["source"]
            .as_str()
            .unwrap()
            .ends_with("card10.png")
    );

    let out = dir.path().join("out70");
    let output = run(&[s(&cards_dir), "--out-dir", s(&out), "--no-hidden-slot"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(names(&out), ["deck.json", "deck.png"]);
    let d = read_json(&out.join("deck.json"));
    assert_eq!(d["back_is_hidden"], true);
    assert_eq!(d["sheets"][0]["card_count"], 70);
    assert!(d["sheets"][0]["hidden_slot"].is_null());
    let sheet = image::open(out.join("deck.png")).unwrap().to_rgb8();
    assert_eq!(sheet.dimensions(), (30, 28));
    // Slot 69 (column 9, row 6) holds the 70th card.
    assert_eq!(sheet.get_pixel(27, 24).0, colour(69));
}

#[test]
fn a_hidden_image_and_jpeg_output() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(&dir.path().join("cards"), 2);
    let back = dir.path().join("back.png");
    solid(&back, (3, 4), [250, 0, 0]);
    let out = dir.path().join("out");
    let output = run(&[
        s(&all[0]),
        s(&all[1]),
        "--hidden",
        s(&back),
        "--to",
        "jpeg",
        "--quality",
        "100",
        "--out-dir",
        s(&out),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(names(&out), ["deck.jpg", "deck.json"]);
    let sheet = image::open(out.join("deck.jpg")).unwrap().to_rgb8();
    let p = sheet.get_pixel(4, 5).0;
    assert!(p[0] > 200 && p[1] < 60 && p[2] < 60, "{p:?}");
}

#[test]
fn json_report_and_dry_run() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(&dir.path().join("cards"), 3);
    let out = dir.path().join("out");
    let output = run(&[
        s(&all[0]),
        s(&all[1]),
        "--out-dir",
        s(&out),
        "--dry-run",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!out.exists(), "a dry run must not create the directory");
    let report = json(&output);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["errors"].as_array().unwrap().len(), 0);
    assert_eq!(report["description"]["sheets"][0]["card_count"], 2);
    let files = report["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["kind"], "sheet");
    assert!(files[0]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(files[0]["written"], false);
    assert_eq!(files[0]["overwrote"], false);
    assert_eq!(files[1]["kind"], "description");

    // Without a destination a dry run only reports; a real run refuses.
    let output = run(&[s(&all[0]), "--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("dry run"), "{}", stdout(&output));
    let output = run(&[s(&all[0])]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--out-dir"), "{}", stderr(&output));
}

#[test]
fn output_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let cards_dir = dir.path().join("cards");
    cards(&cards_dir, 12);
    for to in ["png", "jpg"] {
        let mut runs = Vec::new();
        for n in 0..2 {
            let out = dir.path().join(format!("out_{to}_{n}"));
            let output = run(&[s(&cards_dir), "--out-dir", s(&out), "--to", to]);
            assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
            let sheet = std::fs::read(out.join(format!("deck.{to}"))).unwrap();
            let description = std::fs::read(out.join("deck.json")).unwrap();
            runs.push((sheet, description));
        }
        assert_eq!(runs[0], runs[1], "{to}");
    }
}

#[test]
fn refuses_to_overwrite_before_writing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let cards_dir = dir.path().join("cards");
    cards(&cards_dir, 70);
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("deck_2.png"), b"keep me").unwrap();

    let output = run(&[s(&cards_dir), "--out-dir", s(&out)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("deck_2.png"),
        "{}",
        stderr(&output)
    );
    assert_eq!(names(&out), ["deck_2.png"], "nothing else may be written");
    assert_eq!(std::fs::read(out.join("deck_2.png")).unwrap(), b"keep me");

    let output = run(&[s(&cards_dir), "--out-dir", s(&out), "--yes", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(json(&output)["files"][1]["overwrote"], true);
    assert_eq!(names(&out), ["deck.json", "deck_1.png", "deck_2.png"]);
    assert_ne!(std::fs::read(out.join("deck_2.png")).unwrap(), b"keep me");
}

#[test]
fn an_output_may_not_be_an_input() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let all = cards(&work, 1);
    let target = work.join("card0.png");
    assert_eq!(all[0], target);
    let before = std::fs::read(&target).unwrap();
    let output = run(&[
        s(&target),
        "--out-dir",
        s(&work),
        "--name",
        "card0",
        "--yes",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("input"), "{}", stderr(&output));
    assert_eq!(std::fs::read(&target).unwrap(), before);
}

#[test]
fn another_size_is_refused_unless_fitted() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(&dir.path().join("cards"), 2);
    let wide = dir.path().join("wide.png");
    solid(&wide, (6, 4), [9, 9, 9]);
    let out = dir.path().join("out");
    let args = [s(&all[0]), s(&all[1]), s(&wide), "--out-dir", s(&out)];

    let output = run(&args);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert!(text.contains("wide.png") && text.contains("6x4"), "{text}");
    assert!(text.contains("3x4"), "{text}");
    assert!(!out.exists(), "nothing may be written");
    let output = run(&[args.as_slice(), &["--json"]].concat());
    assert_eq!(output.status.code(), Some(1));
    let report = json(&output);
    assert!(report["description"].is_null());
    assert!(report["errors"][0].as_str().unwrap().contains("wide.png"));

    for fit in ["contain", "cover", "stretch"] {
        let out = dir.path().join(format!("out_{fit}"));
        let output = run(&[
            s(&all[0]),
            s(&all[1]),
            s(&wide),
            "--out-dir",
            s(&out),
            "--fit",
            fit,
        ]);
        assert_eq!(output.status.code(), Some(0), "{fit}: {}", stdout(&output));
        let sheet = image::open(out.join("deck.png")).unwrap().to_rgb8();
        assert_eq!(sheet.dimensions(), (6, 8), "{fit}");
        // The wide card lies in slot 2: solid colour, so cover and stretch fill the cell.
        if fit != "contain" {
            assert_eq!(sheet.get_pixel(1, 5).0, [9, 9, 9], "{fit}");
        } else {
            // A band of fill above and below the scaled card: 6x4 -> 3x2.
            assert_eq!(sheet.get_pixel(1, 4).0, [0x80; 3]);
            assert_eq!(sheet.get_pixel(1, 5).0, [9, 9, 9]);
        }
    }

    // --card-size alone does not make other sizes fit.
    let output = run(&[s(&all[0]), "--card-size", "6x8", "--dry-run"]);
    assert_eq!(output.status.code(), Some(1));
    let output = run(&[
        s(&all[0]),
        "--card-size",
        "6x8",
        "--fit",
        "stretch",
        "--dry-run",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert!(stdout(&output).contains("12x16 px"), "{}", stdout(&output));
}

#[test]
fn max_sheet_side_scales_down_with_a_note() {
    let dir = tempfile::tempdir().unwrap();
    let cards_dir = dir.path().join("cards");
    cards(&cards_dir, 69);
    let out = dir.path().join("out");
    let output = run(&[
        s(&cards_dir),
        "--out-dir",
        s(&out),
        "--max-sheet-side",
        "15",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(
        text.contains("note: ") && text.contains("from 3x4 to 1x2"),
        "{text}"
    );
    assert_eq!(
        image::image_dimensions(out.join("deck.png")).unwrap(),
        (10, 14)
    );

    let output = run(&[
        s(&cards_dir),
        "--dry-run",
        "--max-sheet-side",
        "9",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        json(&output)["errors"][0]
            .as_str()
            .unwrap()
            .contains("1 pixel")
    );
}

#[test]
fn a_list_file_adds_cards_after_the_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(&dir.path().join("cards"), 4);
    let list = dir.path().join("list.txt");
    let text =
        "\u{feff}# the deck\r\n\r\ncards/card3.png\r\ncards/card1.png\r\n# cards/card2.png\r\n";
    std::fs::write(&list, text).unwrap();
    let out = dir.path().join("out");
    let output = run(&[
        s(&all[0]),
        "--list",
        s(&list),
        "--out-dir",
        s(&out),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let d = read_json(&out.join("deck.json"));
    let sources: Vec<&str> = d["sheets"][0]["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources.len(), 3);
    assert!(sources[0].ends_with("card0.png"));
    assert!(sources[1].ends_with("cards/card3.png"));
    assert!(sources[2].ends_with("cards/card1.png"));
    let sheet = image::open(out.join("deck.png")).unwrap().to_rgb8();
    assert_eq!(sheet.get_pixel(3, 0).0, colour(3));
    assert_eq!(sheet.get_pixel(0, 4).0, colour(1));

    // The same file may be listed several times.
    let output = run(&[s(&all[0]), s(&all[0]), s(&all[0]), "--dry-run", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(json(&output)["description"]["sheets"][0]["card_count"], 3);
}

#[test]
fn explicit_grids() {
    let dir = tempfile::tempdir().unwrap();
    let cards_dir = dir.path().join("cards");
    cards(&cards_dir, 4);
    let grid = |extra: &[&str]| {
        let mut args = vec![s(&cards_dir), "--dry-run", "--json"];
        args.extend_from_slice(extra);
        run(&args)
    };
    let output = grid(&["--columns", "1"]);
    let d = json(&output);
    assert_eq!(d["description"]["sheets"][0]["columns"], 1);
    assert_eq!(d["description"]["sheets"][0]["rows"], 5);
    let output = grid(&["--rows", "1", "--no-hidden-slot"]);
    let d = json(&output);
    assert_eq!(d["description"]["sheets"][0]["columns"], 4);
    assert_eq!(d["description"]["sheets"][0]["rows"], 1);
    let output = grid(&["--columns", "11"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("1..=10"), "{}", stderr(&output));
    let output = grid(&["--rows", "8"]);
    assert!(stderr(&output).contains("1..=7"), "{}", stderr(&output));
    let output = grid(&["--columns", "1", "--rows", "1"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("hidden-card slot"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn bad_arguments_and_rejected_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(&dir.path().join("cards"), 1);
    let out = dir.path().join("out");
    let base = [s(&all[0]), "--out-dir", s(&out)];
    let with = |extra: &[&str]| run(&[base.as_slice(), extra].concat());

    let no_cards = run(&["--out-dir", s(&out)]);
    assert_eq!(no_cards.status.code(), Some(1));
    assert!(
        stderr(&no_cards).contains("no cards"),
        "{}",
        stderr(&no_cards)
    );
    for name in ["a/b", "a\\b", "..", ""] {
        let output = with(&["--name", name]);
        assert_eq!(output.status.code(), Some(1), "name {name:?}");
        assert!(stderr(&output).contains("--name"), "{}", stderr(&output));
    }
    assert!(!with(&["--quality", "90"]).status.success());
    assert!(!with(&["--card-size", "big"]).status.success());
    assert!(!with(&["--fill", "nope"]).status.success());
    let back = dir.path().join("back.png");
    solid(&back, (3, 4), [1, 1, 1]);
    let output = with(&["--hidden", s(&back), "--no-hidden-slot"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("--no-hidden-slot"),
        "{}",
        stderr(&output)
    );
    assert!(!out.exists());

    let bmp = dir.path().join("a.bmp");
    solid(&bmp, (3, 4), [1, 2, 3]);
    let output = run(&[s(&bmp), "--out-dir", s(&out)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("bmp"), "{}", stdout(&output));
    assert!(!out.exists());
}

// ---- `image deck split` ----

fn split(args: &[&str]) -> Output {
    let mut full = vec!["image", "deck", "split"];
    full.extend_from_slice(args);
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(&full)
        .output()
        .unwrap()
}

fn fixture_save() -> String {
    format!(
        "{}/../../tests/fixtures/custom_assets.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn pixels(path: &Path) -> image::RgbImage {
    image::open(path).unwrap().to_rgb8()
}

/// Builds `n` cards with `extra` options and returns the output directory
/// (sheet and description are named `mydeck`).
fn built(dir: &Path, n: u32, extra: &[&str]) -> PathBuf {
    let cards_dir = dir.join("cards");
    cards(&cards_dir, n);
    let out = dir.join("built");
    let mut args = vec![s(&cards_dir), "--out-dir", s(&out), "--name", "mydeck"];
    args.extend_from_slice(extra);
    let output = run(&args);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    out
}

#[test]
fn splitting_a_built_sheet_gives_the_cards_back() {
    let dir = tempfile::tempdir().unwrap();
    let back = dir.path().join("back.png");
    solid(&back, (3, 4), [9, 8, 7]);
    let out = built(dir.path(), 6, &["--hidden", s(&back)]);
    let cards_out = dir.path().join("cards_out");
    let output = split(&[
        s(&out.join("mydeck.png")),
        "--description",
        s(&out.join("mydeck.json")),
        "--out-dir",
        s(&cards_out),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(
        names(&cards_out),
        [
            "mydeck_00.png",
            "mydeck_01.png",
            "mydeck_02.png",
            "mydeck_03.png",
            "mydeck_04.png",
            "mydeck_05.png",
            "mydeck_hidden.png"
        ]
    );
    for k in 0..6 {
        let img = pixels(&cards_out.join(format!("mydeck_{k:02}.png")));
        assert_eq!(img, pixels(&dir.path().join(format!("cards/card{k}.png"))));
    }
    assert_eq!(pixels(&cards_out.join("mydeck_hidden.png")), pixels(&back));

    let text = stdout(&output);
    assert!(
        text.contains("mydeck_01.png: slot 1 (column 1, row 0), 3x4 px,"),
        "{text}"
    );
    assert!(text.contains("hidden-card slot"), "{text}");
    assert!(
        text.contains("7 files written from a 3x3 grid of 3x4 px cells, 0 failed"),
        "{text}"
    );
}

#[test]
fn split_json_report() {
    let dir = tempfile::tempdir().unwrap();
    let out = built(dir.path(), 3, &[]);
    let cards_out = dir.path().join("o");
    let output = split(&[
        s(&out.join("mydeck.png")),
        "--columns",
        "2",
        "--rows",
        "2",
        "--out-dir",
        s(&cards_out),
        "--name",
        "c",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let d = json(&output);
    assert_eq!(d["dry_run"], false);
    assert_eq!(d["grid_source"], "flags");
    assert_eq!(
        (d["columns"].as_u64(), d["rows"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(
        (d["cell_width"].as_u64(), d["cell_height"].as_u64()),
        (Some(3), Some(4))
    );
    assert!(d["errors"].as_array().unwrap().is_empty());
    let files = d["files"].as_array().unwrap();
    assert_eq!(files.len(), 4);
    assert_eq!(files[1]["slot"], 1);
    assert_eq!(files[1]["column"], 1);
    assert_eq!(files[1]["row"], 0);
    assert_eq!(files[1]["hidden"], false);
    assert_eq!(files[1]["written"], true);
    assert_eq!(files[1]["overwrote"], false);
    assert_eq!(files[1]["width"], 3);
    assert!(files[1]["path"].as_str().unwrap().ends_with("c_01.png"));
    assert!(files[1]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(files[3]["hidden"], true);
    assert!(files[3]["path"].as_str().unwrap().ends_with("c_hidden.png"));
}

/// Four cards on a 2x2 sheet without a hidden slot, named `plain`.
fn built_no_hidden(dir: &Path) -> PathBuf {
    let cards_dir = dir.join("plain_cards");
    cards(&cards_dir, 4);
    let out = dir.join("plain_out");
    let output = run(&[
        s(&cards_dir),
        "--out-dir",
        s(&out),
        "--name",
        "plain",
        "--no-hidden-slot",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    out
}

#[test]
fn the_description_decides_count_and_hidden_slot() {
    let dir = tempfile::tempdir().unwrap();
    // 2 cards on a fixed 3x2 grid: slots 2 to 4 are unused fill.
    let out = built(dir.path(), 2, &["--columns", "3", "--rows", "2"]);
    let sheet = out.join("mydeck.png");
    let description = out.join("mydeck.json");
    let dry = |extra: &[&str]| {
        let mut args = vec![s(&sheet), "--dry-run", "--json"];
        args.extend_from_slice(extra);
        split(&args)
    };
    let output = dry(&["--description", s(&description)]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let d = json(&output);
    assert_eq!(d["grid_source"], "description");
    assert_eq!(d["dry_run"], true);
    let files = d["files"].as_array().unwrap();
    assert_eq!(files.len(), 3, "2 cards and the hidden slot");
    assert_eq!(files[2]["slot"], 5);
    assert_eq!(files[2]["hidden"], true);
    assert_eq!(files[2]["written"], false);

    // --count overrides the card count; the flags say all slots.
    let output = dry(&["--description", s(&description), "--count", "4"]);
    assert_eq!(json(&output)["files"].as_array().unwrap().len(), 5);
    let output = dry(&["--columns", "3", "--rows", "2"]);
    assert_eq!(json(&output)["files"].as_array().unwrap().len(), 6);

    // hidden_slot: null in the description, and --no-hidden-slot over it.
    let plain = built_no_hidden(dir.path());
    let output = split(&[
        s(&plain.join("plain.png")),
        "--description",
        s(&plain.join("plain.json")),
        "--dry-run",
        "--json",
    ]);
    let files = json(&output)["files"].as_array().unwrap().clone();
    assert_eq!(files.len(), 4);
    assert!(files.iter().all(|f| f["hidden"] == false));
    let output = dry(&["--description", s(&description), "--no-hidden-slot"]);
    let files = json(&output)["files"].as_array().unwrap().clone();
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f["hidden"] == false));
}

#[test]
fn a_bad_description_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = built(dir.path(), 3, &[]);
    let description = out.join("mydeck.json");
    let sheet = out.join("mydeck.png");
    let copy = dir.path().join("Other.png");
    std::fs::copy(&sheet, &copy).unwrap();
    let output = split(&[s(&copy), "--description", s(&description), "--dry-run"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains("Other.png") && text.contains("it lists: mydeck.png"),
        "{text}"
    );

    // A newer format version and a file that is not a description.
    let newer = std::fs::read_to_string(&description)
        .unwrap()
        .replace("\"format_version\": 1", "\"format_version\": 2");
    let newer_path = dir.path().join("newer.json");
    std::fs::write(&newer_path, newer).unwrap();
    let output = split(&[s(&sheet), "--description", s(&newer_path), "--dry-run"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("format_version 2"),
        "{}",
        stderr(&output)
    );
    let text_file = dir.path().join("notes.json");
    std::fs::write(&text_file, "not json").unwrap();
    let output = split(&[s(&sheet), "--description", s(&text_file), "--dry-run"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("not valid JSON"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_grid_can_come_from_a_save() {
    let dir = tempfile::tempdir().unwrap();
    // The fixture's deck "1" is a 10x7 sheet with BackIsHidden true.
    let sheet = dir.path().join("sheet.png");
    solid(&sheet, (30, 28), [4, 5, 6]);
    let save = fixture_save();
    let plan = |id: &str| {
        split(&[
            s(&sheet),
            "--save",
            &save,
            "--deck-id",
            id,
            "--dry-run",
            "--json",
        ])
    };
    let output = plan("1");
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let d = json(&output);
    assert_eq!(d["grid_source"], "save");
    let files = d["files"].as_array().unwrap();
    assert_eq!(files.len(), 70, "BackIsHidden true: all slots are cards");
    assert!(files.iter().all(|f| f["hidden"] == false));
    assert!(
        files[69]["path"]
            .as_str()
            .unwrap()
            .ends_with("sheet_69.png")
    );

    let output = plan("2");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("ids are: 1"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn counting_names_and_the_hidden_slot() {
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("big.png");
    solid(&sheet, (30, 28), [4, 5, 6]);
    let out = dir.path().join("o");
    let base = [
        s(&sheet),
        "--columns",
        "10",
        "--rows",
        "7",
        "--out-dir",
        s(&out),
    ];
    let with = |extra: &[&str]| split(&[base.as_slice(), extra].concat());

    let output = with(&["--dry-run", "--json"]);
    let d = json(&output);
    let files = d["files"].as_array().unwrap();
    assert_eq!(files.len(), 70);
    let path = |i: usize| files[i]["path"].as_str().unwrap().to_owned();
    assert!(path(0).ends_with("big_00.png"));
    assert!(path(68).ends_with("big_68.png"));
    assert!(path(69).ends_with("big_hidden.png"));
    assert!(!out.exists());

    let output = with(&["--count", "3", "--skip-hidden", "--dry-run", "--json"]);
    assert_eq!(json(&output)["files"].as_array().unwrap().len(), 3);
    let output = with(&["--count", "70"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("69 card slots"),
        "{}",
        stderr(&output)
    );
    assert!(!out.exists());
    let output = with(&["--count", "70", "--no-hidden-slot", "--dry-run", "--json"]);
    assert_eq!(json(&output)["files"].as_array().unwrap().len(), 70);

    let output = with(&["--count", "2"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(names(&out), ["big_00.png", "big_01.png", "big_hidden.png"]);
}

#[test]
fn an_uneven_sheet_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("odd.png");
    solid(&sheet, (7, 5), [4, 5, 6]);
    let out = dir.path().join("o");
    let output = split(&[
        s(&sheet),
        "--columns",
        "2",
        "--rows",
        "2",
        "--out-dir",
        s(&out),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert!(
        text.contains("7x5") && text.contains("2 columns x 2 rows"),
        "{text}"
    );
    assert!(text.contains("sheet not split, 1 failed"), "{text}");
    assert!(!out.exists());
}

#[test]
fn existing_files_are_not_overwritten_without_yes() {
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("s.png");
    solid(&sheet, (6, 8), [4, 5, 6]);
    let out = dir.path().join("o");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("s_02.png"), b"old").unwrap();
    let args = [
        s(&sheet),
        "--columns",
        "2",
        "--rows",
        "2",
        "--out-dir",
        s(&out),
    ];
    let output = split(&args);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"), "{}", stderr(&output));
    assert!(stderr(&output).contains("s_02.png"), "{}", stderr(&output));
    assert_eq!(names(&out), ["s_02.png"], "nothing else was written");
    assert_eq!(std::fs::read(out.join("s_02.png")).unwrap(), b"old");

    let output = split(&[args.as_slice(), &["--yes"]].concat());
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("overwrites"),
        "{}",
        stdout(&output)
    );
    assert_eq!(names(&out).len(), 4);
    assert_ne!(std::fs::read(out.join("s_02.png")).unwrap(), b"old");
}

#[test]
fn the_sheet_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("s_00.png");
    solid(&sheet, (6, 8), [4, 5, 6]);
    let output = split(&[
        s(&sheet),
        "--columns",
        "2",
        "--rows",
        "2",
        "--out-dir",
        s(dir.path()),
        "--name",
        "s",
        "--yes",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("would overwrite an input"),
        "{}",
        stderr(&output)
    );
    assert_eq!(names(dir.path()), ["s_00.png"]);
}

#[test]
fn jpeg_output_is_deterministic_and_flattens_transparency() {
    let dir = tempfile::tempdir().unwrap();
    let sheet = dir.path().join("a.png");
    image::RgbaImage::from_fn(6, 4, |x, _| {
        if x < 3 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    })
    .save(&sheet)
    .unwrap();
    let run_to = |out: &Path| {
        split(&[
            s(&sheet),
            "--columns",
            "2",
            "--rows",
            "1",
            "--no-hidden-slot",
            "--to",
            "jpg",
            "--quality",
            "90",
            "--fill",
            "white",
            "--out-dir",
            s(out),
        ])
    };
    let (one, two) = (dir.path().join("one"), dir.path().join("two"));
    let output = run_to(&one);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("12 transparent or translucent pixels flattened onto #ffffff"),
        "{}",
        stdout(&output)
    );
    assert_eq!(run_to(&two).status.code(), Some(0));
    assert_eq!(names(&one), ["a_00.jpg", "a_01.jpg"]);
    for name in names(&one) {
        assert_eq!(
            std::fs::read(one.join(&name)).unwrap(),
            std::fs::read(two.join(&name)).unwrap()
        );
    }
    let flat = pixels(&one.join("a_01.jpg"));
    assert!(flat.pixels().all(|p| p.0.iter().all(|&c| c > 250)));
}

#[test]
fn split_arguments_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let sheet_path = dir.path().join("s.png");
    solid(&sheet_path, (6, 8), [4, 5, 6]);
    let out = dir.path().join("o");
    let sheet = s(&sheet_path);
    let save = fixture_save();
    let usage = |args: &[&str]| {
        let output = split(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    };
    let dir_arg = ["--out-dir", s(&out)];
    usage(&[sheet, dir_arg[0], dir_arg[1]]);
    usage(&[sheet, "--columns", "2", dir_arg[0], dir_arg[1]]);
    usage(&[sheet, "--rows", "2", dir_arg[0], dir_arg[1]]);
    usage(&[sheet, "--columns", "11", "--rows", "2"]);
    usage(&[sheet, "--columns", "2", "--rows", "8"]);
    usage(&[
        sheet,
        "--columns",
        "2",
        "--rows",
        "2",
        "--description",
        "d.json",
    ]);
    usage(&[
        sheet,
        "--columns",
        "2",
        "--rows",
        "2",
        "--save",
        &save,
        "--deck-id",
        "1",
    ]);
    usage(&[
        sheet,
        "--description",
        "d.json",
        "--save",
        &save,
        "--deck-id",
        "1",
    ]);
    usage(&[sheet, "--save", &save]);
    usage(&[sheet, "--deck-id", "1", "--columns", "2", "--rows", "2"]);
    usage(&[sheet, "--description", "d.json", "--deck-id", "1"]);
    usage(&[sheet, "--columns", "2", "--rows", "2", "--count", "0"]);
    usage(&[
        sheet,
        "--columns",
        "2",
        "--rows",
        "2",
        "--no-hidden-slot",
        "--skip-hidden",
    ]);

    let flags = [sheet, "--columns", "2", "--rows", "2"];
    let fails = |extra: &[&str], needle: &str| {
        let output = split(&[flags.as_slice(), extra].concat());
        assert_eq!(
            output.status.code(),
            Some(1),
            "{extra:?}: {}",
            stdout(&output)
        );
        assert!(
            stderr(&output).contains(needle),
            "{extra:?}: {}",
            stderr(&output)
        );
    };
    fails(&[], "--out-dir");
    fails(&["--out-dir", s(&out), "--quality", "90"], "--quality");
    fails(&["--out-dir", s(&out), "--fill", "nope"], "nope");
    fails(&["--out-dir", s(&out), "--name", "a/b"], "--name");
    fails(&["--out-dir", s(&out), "--count", "4"], "3 card slots");
    assert!(!out.exists());

    let bmp = dir.path().join("a.bmp");
    solid(&bmp, (6, 8), [1, 2, 3]);
    let output = split(&[
        s(&bmp),
        "--columns",
        "2",
        "--rows",
        "2",
        "--out-dir",
        s(&out),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("bmp"), "{}", stdout(&output));
    assert!(!out.exists());
    let missing = dir.path().join("none.png");
    let output = split(&[s(&missing), "--columns", "2", "--rows", "2", "--dry-run"]);
    assert_eq!(output.status.code(), Some(1));
}
