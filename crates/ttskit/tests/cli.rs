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
