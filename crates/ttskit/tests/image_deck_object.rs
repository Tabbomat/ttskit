//! `ttskit image deck object` end to end. Needs the `image` feature.
#![cfg(feature = "image")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ttskit(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

const BACK: &str = "https://example.invalid/back/";

fn url(name: &str) -> String {
    format!("https://example.invalid/{name}/")
}

/// A project directory with `n` cards built into `assets/` (grid 2x2, so
/// three cards fit on a sheet) and a manifest that has every sheet.
struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Project {
    fn new(n: u32) -> Project {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(
            root.join("ttskit.toml"),
            "[cloud]\nfolder = \"Test\"\n[local]\ndir = \"assets\"\n",
        )
        .unwrap();
        let cards = root.join("cards");
        std::fs::create_dir_all(&cards).unwrap();
        for k in 0..n {
            image::RgbImage::from_pixel(3, 4, image::Rgb([k as u8 * 9, 20, 30]))
                .save(cards.join(format!("card{k:02}.png")))
                .unwrap();
        }
        let built = ttskit(
            &[
                "image",
                "deck",
                "build",
                "cards",
                "--out-dir",
                "assets",
                "--name",
                "deck",
                "--columns",
                "2",
                "--rows",
                "2",
            ],
            &root,
        );
        assert!(built.status.success(), "{}", stderr(&built));
        let project = Project { _dir: dir, root };
        project.write_manifest(&project.sheets());
        project
    }

    fn sheets(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.root.join("assets"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".png"))
            .collect();
        names.sort();
        names
    }

    /// A manifest with an entry for each of `sheets`, URL `https://.../NAME/`.
    fn write_manifest(&self, sheets: &[String]) {
        let entries: Vec<serde_json::Value> = sheets
            .iter()
            .map(|name| {
                let data = std::fs::read(self.root.join("assets").join(name)).unwrap();
                serde_json::json!({
                    "path": name,
                    "cloud_path": format!("Test/{name}"),
                    "sha1": ttskit_cloud::sha1_hex(&data),
                    "size": data.len(),
                    "url": url(name),
                })
            })
            .collect();
        let manifest = serde_json::json!({ "version": 1, "entries": entries });
        std::fs::write(
            self.root.join("ttskit.manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn object(&self, extra: &[&str]) -> Output {
        let mut args = vec!["image", "deck", "object", "assets/deck.json"];
        args.extend_from_slice(extra);
        ttskit(&args, &self.root)
    }
}

#[test]
fn manifest_lookup_for_one_sheet() {
    let p = Project::new(3);
    let o = p.object(&["--out", "deck_object.json", "--back-url", BACK]);
    assert!(o.status.success(), "{}{}", stdout(&o), stderr(&o));
    let v = read_json(&p.root.join("deck_object.json"));
    let deck = &v["ObjectStates"][0];
    assert_eq!(deck["Name"], "DeckCustom");
    assert_eq!(deck["DeckIDs"], serde_json::json!([100, 101, 102]));
    assert_eq!(deck["CustomDeck"]["1"]["FaceURL"], url("deck.png"));
    assert_eq!(deck["CustomDeck"]["1"]["BackURL"], BACK);
    assert_eq!(deck["CustomDeck"]["1"]["BackIsHidden"], false);
    assert_eq!(deck["CustomDeck"]["1"]["NumWidth"], 2);
    assert_eq!(deck["ContainedObjects"].as_array().unwrap().len(), 3);
    assert_eq!(deck["ContainedObjects"][0]["Nickname"], "");
}

#[test]
fn output_is_stable_and_in_the_game_format() {
    let p = Project::new(3);
    let run = |name: &str| {
        let o = p.object(&["--out", name, "--back-url", BACK]);
        assert!(o.status.success(), "{}", stderr(&o));
        std::fs::read(p.root.join(name)).unwrap()
    };
    let (a, b) = (run("a.json"), run("b.json"));
    assert_eq!(a, b);
    let text = String::from_utf8(a).unwrap();
    assert!(text.starts_with("{\r\n  \"ObjectStates\": [\r\n"), "{text}");
    assert!(!text.ends_with('\n'));
}

#[test]
fn several_sheets() {
    let p = Project::new(7);
    assert_eq!(p.sheets(), ["deck_1.png", "deck_2.png", "deck_3.png"]);
    let o = p.object(&["--out", "o.json", "--back-url", BACK]);
    assert!(o.status.success(), "{}", stderr(&o));
    let deck = read_json(&p.root.join("o.json"))["ObjectStates"][0].clone();
    assert_eq!(
        deck["DeckIDs"],
        serde_json::json!([100, 101, 102, 200, 201, 202, 300])
    );
    let custom = deck["CustomDeck"].as_object().unwrap();
    assert_eq!(custom.len(), 3);
    assert_eq!(custom["2"]["FaceURL"], url("deck_2.png"));
    assert_eq!(custom["3"]["FaceURL"], url("deck_3.png"));
}

#[test]
fn unique_backs_take_one_back_per_sheet() {
    let p = Project::new(4);
    let ok = p.object(&[
        "--out",
        "o.json",
        "--unique-backs",
        "--back-url",
        "https://example.invalid/b1/",
        "--back-url",
        "https://example.invalid/b2/",
    ]);
    assert!(ok.status.success(), "{}", stderr(&ok));
    let deck = read_json(&p.root.join("o.json"))["ObjectStates"][0].clone();
    assert_eq!(
        deck["CustomDeck"]["2"]["BackURL"],
        "https://example.invalid/b2/"
    );
    assert_eq!(deck["CustomDeck"]["2"]["UniqueBack"], true);

    let bad = p.object(&["--out", "o2.json", "--unique-backs", "--back-url", BACK]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("one per sheet"), "{}", stderr(&bad));
    assert!(!p.root.join("o2.json").exists());
}

#[test]
fn a_single_card_gives_a_card_object() {
    let p = Project::new(1);
    let o = p.object(&["--out", "o.json", "--back-url", BACK]);
    assert!(o.status.success(), "{}", stderr(&o));
    let card = read_json(&p.root.join("o.json"))["ObjectStates"][0].clone();
    assert_eq!(card["Name"], "CardCustom");
    assert_eq!(card["CardID"], 100);
    assert_eq!(card["CustomDeck"]["1"]["FaceURL"], url("deck.png"));
    assert!(stdout(&o).contains("card with 1 card"), "{}", stdout(&o));
}

#[test]
fn urls_and_paths_can_be_given_directly() {
    let p = Project::new(3);
    // Direct URLs need no manifest and no config.
    std::fs::remove_file(p.root.join("ttskit.toml")).unwrap();
    let o = p.object(&[
        "--out",
        "o.json",
        "--face-url",
        "https://example.invalid/own-face/",
        "--back-url",
        BACK,
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let deck = read_json(&p.root.join("o.json"))["ObjectStates"][0].clone();
    assert_eq!(
        deck["CustomDeck"]["1"]["FaceURL"],
        "https://example.invalid/own-face/"
    );
    // Without a config the lookup is refused, and says why.
    let o = p.object(&["--out", "o2.json", "--back-url", BACK]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("manifest"), "{}", stderr(&o));
}

#[test]
fn a_back_image_is_looked_up_by_local_path() {
    let p = Project::new(3);
    std::fs::write(p.root.join("assets").join("back.png"), b"not really a png").unwrap();
    let mut sheets = p.sheets();
    sheets.push("back.png".to_owned());
    p.write_manifest(&sheets);
    let o = p.object(&["--out", "o.json", "--back-url", "assets/back.png"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let deck = read_json(&p.root.join("o.json"))["ObjectStates"][0].clone();
    assert_eq!(deck["CustomDeck"]["1"]["BackURL"], url("back.png"));
}

#[test]
fn a_sheet_that_is_not_uploaded_or_changed_is_refused() {
    let p = Project::new(3);
    // Changed after the upload.
    let sheet = p.root.join("assets").join("deck.png");
    let mut data = std::fs::read(&sheet).unwrap();
    data.push(0);
    std::fs::write(&sheet, data).unwrap();
    let o = p.object(&["--out", "o.json", "--back-url", BACK]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("changed since"), "{}", stderr(&o));
    // Not in the manifest at all.
    p.write_manifest(&[]);
    let o = p.object(&["--out", "o.json", "--back-url", BACK]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("not in the manifest"), "{}", stderr(&o));
    assert!(!p.root.join("o.json").exists());
}

#[test]
fn needs_a_back_and_an_output_and_keeps_an_existing_file() {
    let p = Project::new(3);
    let o = p.object(&["--out", "o.json"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("--back-url"), "{}", stderr(&o));
    let o = p.object(&["--back-url", BACK]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("--out"), "{}", stderr(&o));

    std::fs::write(p.root.join("o.json"), "keep").unwrap();
    let o = p.object(&["--out", "o.json", "--back-url", BACK]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("--yes"), "{}", stderr(&o));
    assert_eq!(
        std::fs::read_to_string(p.root.join("o.json")).unwrap(),
        "keep"
    );
    let o = p.object(&["--out", "o.json", "--back-url", BACK, "--yes"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        std::fs::read_to_string(p.root.join("o.json"))
            .unwrap()
            .contains("DeckCustom")
    );
}

#[test]
fn dry_run_writes_nothing_and_json_reports() {
    let p = Project::new(3);
    let o = p.object(&["--out", "o.json", "--back-url", BACK, "--dry-run", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!p.root.join("o.json").exists());
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r["object"], "deck");
    assert_eq!(r["cards"], 3);
    assert_eq!(r["file"]["written"], false);
    assert_eq!(r["face_urls"][0], url("deck.png"));
}

#[test]
fn nicknames_from_file_names() {
    let p = Project::new(3);
    let o = p.object(&[
        "--out",
        "o.json",
        "--back-url",
        BACK,
        "--nicknames-from-files",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let deck = read_json(&p.root.join("o.json"))["ObjectStates"][0].clone();
    assert_eq!(deck["ContainedObjects"][0]["Nickname"], "card00");
    assert_eq!(deck["ContainedObjects"][2]["Nickname"], "card02");
}

#[test]
fn meta_file_sets_text_of_deck_and_cards() {
    let p = Project::new(3);
    std::fs::write(
        p.root.join("meta.json"),
        r#"{"deck": {"nickname": "My deck", "description": "About", "gm_notes": "Secret"},
            "cards": {"card01.png": {"nickname": "Second", "gm_notes": "n"}}}"#,
    )
    .unwrap();
    let o = p.object(&[
        "--out",
        "o.json",
        "--back-url",
        BACK,
        "--meta",
        "meta.json",
        "--nicknames-from-files",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let deck = read_json(&p.root.join("o.json"))["ObjectStates"][0].clone();
    assert_eq!(deck["Nickname"], "My deck");
    assert_eq!(deck["Description"], "About");
    assert_eq!(deck["GMNotes"], "Secret");
    let cards = &deck["ContainedObjects"];
    // The metadata wins over the file name; other cards get the file name.
    assert_eq!(cards[1]["Nickname"], "Second");
    assert_eq!(cards[1]["GMNotes"], "n");
    assert_eq!(cards[0]["Nickname"], "card00");

    std::fs::write(p.root.join("bad.json"), r#"{"cards": {"zzz.png": {}}}"#).unwrap();
    let o = p.object(&["--out", "o2.json", "--back-url", BACK, "--meta", "bad.json"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("bad.json"), "{}", stderr(&o));
    assert!(!p.root.join("o2.json").exists());
}
