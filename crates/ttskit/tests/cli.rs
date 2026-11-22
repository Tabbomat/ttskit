use std::process::Command;

fn ttskit(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn help_mentions_cloud() {
    let output = ttskit(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("cloud"));
}

#[test]
fn no_arguments_shows_help_and_fails() {
    let output = ttskit(&[]);
    assert_ne!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage"));
}

#[test]
fn cloud_list_help_mentions_folder() {
    let output = ttskit(&["cloud", "list", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("--folder"));
}

#[cfg(not(feature = "steam"))]
#[test]
fn cloud_list_without_steam_feature_fails() {
    let output = ttskit(&["cloud", "list"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--features steam"));
}

fn ttskit_in(args: &[&str], env_dir: Option<&std::path::Path>) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ttskit"));
    cmd.args(args).env_remove("TTSKIT_BACKUP_DIR");
    if let Some(dir) = env_dir {
        cmd.env("TTSKIT_BACKUP_DIR", dir);
    }
    cmd.output().unwrap()
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn backup_dir_with_one_backup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let backup = dir.path().join("20260101T000000Z");
    std::fs::create_dir(&backup).unwrap();
    std::fs::write(backup.join("CloudInfo.bson"), b"abc").unwrap();
    dir
}

#[test]
fn cloud_restore_help_mentions_yes_and_backup_dir() {
    let output = ttskit(&["cloud", "restore", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(text.contains("--yes"));
    assert!(text.contains("--backup-dir"));
}

#[test]
fn cloud_restore_without_backups_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let output = ttskit_in(&["cloud", "restore", "--backup-dir", path], None);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("No backups in"));
}

#[test]
fn cloud_restore_lists_backups() {
    let dir = backup_dir_with_one_backup();
    let path = dir.path().to_str().unwrap();
    let output = ttskit_in(&["cloud", "restore", "--backup-dir", path], None);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(text.contains("Backups in"));
    assert!(text.contains("20260101T000000Z  CloudInfo.bson 3 bytes, CloudFolder.bson absent"));
}

#[test]
fn cloud_restore_honours_the_backup_dir_env_var() {
    let dir = backup_dir_with_one_backup();
    let output = ttskit_in(&["cloud", "restore"], Some(dir.path()));
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("20260101T000000Z"));
}

#[test]
fn cloud_restore_rejects_an_invalid_name_before_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let output = ttskit_in(&["cloud", "restore", "nope", "--backup-dir", path], None);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("invalid backup name"), "{text}");
    assert!(!text.contains("--features steam"));
}

#[cfg(not(feature = "steam"))]
#[test]
fn cloud_restore_of_an_existing_backup_needs_steam() {
    let dir = backup_dir_with_one_backup();
    let path = dir.path().to_str().unwrap();
    let output = ttskit_in(
        &["cloud", "restore", "20260101T000000Z", "--backup-dir", path],
        None,
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--features steam"));
}

const CONFIG: &str = "[cloud]\nfolder = \"Game\"\n\n[local]\ndir = \"src\"\n";
const SHA1: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

/// A project directory with a config and a hand-written manifest.
fn project() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ttskit.toml"), CONFIG).unwrap();
    let manifest = format!(
        "{{\n  \"version\": 1,\n  \"entries\": [\n    {{\n      \"path\": \"a.png\",\n      \"cloud_path\": \"Game/a.png\",\n      \"sha1\": \"{SHA1}\",\n      \"size\": 3,\n      \"url\": \"https://example.invalid/ugc/1/{SHA1}/\"\n    }}\n  ]\n}}\n"
    );
    std::fs::write(dir.path().join("ttskit.manifest.json"), &manifest).unwrap();
    let config = dir.path().join("ttskit.toml").to_str().unwrap().to_owned();
    (dir, config)
}

#[test]
fn cloud_manifest_prints_the_file_without_steam() {
    let (dir, config) = project();
    let output = ttskit(&["cloud", "manifest", "--config", &config]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let file = std::fs::read_to_string(dir.path().join("ttskit.manifest.json")).unwrap();
    assert_eq!(stdout(&output), file);
    assert_eq!(stderr(&output), "");

    let output = ttskit(&["cloud", "manifest", "--json", "--config", &config]);
    assert_eq!(stdout(&output), file);
}

#[test]
fn cloud_manifest_tsv() {
    let (_dir, config) = project();
    let output = ttskit(&["cloud", "manifest", "--tsv", "--config", &config]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("a.png\thttps://example.invalid/ugc/1/{SHA1}/\n")
    );
}

#[test]
fn cloud_manifest_without_a_manifest_file_is_empty() {
    let (dir, config) = project();
    std::fs::remove_file(dir.path().join("ttskit.manifest.json")).unwrap();
    let output = ttskit(&["cloud", "manifest", "--tsv", "--config", &config]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "");
    let output = ttskit(&["cloud", "manifest", "--config", &config]);
    assert!(stdout(&output).contains("\"entries\": []"));
}

#[test]
fn cloud_manifest_rejects_tsv_with_json() {
    let (_dir, config) = project();
    let output = ttskit(&["cloud", "manifest", "--tsv", "--json", "--config", &config]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("cannot be used with"));
}

#[test]
fn cloud_manifest_with_a_missing_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ttskit.toml");
    let output = ttskit(&["cloud", "manifest", "--config", config.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).starts_with("error: "));
    assert_eq!(stdout(&output), "");
}

#[test]
fn cloud_import_tsv_help_mentions_the_flags() {
    let output = ttskit(&["cloud", "import-tsv", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for flag in ["--config", "--dry-run", "--yes", "--json"] {
        assert!(text.contains(flag), "{flag}");
    }
}

#[test]
fn cloud_import_tsv_checks_local_input_before_connecting() {
    let (dir, config) = project();
    let missing = dir.path().join("nope.tsv");
    let output = ttskit(&[
        "cloud",
        "import-tsv",
        missing.to_str().unwrap(),
        "--config",
        &config,
    ]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("cannot read"), "{text}");
    assert!(!text.contains("--features steam"));
}

fn fixture_path() -> String {
    format!(
        "{}/../../tests/fixtures/custom_assets.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn cloud_scan_prints_json_without_steam() {
    let output = ttskit(&["cloud", "scan", &fixture_path(), "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert!(value["total"].as_u64().unwrap() > 0);
    for host in ["imgur", "steamusercontent", "other"] {
        assert!(value["hosts"][host].is_array(), "{host}");
    }
    assert_eq!(
        value["hosts"]["imgur"][0]["url"],
        "https://i.imgur.com/AAAAAAA.png"
    );
}

#[test]
fn cloud_scan_prints_text() {
    let output = ttskit(&["cloud", "scan", &fixture_path()]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("imgur ("));
}

#[test]
fn cloud_scan_fails_on_missing_or_invalid_files() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.json");
    let output = ttskit(&["cloud", "scan", missing.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).starts_with("error: "));

    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{").unwrap();
    let output = ttskit(&["cloud", "scan", bad.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.starts_with("error: "), "{text}");
    assert!(text.contains("not a valid save"), "{text}");
}

#[test]
fn cloud_manifest_rejects_an_invalid_ignore_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ttskit.toml");
    std::fs::write(&config, format!("{CONFIG}ignore = [\"{{a\"]\n")).unwrap();
    let output = ttskit(&["cloud", "manifest", "--config", config.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).starts_with("error: "));
    assert_eq!(stdout(&output), "");
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn save_unpack_writes_the_tree_and_prints_a_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("MySave");
    let save = fixture("layout_edge_cases.json");
    let output = ttskit(&[
        "save",
        "unpack",
        save.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    let first = text.lines().next().unwrap();
    assert!(first.starts_with("Unpacked 23 objects into "), "{text}");
    assert!(first.contains(&out.display().to_string()), "{text}");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("warning: ")).count(),
        2,
        "{text}"
    );
    assert!(text.contains("objects/Mixed script.mix001/script.lua"));
    assert!(out.join("save.json").is_file());
    assert!(out.join(".ttskit").join("unpack.json").is_file());
    assert!(
        out.join("objects")
            .join("Board.f00d01")
            .join("script.lua")
            .is_file()
    );
}

#[test]
fn save_unpack_json_has_the_documented_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("MySave");
    let save = fixture("layout_edge_cases.json");
    let output = ttskit(&[
        "save",
        "unpack",
        save.to_str().unwrap(),
        "--output",
        out.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "output",
            "objects",
            "files",
            "mixed_eol",
            "assets_mapped",
            "dry_run",
            "create",
            "change",
            "remove",
            "unchanged"
        ]
    );
    assert_eq!(value["output"], out.display().to_string());
    assert_eq!(value["objects"], 23);
    assert_eq!(
        value["mixed_eol"],
        serde_json::json!([
            "objects/Mixed script.mix001/script.lua",
            "objects/Tie script.tie001/script.lua"
        ])
    );
    let written = walk_count(&out);
    assert_eq!(value["files"], written);
}

fn walk_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            let path = e.path();
            if path.is_dir() { walk_count(&path) } else { 1 }
        })
        .sum()
}

#[test]
fn save_unpack_refuses_a_non_empty_output_dir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("keep.txt"), "mine").unwrap();
    let save = fixture("containers.json");
    let output = ttskit(&[
        "save",
        "unpack",
        save.to_str().unwrap(),
        "-o",
        tmp.path().to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("not empty"), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    let names: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
    assert_eq!(names.len(), 1);
}

#[test]
fn save_unpack_reports_unreadable_and_invalid_saves() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let missing = tmp.path().join("missing.json");
    let output = ttskit(&[
        "save",
        "unpack",
        missing.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("cannot read"));

    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, "[1]").unwrap();
    let output = ttskit(&[
        "save",
        "unpack",
        bad.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("JSON object"),
        "{}",
        stderr(&output)
    );
    assert!(!out.exists());
}

#[test]
fn save_unpack_requires_output() {
    let save = fixture("containers.json");
    let output = ttskit(&["save", "unpack", save.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("--output"), "{}", stderr(&output));
}

#[test]
fn save_pack_round_trips_and_refuses_to_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("MySave");
    let input = fixture("containers.json");
    let unpack = ttskit(&[
        "save",
        "unpack",
        input.to_str().unwrap(),
        "-o",
        tree.to_str().unwrap(),
    ]);
    assert_eq!(unpack.status.code(), Some(0), "{}", stderr(&unpack));

    let out = tmp.path().join("sub").join("packed.json");
    let args = [
        "save",
        "pack",
        tree.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--keep-eol",
    ];
    let output = ttskit(&args);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).starts_with("Packed "),
        "{}",
        stdout(&output)
    );
    assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&input).unwrap());

    // A second run must not touch the existing file.
    std::fs::write(&out, "mine").unwrap();
    let output = ttskit(&args);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("already exists"),
        "{}",
        stderr(&output)
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"mine");
}

#[test]
fn save_pack_reports_errors_and_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("MySave");
    let input = fixture("containers.json");
    ttskit(&[
        "save",
        "unpack",
        input.to_str().unwrap(),
        "-o",
        tree.to_str().unwrap(),
    ]);
    std::fs::write(tree.join("stray.txt"), "x").unwrap();
    let out = tmp.path().join("packed.json");
    let output = ttskit(&[
        "save",
        "pack",
        tree.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).contains("warning: ignored unknown file stray.txt"));
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(
        value["warnings"],
        serde_json::json!(["ignored unknown file stray.txt"])
    );
    assert_eq!(value["bytes"], std::fs::metadata(&out).unwrap().len());
    // Default output is LF only.
    assert!(!std::fs::read(&out).unwrap().contains(&b'\r'));

    std::fs::write(tree.join("save.json"), "{").unwrap();
    let out2 = tmp.path().join("other.json");
    let output = ttskit(&[
        "save",
        "pack",
        tree.to_str().unwrap(),
        "-o",
        out2.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("save.json"), "{}", stderr(&output));
    assert!(!out2.exists());
}

fn unpack_args<'a>(save: &'a str, out: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["save", "unpack", save, "-o", out];
    args.extend_from_slice(extra);
    args
}

#[test]
fn save_unpack_force_overwrites_ttskit_files_and_keeps_unknown_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("tree");
    let save = fixture("containers.json");
    let (save, outs) = (save.to_str().unwrap(), out.to_str().unwrap());
    assert_eq!(ttskit(&unpack_args(save, outs, &[])).status.code(), Some(0));
    std::fs::write(out.join("keep.txt"), "mine").unwrap();
    std::fs::write(out.join("save.json"), "edited").unwrap();

    let refused = ttskit(&unpack_args(save, outs, &[]));
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("--force"), "{}", stderr(&refused));
    assert_eq!(std::fs::read(out.join("save.json")).unwrap(), b"edited");

    let forced = ttskit(&unpack_args(save, outs, &["--force"]));
    assert_eq!(forced.status.code(), Some(0), "{}", stderr(&forced));
    let text = stdout(&forced);
    assert!(text.contains("change save.json"), "{text}");
    assert_ne!(std::fs::read(out.join("save.json")).unwrap(), b"edited");
    assert_eq!(std::fs::read(out.join("keep.txt")).unwrap(), b"mine");

    // `--yes` is an alias.
    std::fs::write(out.join("save.json"), "edited").unwrap();
    let yes = ttskit(&unpack_args(save, outs, &["--yes"]));
    assert_eq!(yes.status.code(), Some(0), "{}", stderr(&yes));
}

#[test]
fn save_unpack_dry_run_lists_the_plan_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("tree");
    let save = fixture("containers.json");
    let (save, outs) = (save.to_str().unwrap(), out.to_str().unwrap());

    // Missing directory: everything would be created, nothing appears.
    let dry = ttskit(&unpack_args(save, outs, &["--dry-run"]));
    assert_eq!(dry.status.code(), Some(0), "{}", stderr(&dry));
    let text = stdout(&dry);
    assert!(text.starts_with("Dry run: nothing was written."), "{text}");
    assert!(text.contains("create save.json"), "{text}");
    assert!(text.contains("Would unpack "), "{text}");
    assert!(!out.exists());

    // A non-empty directory is still refused without --force.
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("keep.txt"), "mine").unwrap();
    let refused = ttskit(&unpack_args(save, outs, &["--dry-run"]));
    assert_eq!(refused.status.code(), Some(1));

    // With --force the plan shows changes and removals.
    assert_eq!(
        ttskit(&unpack_args(save, outs, &["--force"])).status.code(),
        Some(0)
    );
    std::fs::write(out.join("save.json"), "edited").unwrap();
    std::fs::write(out.join("stale.lua"), "old").unwrap();
    let before = walk_count(&out);
    let other = fixture("minimal_save.json");
    let other = other.to_str().unwrap();
    let dry = ttskit(&unpack_args(
        other,
        outs,
        &["--force", "--clean", "--dry-run"],
    ));
    assert_eq!(dry.status.code(), Some(0), "{}", stderr(&dry));
    let text = stdout(&dry);
    assert!(text.contains("change save.json"), "{text}");
    assert!(text.lines().any(|l| l.starts_with("remove ")), "{text}");
    assert!(
        !text.contains("stale.lua"),
        "unknown files are never listed: {text}"
    );
    assert_eq!(walk_count(&out), before);
    assert_eq!(std::fs::read(out.join("save.json")).unwrap(), b"edited");

    let json = ttskit(&unpack_args(
        other,
        outs,
        &["--force", "--dry-run", "--json"],
    ));
    let value: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(value["dry_run"], true);
    let changed = value["change"].as_array().unwrap();
    assert!(changed.contains(&serde_json::json!("save.json")));
}

#[test]
fn save_unpack_clean_removes_only_files_ttskit_created() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("tree");
    let first = fixture("containers.json");
    let second = fixture("minimal_save.json");
    let (first, second, outs) = (
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        out.to_str().unwrap(),
    );
    assert_eq!(
        ttskit(&unpack_args(first, outs, &[])).status.code(),
        Some(0)
    );
    std::fs::write(out.join("keep.txt"), "mine").unwrap();
    std::fs::write(out.join("objects").join("keep.txt"), "mine too").unwrap();

    // --clean needs --force.
    let alone = ttskit(&unpack_args(second, outs, &["--clean"]));
    assert_eq!(alone.status.code(), Some(2));

    // Without --clean stale files stay (and, as the metadata is rewritten,
    // are forgotten: a later --clean will not touch them).
    let stale = out.join("objects").join("Outer bag.bag001");
    let plain = ttskit(&unpack_args(second, outs, &["--force"]));
    assert_eq!(plain.status.code(), Some(0), "{}", stderr(&plain));
    assert!(stale.join("object.json").exists());
    let later = ttskit(&unpack_args(second, outs, &["--force", "--clean"]));
    assert!(stdout(&later).contains("0 removed"), "{}", stdout(&later));
    assert!(stale.join("object.json").exists());

    // Start over: the previous unpack is tracked, so --clean removes its leftovers.
    std::fs::remove_dir_all(&stale).unwrap();
    assert_eq!(
        ttskit(&unpack_args(first, outs, &["--force"]))
            .status
            .code(),
        Some(0)
    );
    assert!(stale.join("object.json").exists());

    let cleaned = ttskit(&unpack_args(second, outs, &["--force", "--clean"]));
    assert_eq!(cleaned.status.code(), Some(0), "{}", stderr(&cleaned));
    let text = stdout(&cleaned);
    assert!(
        text.contains("remove objects/Outer bag.bag001/object.json"),
        "{text}"
    );
    assert!(!stale.exists(), "emptied directories are removed");
    assert!(out.join("script.lua").exists());
    assert_eq!(std::fs::read(out.join("keep.txt")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read(out.join("objects").join("keep.txt")).unwrap(),
        b"mine too"
    );
}

#[test]
fn save_unpack_help_documents_the_safety_flags() {
    let output = ttskit(&["save", "unpack", "--help"]);
    let text = stdout(&output);
    for needle in ["--force", "--clean", "--dry-run", "unpack.json"] {
        assert!(text.contains(needle), "{needle} missing: {text}");
    }
}

fn unpacked_tree(tmp: &std::path::Path) -> (String, String) {
    let tree = tmp.join("tree");
    let input = fixture("containers.json");
    let unpack = ttskit(&[
        "save",
        "unpack",
        input.to_str().unwrap(),
        "-o",
        tree.to_str().unwrap(),
    ]);
    assert_eq!(unpack.status.code(), Some(0), "{}", stderr(&unpack));
    let out = tmp.join("packed.json");
    (
        tree.to_str().unwrap().to_owned(),
        out.to_str().unwrap().to_owned(),
    )
}

#[test]
fn save_pack_force_overwrites_an_existing_save() {
    let tmp = tempfile::tempdir().unwrap();
    let (tree, out) = unpacked_tree(tmp.path());
    std::fs::write(&out, "old").unwrap();

    let refused = ttskit(&["save", "pack", &tree, "-o", &out]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("--force"), "{}", stderr(&refused));
    assert_eq!(std::fs::read(&out).unwrap(), b"old");

    let forced = ttskit(&["save", "pack", &tree, "-o", &out, "--force"]);
    assert_eq!(forced.status.code(), Some(0), "{}", stderr(&forced));
    assert!(
        stdout(&forced).starts_with("Packed "),
        "{}",
        stdout(&forced)
    );
    assert_ne!(std::fs::read(&out).unwrap(), b"old");
    // No temporary file is left behind.
    let names: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
    assert_eq!(names.len(), 2);

    // Packing the same tree again is reported as unchanged.
    let again = ttskit(&["save", "pack", &tree, "-o", &out, "--yes"]);
    assert!(stdout(&again).contains("(unchanged)"), "{}", stdout(&again));
}

#[test]
fn save_pack_dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (tree, out) = unpacked_tree(tmp.path());

    let dry = ttskit(&["save", "pack", &tree, "-o", &out, "--dry-run"]);
    assert_eq!(dry.status.code(), Some(0), "{}", stderr(&dry));
    let text = stdout(&dry);
    assert!(text.starts_with("Dry run: nothing was written."), "{text}");
    assert!(text.contains(&format!("create {out}")), "{text}");
    assert!(text.contains("Would pack "), "{text}");
    assert!(!std::path::Path::new(&out).exists());

    // An existing file still needs --force, even for a dry run.
    std::fs::write(&out, "old").unwrap();
    let refused = ttskit(&["save", "pack", &tree, "-o", &out, "--dry-run"]);
    assert_eq!(refused.status.code(), Some(1));

    let json = ttskit(&[
        "save",
        "pack",
        &tree,
        "-o",
        &out,
        "--dry-run",
        "--force",
        "--json",
    ]);
    let value: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["action"], "change");
    assert_eq!(std::fs::read(&out).unwrap(), b"old");

    // Errors in the tree are still reported by a dry run.
    std::fs::write(std::path::Path::new(&tree).join("save.json"), "{").unwrap();
    let bad = ttskit(&["save", "pack", &tree, "-o", &out, "--dry-run", "--force"]);
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn save_pack_help_documents_the_safety_flags() {
    let text = stdout(&ttskit(&["save", "pack", "--help"]));
    for needle in ["--force", "--dry-run"] {
        assert!(text.contains(needle), "{needle} missing: {text}");
    }
}

#[test]
fn save_pack_include_bundles_modules_and_names_missing_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("tree");
    let lib = tmp.path().join("lib");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(
        tree.join("save.json"),
        r#"{"LuaScript": "file:script.lua"}"#,
    )
    .unwrap();
    std::fs::write(tree.join("script.lua"), "require(\"shared\")").unwrap();
    let out = tmp.path().join("out.json");
    let args = |extra: &[&str]| {
        let mut v = vec![
            "save",
            "pack",
            tree.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--force",
        ];
        v.extend_from_slice(extra);
        v.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let run = |extra: &[&str]| {
        let owned = args(extra);
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        ttskit(&refs)
    };

    let missing = run(&["--include", lib.to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing).contains("\"shared\""),
        "{}",
        stderr(&missing)
    );
    assert!(
        stderr(&missing).contains("script.lua"),
        "{}",
        stderr(&missing)
    );

    std::fs::write(lib.join("shared.lua"), "return 1").unwrap();
    let ok = run(&["--include", lib.to_str().unwrap()]);
    assert_eq!(ok.status.code(), Some(0), "{}", stderr(&ok));
    let packed = std::fs::read_to_string(&out).unwrap();
    assert!(packed.contains("__bundle_register"), "{packed}");
}

// ---- editor -------------------------------------------------------------

/// A port nothing listens on (bound once, then released).
fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn editor_commands_exit_3_when_the_game_is_not_reachable() {
    let game = unused_port().to_string();
    let editor = unused_port().to_string();
    let ports = ["--game-port", &game, "--editor-port", &editor];
    for args in [
        vec!["editor", "get-scripts"],
        vec!["editor", "exec", "print(1)"],
        vec!["editor", "send", "{\"a\":1}"],
    ] {
        let mut full = args.clone();
        full.extend(ports);
        let output = ttskit(&full);
        assert_eq!(output.status.code(), Some(3), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("is Tabletop Simulator running?"),
            "{args:?}"
        );
    }
}

#[test]
fn editor_usage_errors_and_lua_free_failures_keep_exit_1_or_2() {
    // A message that is not a JSON object is refused locally: plain failure.
    let output = ttskit(&["editor", "send", "[1]"]);
    assert_eq!(output.status.code(), Some(1));
    // clap usage errors keep clap's status.
    let output = ttskit(&["editor", "console", "--follow", "--quiet-ms", "5"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn exec_and_console_share_the_quiet_ms_flag() {
    for cmd in ["exec", "console"] {
        let output = ttskit(&["editor", cmd, "--help"]);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("--quiet-ms"),
            "{cmd}"
        );
    }
}

#[test]
fn console_prints_what_the_game_sends_and_exits_0() {
    use std::io::Read;
    use std::process::Stdio;
    let editor_port = unused_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args(["editor", "console", "--quiet-ms", "700", "--json"])
        .args(["--editor-port", &editor_port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let editor: std::net::SocketAddr = ([127, 0, 0, 1], editor_port).into();
    let game = rt
        .block_on(ttskit_editor::fake::FakeGame::start(editor, vec![]))
        .unwrap();
    // The child needs a moment to bind; retry until the first push lands.
    let mut pushed = false;
    for _ in 0..100 {
        let doc = serde_json::json!({"messageID": 2, "message": "from the game"});
        if rt.block_on(game.push_raw(editor, &doc)).is_ok() {
            pushed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(pushed, "console never started listening");
    let status = child.wait().unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(v["type"], "print");
    assert_eq!(v["message"], "from the game");
}

#[test]
fn watch_once_exits_3_when_the_game_is_not_reachable() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    let save = ttskit_testutil::fixture_path("scripts_ui.json");
    let out = ttskit(&[
        "save",
        "unpack",
        save.to_str().unwrap(),
        "-o",
        tree.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
    let game = unused_port().to_string();
    let editor = unused_port().to_string();
    let output = ttskit(&[
        "watch",
        tree.to_str().unwrap(),
        "--once",
        "--game-port",
        &game,
        "--editor-port",
        &editor,
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("is Tabletop Simulator running?"));
}

#[test]
fn watch_once_fails_on_a_missing_directory() {
    let output = ttskit(&["watch", "does-not-exist-dir", "--once"]);
    assert_eq!(output.status.code(), Some(1));
}

// ---- mcp ----

fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn mcp_serves_on_stdio_and_exits_zero_when_stdin_closes() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let game = closed_port().to_string();
    let editor = closed_port().to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ttskit"))
        .args([
            "mcp",
            "--game-port",
            &game,
            "--editor-port",
            &editor,
            "--timeout",
            "1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    // Read on a thread so that a hung server fails the test instead of hanging it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut send = |doc: serde_json::Value| {
        writeln!(stdin, "{doc}").unwrap();
        stdin.flush().unwrap();
    };
    let response = |id: u64| -> serde_json::Value {
        loop {
            let line = rx
                .recv_timeout(Duration::from_secs(20))
                .expect("a response line");
            let doc: serde_json::Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout must carry only JSON-RPC, got {line:?}: {e}"));
            if doc["id"] == id {
                return doc;
            }
        }
    };
    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "0"}
        }
    }));
    assert_eq!(response(1)["result"]["serverInfo"]["name"], "ttskit");
    send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    assert_eq!(response(2)["result"]["tools"].as_array().unwrap().len(), 6);
    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "get_status", "arguments": {}}
    }));
    let result = response(3)["result"].clone();
    let text = result["content"][0]["text"].as_str().unwrap();
    let status: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(status["gameReachable"], false);
    assert!(status["error"].is_string());

    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("ttskit mcp did not exit after stdin closed");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0));
}

#[test]
fn mcp_with_a_missing_root_fails() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let output = ttskit(&["mcp", "--root", missing.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("root"));
    assert!(stdout(&output).is_empty());
}

#[test]
fn mcp_fails_when_the_editor_port_is_in_use() {
    // Something accepts connections on the port, so the server must not start.
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    let output = ttskit(&["mcp", "--editor-port", &port]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("already in use"),
        "{}",
        stderr(&output)
    );
    assert!(stdout(&output).is_empty());
}

#[test]
fn mcp_console_buffer_must_be_at_least_one() {
    let output = ttskit(&["mcp", "--console-buffer", "0"]);
    assert_eq!(output.status.code(), Some(2));
}

#[cfg(not(feature = "image"))]
#[test]
fn image_info_without_image_feature_fails() {
    let output = ttskit(&["image", "info", "a.png"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--features image"));
}

#[cfg(not(feature = "image"))]
#[test]
fn image_convert_without_image_feature_fails() {
    let output = ttskit(&["image", "convert", "a.png", "--to", "png"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--features image"));
}

#[cfg(feature = "image")]
mod image_info {
    use super::*;

    fn png(path: &std::path::Path, w: u32, h: u32) {
        image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn prints_one_line_per_file_and_a_summary() {
        let dir = tempfile::tempdir().unwrap();
        png(&dir.path().join("b.png"), 4, 2);
        png(&dir.path().join("a.png"), 3, 3);
        let output = ttskit(&["image", "info", dir.path().to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(0));
        let text = stdout(&output);
        let a = text.find("a.png: png 3x3").expect(&text);
        let b = text.find("b.png: png 4x2").expect(&text);
        assert!(a < b, "sorted by path: {text}");
        assert!(text.contains("2 inspected, 0 not usable in the game, 0 unreadable"));
    }

    #[test]
    fn json_is_one_object_and_failures_set_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("a.png");
        png(&good, 2, 2);
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, b"nope").unwrap();
        let output = ttskit(&[
            "image",
            "info",
            "--json",
            good.to_str().unwrap(),
            bad.to_str().unwrap(),
        ]);
        assert_eq!(output.status.code(), Some(1));
        let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
        assert_eq!(value["images"].as_array().unwrap().len(), 1);
        assert_eq!(value["images"][0]["width"], 2);
        assert_eq!(value["errors"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn unusable_image_is_explained_and_sets_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let bmp = dir.path().join("a.bmp");
        image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3]))
            .save(&bmp)
            .unwrap();
        let output = ttskit(&["image", "info", bmp.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1));
        let text = stdout(&output);
        assert!(
            text.contains("  error: the game does not load bmp"),
            "{text}"
        );
        assert!(text.contains("1 inspected, 1 not usable in the game, 0 unreadable"));

        let json = ttskit(&["image", "info", "--json", bmp.to_str().unwrap()]);
        assert_eq!(json.status.code(), Some(1));
        let value: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
        assert_eq!(value["images"][0]["compatible"], false);
        assert_eq!(value["images"][0]["problems"][0]["code"], "format-rejected");
        assert_eq!(value["images"][0]["problems"][0]["severity"], "error");
    }

    #[test]
    fn warnings_do_not_set_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        // Translucent everywhere: no token can be made, but the image loads.
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 128]))
            .save(&png)
            .unwrap();
        let output = ttskit(&["image", "info", png.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(0));
        assert!(stdout(&output).contains("  warning: no fully opaque pixel"));
    }

    #[test]
    fn output_is_identical_between_runs() {
        let dir = tempfile::tempdir().unwrap();
        png(&dir.path().join("a.png"), 5, 5);
        let args = ["image", "info", "--json", dir.path().to_str().unwrap()];
        assert_eq!(ttskit(&args).stdout, ttskit(&args).stdout);
    }
}

#[cfg(not(feature = "image"))]
#[test]
fn image_build_without_the_feature_says_so() {
    let output = ttskit(&["image", "build"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no image support"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn completions_prints_a_script_for_every_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = ttskit(&["completions", shell]);
        assert_eq!(output.status.code(), Some(0), "{shell}");
        let script = String::from_utf8_lossy(&output.stdout);
        assert!(script.contains("ttskit"), "{shell}: {script}");
        assert!(script.contains("completions"), "{shell}");
        assert!(output.stderr.is_empty(), "{shell}");
    }
}

#[test]
fn completions_rejects_an_unknown_shell() {
    let output = ttskit(&["completions", "nushell"]);
    assert_ne!(output.status.code(), Some(0));
}
