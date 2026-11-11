//! The `cloud` subcommands.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::time::SystemTime;

use serde_json::json;
use ttskit_cloud::{
    Backup, BackupError, BackupStore, CloudBackend, CloudError, CloudIndex, IndexError,
    RestoreAction, is_game_internal, normalize_folder, plan_restore, restore,
};

/// Errors of the `cloud` subcommands.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// The cloud backend failed.
    #[error(transparent)]
    Cloud(#[from] CloudError),
    /// The folder index in the cloud cannot be read.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// The requested folder does not exist.
    #[error("no cloud folder \"{0}\"")]
    FolderNotFound(String),
    /// A backup or restore failed.
    #[error(transparent)]
    Backup(#[from] BackupError),
    /// No default backup directory can be determined.
    #[error("cannot determine the local data directory; use --backup-dir")]
    NoBackupDir,
    /// Writing the output failed.
    #[error("cannot write output: {0}")]
    Io(#[from] io::Error),
}

/// The directory holding the cloud index backups: `explicit` if given,
/// otherwise `<local app data>/ttskit/cloud-backups`.
pub fn backup_dir(explicit: Option<PathBuf>) -> Result<PathBuf, CommandError> {
    match explicit {
        Some(dir) => Ok(dir),
        None => dirs::data_local_dir()
            .map(|d| d.join("ttskit").join("cloud-backups"))
            .ok_or(CommandError::NoBackupDir),
    }
}

/// Lists the backups in `store`, newest first.
pub fn list_backups(store: &BackupStore, out: &mut dyn io::Write) -> Result<(), CommandError> {
    let backups = store.list()?;
    let dir = store.root().display();
    let mut buf: Vec<u8> = Vec::new();
    if backups.is_empty() {
        writeln!(buf, "No backups in {dir}")?;
    } else {
        writeln!(buf, "Backups in {dir}:")?;
        let width = backups.iter().map(|b| b.name.len()).max().unwrap_or(0);
        for backup in &backups {
            let files: Vec<String> = backup
                .files
                .iter()
                .map(|f| match f.size {
                    Some(size) => format!("{} {size} bytes", f.name),
                    None => format!("{} absent", f.name),
                })
                .collect();
            writeln!(buf, "  {:<width$}  {}", backup.name, files.join(", "))?;
        }
    }
    out.write_all(&buf)?;
    Ok(())
}

/// Plans restoring `backup` and prints the plan. With `yes` the restore is
/// carried out too (after backing up the current index); without it nothing
/// is changed.
pub fn restore_backup(
    backend: &mut dyn CloudBackend,
    store: &BackupStore,
    backup: &Backup,
    yes: bool,
    now: SystemTime,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let plan = plan_restore(backend, backup)?;
    let mut buf: Vec<u8> = Vec::new();
    writeln!(buf, "Restore backup {}:", backup.name)?;
    for step in &plan.steps {
        let (action, detail) = match step.action {
            RestoreAction::Write { size } => ("write", format!(" ({size} bytes)")),
            RestoreAction::Delete => ("delete", String::new()),
            RestoreAction::Unchanged => ("unchanged", String::new()),
        };
        writeln!(buf, "  {action:<9}  {}{detail}", step.file)?;
    }
    if plan.is_noop() {
        writeln!(
            buf,
            "Nothing to do: the cloud index already matches this backup."
        )?;
    } else if !yes {
        writeln!(
            buf,
            "Dry run: nothing was changed. Run again with --yes to restore."
        )?;
    }
    out.write_all(&buf)?;
    if plan.is_noop() || !yes {
        return Ok(());
    }

    let outcome = restore(backend, store, &plan, now)?;
    if let Some(pre) = &outcome.pre_restore_backup {
        writeln!(out, "Backed up the current cloud index to {}.", pre.name)?;
    }
    writeln!(out, "Restored backup {}.", backup.name)?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Missing,
    Unindexed,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Missing => "missing",
            Status::Unindexed => "unindexed",
        }
    }
}

/// A file as shown by `cloud list`.
struct FileRow {
    name: String,
    cloud_name: String,
    /// Normalized folder; `None` for files that are not in the index.
    folder: Option<String>,
    size: Option<u64>,
    url: Option<String>,
    date: Option<String>,
    status: Status,
}

fn lower_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_ascii_lowercase()
        .cmp(&b.to_ascii_lowercase())
        .then_with(|| a.cmp(b))
}

fn total_size(files: &[FileRow]) -> u64 {
    files
        .iter()
        .fold(0, |sum, f| sum.saturating_add(f.size.unwrap_or(0)))
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Lists the cloud files and folders as a tree, or as JSON when `json` is set.
/// `folder` limits the output to that folder and its subfolders.
pub fn list(
    backend: &dyn CloudBackend,
    folder: Option<&str>,
    json: bool,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let steam_files = backend.list()?;
    let index = CloudIndex::load(backend)?;
    let all_folders = index.all_folders();

    // Lowercase filter path; `None` shows everything.
    let filter = match folder.map(normalize_folder) {
        Some(f) if !f.is_empty() => {
            let f = f.to_ascii_lowercase();
            if !all_folders.iter().any(|a| a.eq_ignore_ascii_case(&f)) {
                return Err(CommandError::FolderNotFound(
                    folder.unwrap_or_default().to_owned(),
                ));
            }
            Some(f)
        }
        _ => None,
    };
    let in_filter = |path: &str| match &filter {
        None => true,
        Some(f) => {
            let p = path.to_ascii_lowercase();
            p == *f
                || p.strip_prefix(f.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        }
    };

    let folders: Vec<String> = all_folders.into_iter().filter(|f| in_filter(f)).collect();

    let steam: HashMap<String, u64> = steam_files
        .iter()
        .map(|f| (f.name.to_ascii_lowercase(), f.size))
        .collect();
    let mut files: Vec<FileRow> = Vec::new();
    let mut referenced: HashSet<String> = HashSet::new();
    for entry in &index.entries {
        let key = entry.key.to_ascii_lowercase();
        let found = steam.get(&key).copied();
        referenced.insert(key);
        let entry_folder = normalize_folder(&entry.folder);
        if !in_filter(&entry_folder) {
            continue;
        }
        files.push(FileRow {
            name: entry.name.clone(),
            cloud_name: entry.key.clone(),
            folder: Some(entry_folder),
            size: found.or(entry.size),
            url: entry.url.clone(),
            date: entry.date.clone(),
            status: if found.is_some() {
                Status::Ok
            } else {
                Status::Missing
            },
        });
    }
    files.sort_by(|a, b| {
        let key = |f: &FileRow| {
            (
                f.folder.as_deref().unwrap_or("").to_ascii_lowercase(),
                f.name.to_ascii_lowercase(),
            )
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| a.cloud_name.cmp(&b.cloud_name))
    });
    if filter.is_none() {
        let mut unindexed: Vec<FileRow> = steam_files
            .iter()
            .filter(|f| {
                !referenced.contains(&f.name.to_ascii_lowercase()) && !is_game_internal(&f.name)
            })
            .map(|f| FileRow {
                name: f.name.clone(),
                cloud_name: f.name.clone(),
                folder: None,
                size: Some(f.size),
                url: None,
                date: None,
                status: Status::Unindexed,
            })
            .collect();
        unindexed.sort_by(|a, b| lower_cmp(&a.name, &b.name));
        files.extend(unindexed);
    }

    // Build everything first so that nothing is written on an error.
    let mut buf: Vec<u8> = Vec::new();
    if json {
        write_json(&folders, &files, &mut buf)?;
    } else {
        write_tree(&folders, &files, filter.as_deref(), &mut buf)?;
    }
    out.write_all(&buf)?;
    Ok(())
}

fn write_json(
    folders: &[String],
    files: &[FileRow],
    out: &mut Vec<u8>,
) -> Result<(), CommandError> {
    let rows: Vec<_> = files
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "cloud_name": f.cloud_name,
                "folder": f.folder,
                "size": f.size,
                "url": f.url,
                "date": f.date,
                "status": f.status.as_str(),
            })
        })
        .collect();
    let value = json!({
        "folders": folders,
        "files": rows,
        "total_size": total_size(files),
    });
    let text = serde_json::to_string_pretty(&value).map_err(io::Error::from)?;
    out.extend_from_slice(text.as_bytes());
    out.push(b'\n');
    Ok(())
}

/// One line of the tree. Lines with a `size` take part in column alignment.
struct Line {
    label: String,
    size: Option<String>,
    missing: bool,
}

impl Line {
    fn text(label: String) -> Self {
        Line {
            label,
            size: None,
            missing: false,
        }
    }
}

fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

fn last_segment(path: &str) -> &str {
    path.rfind('/').map_or(path, |i| &path[i + 1..])
}

fn emit_children(
    path: &str,
    depth: usize,
    folders: &[String],
    files: &[FileRow],
    lines: &mut Vec<Line>,
) {
    let indent = "  ".repeat(depth);
    let mut subs: Vec<&String> = folders.iter().filter(|f| parent_of(f) == path).collect();
    subs.sort_by(|a, b| lower_cmp(last_segment(a), last_segment(b)));
    for sub in subs {
        lines.push(Line::text(format!("{indent}{}/", last_segment(sub))));
        emit_children(sub, depth + 1, folders, files, lines);
    }
    // `files` is sorted by (folder, name, cloud name) already.
    for f in files.iter().filter(|f| f.folder.as_deref() == Some(path)) {
        lines.push(Line {
            label: format!("{indent}{}", f.name),
            size: Some(f.size.map_or_else(|| "-".to_owned(), |s| s.to_string())),
            missing: f.status == Status::Missing,
        });
    }
}

fn write_tree(
    folders: &[String],
    files: &[FileRow],
    filter: Option<&str>,
    out: &mut Vec<u8>,
) -> Result<(), CommandError> {
    let mut lines: Vec<Line> = Vec::new();
    match filter {
        None => emit_children("", 0, folders, files, &mut lines),
        Some(f) => {
            let mut roots: Vec<&String> = folders
                .iter()
                .filter(|p| p.eq_ignore_ascii_case(f))
                .collect();
            roots.sort();
            for root in roots {
                lines.push(Line::text(format!("{root}/")));
                emit_children(root, 1, folders, files, &mut lines);
            }
        }
    }
    let unindexed: Vec<&FileRow> = files
        .iter()
        .filter(|f| f.status == Status::Unindexed)
        .collect();
    if !unindexed.is_empty() {
        lines.push(Line::text("Not in index:".to_owned()));
        for f in &unindexed {
            lines.push(Line {
                label: format!("  {}", f.name),
                size: f.size.map(|s| s.to_string()),
                missing: false,
            });
        }
    }

    let label_width = lines
        .iter()
        .filter(|l| l.size.is_some())
        .map(|l| l.label.chars().count())
        .max()
        .unwrap_or(0);
    let size_width = lines
        .iter()
        .filter_map(|l| l.size.as_ref())
        .map(String::len)
        .max()
        .unwrap_or(0);
    for line in &lines {
        match &line.size {
            Some(size) => {
                let marker = if line.missing { "  missing" } else { "" };
                writeln!(
                    out,
                    "{:<label_width$}  {size:>size_width$}{marker}",
                    line.label
                )?;
            }
            None => writeln!(out, "{}", line.label)?,
        }
    }

    let missing = files.iter().filter(|f| f.status == Status::Missing).count();
    let not_indexed = unindexed.len();
    let mut footer = format!(
        "{} in {}, {} bytes",
        count(files.len(), "file"),
        count(folders.len(), "folder"),
        total_size(files)
    );
    if missing > 0 {
        footer.push_str(&format!(", {missing} missing"));
    }
    if not_indexed > 0 {
        footer.push_str(&format!(", {not_indexed} not in index"));
    }
    writeln!(out, "{footer}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use ttskit_cloud::{CLOUD_FOLDER_FILE, CLOUD_INFO_FILE, IndexEntry, MemoryCloud, Op};

    use super::*;

    fn entry(key: &str, name: &str, size: Option<u64>, folder: &str) -> IndexEntry {
        IndexEntry::new(key, name, None, size, None, folder)
    }

    fn seed(cloud: MemoryCloud, index: &CloudIndex) -> MemoryCloud {
        cloud
            .with_file(CLOUD_INFO_FILE, index.encode_info().unwrap())
            .with_file(CLOUD_FOLDER_FILE, index.encode_folders().unwrap())
    }

    /// The example from the feature description.
    fn example() -> MemoryCloud {
        let mut index = CloudIndex::empty();
        index.folders = vec!["Decks/Old".into()];
        index.entries = vec![
            entry("AA_back.png", "back.png", Some(2048), "Decks"),
            entry("BB_front.png", "front.png", Some(12345), "Decks"),
            entry("CC_red.png", "red.png", Some(100), "Tokens/Red"),
            entry("DD_readme.txt", "readme.txt", Some(10), ""),
        ];
        seed(
            MemoryCloud::new()
                .with_file("AA_back.png", vec![0; 2048])
                .with_file("BB_front.png", vec![0; 12345])
                .with_file("DD_readme.txt", vec![0; 10])
                .with_file("stray.png", vec![0; 512]),
            &index,
        )
    }

    fn try_run(
        cloud: &MemoryCloud,
        folder: Option<&str>,
        json: bool,
    ) -> (Result<(), CommandError>, String) {
        let mut out = Vec::new();
        let result = list(cloud, folder, json, &mut out);
        (result, String::from_utf8(out).unwrap())
    }

    fn run(cloud: &MemoryCloud, folder: Option<&str>, json: bool) -> String {
        let (result, text) = try_run(cloud, folder, json);
        result.unwrap();
        text
    }

    fn json_of(cloud: &MemoryCloud, folder: Option<&str>) -> serde_json::Value {
        let text = run(cloud, folder, true);
        assert!(text.ends_with("}\n"));
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn example_tree() {
        let text = run(&example(), None, false);
        println!("{text}");
        assert_eq!(
            text,
            [
                "Decks/",
                "  Old/",
                "  back.png    2048",
                "  front.png  12345",
                "Tokens/",
                "  Red/",
                "    red.png    100  missing",
                "readme.txt      10",
                "Not in index:",
                "  stray.png    512",
                "5 files in 4 folders, 15015 bytes, 1 missing, 1 not in index",
                "",
            ]
            .join("\n")
        );
    }

    #[test]
    fn subfolders_come_before_files_and_sort_case_insensitively() {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_b.txt", "b.txt", Some(1), "x"),
            entry("AA_B.txt", "B.txt", Some(1), "x"),
            entry("AA_a.txt", "a.txt", Some(1), "x/z"),
            entry("AA_c.txt", "c.txt", Some(1), "x/Y"),
            entry("AA_top.txt", "top.txt", Some(1), ""),
        ];
        let text = run(&seed(MemoryCloud::new(), &index), None, false);
        let names: Vec<&str> = text
            .lines()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(
            &names[..8],
            [
                "x/", "Y/", "c.txt", "z/", "a.txt", "B.txt", "b.txt", "top.txt"
            ]
        );
        assert!(text.contains("\n    c.txt"));
        assert!(text.contains("\n  B.txt"));
    }

    #[test]
    fn empty_folder_is_shown() {
        let mut index = CloudIndex::empty();
        index.folders = vec!["Empty".into()];
        let text = run(&seed(MemoryCloud::new(), &index), None, false);
        assert_eq!(text, "Empty/\n0 files in 1 folder, 0 bytes\n");
    }

    #[test]
    fn missing_entry_marker_and_counts() {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_a.png", "a.png", Some(7), ""),
            entry("BB_b.png", "b.png", None, ""),
        ];
        let cloud = seed(MemoryCloud::new(), &index);
        assert_eq!(
            run(&cloud, None, false),
            "a.png  7  missing\nb.png  -  missing\n2 files in 0 folders, 7 bytes, 2 missing\n"
        );
    }

    #[test]
    fn steam_file_is_matched_case_insensitively_and_uses_steam_size() {
        let mut index = CloudIndex::empty();
        index.entries = vec![entry("abcd_a.png", "a.png", Some(7), "")];
        let cloud = seed(
            MemoryCloud::new().with_file("ABCD_A.PNG", vec![0; 3]),
            &index,
        );
        assert_eq!(
            run(&cloud, None, false),
            "a.png  3\n1 file in 0 folders, 3 bytes\n"
        );
    }

    #[test]
    fn same_folder_and_name_with_different_sha1_are_both_shown() {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_a.png", "a.png", Some(1), ""),
            entry("BB_a.png", "a.png", Some(2), ""),
        ];
        let cloud = seed(
            MemoryCloud::new()
                .with_file("AA_a.png", vec![0])
                .with_file("BB_a.png", vec![0; 2]),
            &index,
        );
        assert_eq!(
            run(&cloud, None, false),
            "a.png  1\na.png  2\n2 files in 0 folders, 3 bytes\n"
        );
    }

    #[test]
    fn game_internal_files_are_hidden() {
        let cloud = MemoryCloud::new()
            .with_file("WorkshopUpload", vec![0; 5])
            .with_file("WorkshopImageUpload.png", vec![0; 5]);
        let cloud = seed(cloud, &CloudIndex::empty());
        assert_eq!(run(&cloud, None, false), "0 files in 0 folders, 0 bytes\n");
    }

    #[test]
    fn without_index_all_files_are_not_in_index() {
        let cloud = MemoryCloud::new()
            .with_file("b.png", vec![0; 2])
            .with_file("A.png", vec![0; 10]);
        assert_eq!(
            run(&cloud, None, false),
            "Not in index:\n  A.png  10\n  b.png   2\n2 files in 0 folders, 12 bytes, 2 not in index\n"
        );
    }

    #[test]
    fn empty_cloud_prints_only_the_footer() {
        assert_eq!(
            run(&MemoryCloud::new(), None, false),
            "0 files in 0 folders, 0 bytes\n"
        );
    }

    #[test]
    fn folder_filter_is_recursive_case_insensitive_and_slash_tolerant() {
        for spelling in ["decks", "/DECKS/", "Decks", "//decks//"] {
            let text = run(&example(), Some(spelling), false);
            assert_eq!(
                text,
                [
                    "Decks/",
                    "  Old/",
                    "  back.png    2048",
                    "  front.png  12345",
                    "2 files in 2 folders, 14393 bytes",
                    "",
                ]
                .join("\n"),
                "{spelling}"
            );
        }
    }

    /// The game separates folder levels with `\`.
    fn backslash_example() -> MemoryCloud {
        let mut index = CloudIndex::empty();
        index.folders = vec!["Game".into(), "Game\\dice".into(), "Game\\token".into()];
        index.entries = vec![
            entry("AA_die.obj", "die.obj", Some(3), "Game\\dice"),
            entry("BB_vp.jpg", "vp.jpg", Some(5), "Game\\token"),
        ];
        seed(
            MemoryCloud::new()
                .with_file("AA_die.obj", vec![0; 3])
                .with_file("BB_vp.jpg", vec![0; 5]),
            &index,
        )
    }

    #[test]
    fn backslash_folders_are_nested() {
        assert_eq!(
            run(&backslash_example(), None, false),
            [
                "Game/",
                "  dice/",
                "    die.obj  3",
                "  token/",
                "    vp.jpg   5",
                "2 files in 3 folders, 8 bytes",
                "",
            ]
            .join("\n")
        );
        let value = json_of(&backslash_example(), None);
        assert_eq!(value["folders"], json!(["Game", "Game/dice", "Game/token"]));
        assert_eq!(value["files"][0]["folder"], "Game/dice");
    }

    #[test]
    fn folder_filter_accepts_either_separator() {
        let expected = "Game/dice/\n  die.obj  3\n1 file in 1 folder, 3 bytes\n";
        for spelling in ["game/dice", "Game\\dice", "\\game\\DICE\\"] {
            assert_eq!(
                run(&backslash_example(), Some(spelling), false),
                expected,
                "{spelling}"
            );
        }
        let text = run(&backslash_example(), Some("game"), false);
        assert!(text.starts_with("Game/\n  dice/\n"));
        assert!(text.ends_with("2 files in 3 folders, 8 bytes\n"));
    }

    #[test]
    fn folder_filter_on_nested_folder_shows_full_path() {
        let text = run(&example(), Some("tokens/red"), false);
        assert_eq!(
            text,
            "Tokens/Red/\n  red.png  100  missing\n1 file in 1 folder, 100 bytes, 1 missing\n"
        );
    }

    #[test]
    fn folder_filter_does_not_match_name_prefixes() {
        let mut index = CloudIndex::empty();
        index.entries = vec![
            entry("AA_a.png", "a.png", Some(1), "Deck"),
            entry("BB_b.png", "b.png", Some(1), "Decks"),
        ];
        let cloud = seed(MemoryCloud::new(), &index);
        let text = run(&cloud, Some("Deck"), false);
        assert!(text.starts_with("Deck/\n  a.png"));
        assert!(!text.contains("b.png"));
    }

    #[test]
    fn unknown_folder_is_an_error_and_writes_nothing() {
        for json in [false, true] {
            let (result, text) = try_run(&example(), Some("/Nope/"), json);
            let err = result.unwrap_err();
            assert!(matches!(err, CommandError::FolderNotFound(_)));
            assert_eq!(err.to_string(), "no cloud folder \"/Nope/\"");
            assert!(text.is_empty());
        }
    }

    #[test]
    fn slash_folder_means_everything() {
        let cloud = example();
        assert_eq!(run(&cloud, Some("/"), false), run(&cloud, None, false));
        assert_eq!(run(&cloud, Some(""), true), run(&cloud, None, true));
    }

    #[test]
    fn json_shape_and_order() {
        let mut index = CloudIndex::empty();
        index.folders = vec!["Decks/Old".into()];
        index.entries = vec![
            IndexEntry::new(
                "BB_b.png",
                "b.png",
                Some("http://example.invalid/b".into()),
                Some(5),
                Some("1/2/2020".into()),
                "Decks",
            ),
            entry("AA_a.png", "a.png", None, "Decks"),
            entry("CC_r.png", "r.png", Some(9), ""),
        ];
        let cloud = seed(
            MemoryCloud::new()
                .with_file("BB_b.png", vec![0; 4])
                .with_file("zed.txt", vec![0; 2])
                .with_file("Alpha.txt", vec![0; 1]),
            &index,
        );
        assert_eq!(
            json_of(&cloud, None),
            json!({
                "folders": ["Decks", "Decks/Old"],
                "files": [
                    {"name": "r.png", "cloud_name": "CC_r.png", "folder": "", "size": 9,
                     "url": null, "date": null, "status": "missing"},
                    {"name": "a.png", "cloud_name": "AA_a.png", "folder": "Decks", "size": null,
                     "url": null, "date": null, "status": "missing"},
                    {"name": "b.png", "cloud_name": "BB_b.png", "folder": "Decks", "size": 4,
                     "url": "http://example.invalid/b", "date": "1/2/2020", "status": "ok"},
                    {"name": "Alpha.txt", "cloud_name": "Alpha.txt", "folder": null, "size": 1,
                     "url": null, "date": null, "status": "unindexed"},
                    {"name": "zed.txt", "cloud_name": "zed.txt", "folder": null, "size": 2,
                     "url": null, "date": null, "status": "unindexed"},
                ],
                "total_size": 16,
            })
        );
    }

    #[test]
    fn json_with_folder_filter() {
        let value = json_of(&example(), Some("decks"));
        assert_eq!(value["folders"], json!(["Decks", "Decks/Old"]));
        let names: Vec<_> = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["back.png", "front.png"]);
        assert_eq!(value["total_size"], 14393);
    }

    #[test]
    fn empty_cloud_json() {
        assert_eq!(
            json_of(&MemoryCloud::new(), None),
            json!({"folders": [], "files": [], "total_size": 0})
        );
    }

    #[test]
    fn malformed_index_is_an_error_and_writes_nothing() {
        let cloud = MemoryCloud::new()
            .with_file(CLOUD_INFO_FILE, b"not bson".to_vec())
            .with_file("a.png", vec![0]);
        for json in [false, true] {
            let (result, text) = try_run(&cloud, None, json);
            assert!(matches!(result.unwrap_err(), CommandError::Index(_)));
            assert!(text.is_empty());
        }
    }

    #[test]
    fn singular_forms() {
        let mut index = CloudIndex::empty();
        index.entries = vec![entry("AA_a.png", "a.png", Some(1), "d")];
        let cloud = seed(MemoryCloud::new().with_file("AA_a.png", vec![0]), &index);
        let text = run(&cloud, None, false);
        assert_eq!(text.lines().last(), Some("1 file in 1 folder, 1 bytes"));
    }

    #[test]
    fn total_saturates() {
        let row = |size| FileRow {
            name: "a".into(),
            cloud_name: "a".into(),
            folder: None,
            size: Some(size),
            url: None,
            date: None,
            status: Status::Ok,
        };
        assert_eq!(total_size(&[row(u64::MAX), row(1)]), u64::MAX);
    }

    #[test]
    fn list_failure_propagates_without_output() {
        let cloud = example();
        for json in [false, true] {
            cloud.fail_once(Op::List, None, CloudError::Backend("boom".into()));
            let (result, text) = try_run(&cloud, None, json);
            assert!(matches!(
                result.unwrap_err(),
                CommandError::Cloud(CloudError::Backend(_))
            ));
            assert!(text.is_empty());
        }
    }

    mod restore_tests {
        use std::fs;
        use std::time::{Duration, UNIX_EPOCH};

        use super::*;

        fn index_cloud(name: &str) -> MemoryCloud {
            let mut index = CloudIndex::empty();
            index.entries = vec![entry(&format!("AA_{name}"), name, Some(1), "")];
            seed(MemoryCloud::new(), &index)
        }

        fn time(secs: u64) -> SystemTime {
            UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
        }

        fn names(store: &BackupStore) -> Vec<String> {
            store.list().unwrap().into_iter().map(|b| b.name).collect()
        }

        fn restore_run(
            cloud: &mut MemoryCloud,
            store: &BackupStore,
            name: &str,
            yes: bool,
        ) -> (Result<(), CommandError>, String) {
            let mut out = Vec::new();
            let result = store
                .get(name)
                .map_err(CommandError::from)
                .and_then(|b| restore_backup(cloud, store, &b, yes, time(100), &mut out));
            (result, String::from_utf8(out).unwrap())
        }

        /// A store holding one backup of `index_cloud("a.png")`, and a cloud
        /// that differs from it.
        fn setup() -> (tempfile::TempDir, BackupStore, MemoryCloud, String) {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            let backup = store.create(&index_cloud("a.png"), time(0)).unwrap();
            // The backup lacks CloudFolder.bson: it did not exist in that cloud.
            fs::remove_file(backup.path.join(CLOUD_FOLDER_FILE)).unwrap();
            (dir, store, index_cloud("b.png"), backup.name)
        }

        fn list_text(store: &BackupStore) -> String {
            let mut out = Vec::new();
            list_backups(store, &mut out).unwrap();
            String::from_utf8(out).unwrap()
        }

        #[test]
        fn list_without_backups() {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            assert_eq!(
                list_text(&store),
                format!("No backups in {}\n", dir.path().display())
            );
        }

        #[test]
        fn list_with_missing_dir() {
            let dir = tempfile::tempdir().unwrap();
            let missing = dir.path().join("nope");
            let store = BackupStore::new(&missing);
            assert_eq!(
                list_text(&store),
                format!("No backups in {}\n", missing.display())
            );
        }

        #[test]
        fn list_shows_newest_first_with_absent_files() {
            let dir = tempfile::tempdir().unwrap();
            let older = dir.path().join("20261110T090000Z");
            fs::create_dir(&older).unwrap();
            fs::write(older.join(CLOUD_INFO_FILE), vec![0; 1200]).unwrap();
            let newer = dir.path().join("20261111T184948Z");
            fs::create_dir(&newer).unwrap();
            fs::write(newer.join(CLOUD_INFO_FILE), vec![0; 1234]).unwrap();
            fs::write(newer.join(CLOUD_FOLDER_FILE), vec![0; 56]).unwrap();
            fs::create_dir(dir.path().join("not-a-backup")).unwrap();
            let store = BackupStore::new(dir.path());
            assert_eq!(
                list_text(&store),
                format!(
                    "Backups in {}:\n\
                     \x20 20261111T184948Z  CloudInfo.bson 1234 bytes, CloudFolder.bson 56 bytes\n\
                     \x20 20261110T090000Z  CloudInfo.bson 1200 bytes, CloudFolder.bson absent\n",
                    dir.path().display()
                )
            );
        }

        #[test]
        fn dry_run_prints_the_plan_and_changes_nothing() {
            let (_dir, store, mut cloud, name) = setup();
            cloud.clear_calls();
            let (result, text) = restore_run(&mut cloud, &store, &name, false);
            result.unwrap();
            let size = store.get(&name).unwrap().files[0].size.unwrap();
            assert_eq!(
                text,
                format!(
                    "Restore backup {name}:\n\
                     \x20 write      CloudInfo.bson ({size} bytes)\n\
                     \x20 delete     CloudFolder.bson\n\
                     Dry run: nothing was changed. Run again with --yes to restore.\n"
                )
            );
            assert!(cloud.mutating_calls().is_empty());
            assert_eq!(names(&store), [name]);
        }

        #[test]
        fn yes_restores_and_backs_up_the_current_index() {
            let (_dir, store, mut cloud, name) = setup();
            let old_info = cloud.contents(CLOUD_INFO_FILE).unwrap();
            let old_folders = cloud.contents(CLOUD_FOLDER_FILE).unwrap();
            let (result, text) = restore_run(&mut cloud, &store, &name, true);
            result.unwrap();

            let backup = store.get(&name).unwrap();
            assert_eq!(
                cloud.contents(CLOUD_INFO_FILE),
                backup.read(CLOUD_INFO_FILE).unwrap()
            );
            // The backup has no CloudFolder.bson, so it is deleted.
            assert_eq!(backup.read(CLOUD_FOLDER_FILE).unwrap(), None);
            assert_eq!(cloud.contents(CLOUD_FOLDER_FILE), None);

            let all = names(&store);
            assert_eq!(all.len(), 2);
            let pre = store.get(&all[0]).unwrap();
            assert_ne!(pre.name, name);
            assert_eq!(pre.read(CLOUD_INFO_FILE).unwrap(), Some(old_info));
            assert_eq!(pre.read(CLOUD_FOLDER_FILE).unwrap(), Some(old_folders));
            assert!(text.contains("  delete     CloudFolder.bson\n"));
            assert!(text.ends_with(&format!(
                "Backed up the current cloud index to {}.\nRestored backup {name}.\n",
                pre.name
            )));
        }

        #[test]
        fn matching_cloud_is_a_noop_with_and_without_yes() {
            let (_dir, store, _, name) = setup();
            for yes in [false, true] {
                let mut cloud = index_cloud("a.png");
                cloud.delete(CLOUD_FOLDER_FILE).unwrap();
                cloud.clear_calls();
                let (result, text) = restore_run(&mut cloud, &store, &name, yes);
                result.unwrap();
                assert!(text.contains("  unchanged  CloudInfo.bson\n"));
                assert!(
                    text.ends_with("Nothing to do: the cloud index already matches this backup.\n")
                );
                assert!(!text.contains("Dry run"));
                assert!(cloud.mutating_calls().is_empty());
                assert_eq!(names(&store).as_slice(), std::slice::from_ref(&name));
            }
        }

        #[test]
        fn invalid_backup_content_is_an_error_without_writes() {
            let dir = tempfile::tempdir().unwrap();
            let store = BackupStore::new(dir.path());
            let bad = dir.path().join("20260101T000000Z");
            fs::create_dir(&bad).unwrap();
            fs::write(bad.join(CLOUD_INFO_FILE), b"not bson").unwrap();
            let mut cloud = index_cloud("b.png");
            cloud.clear_calls();
            let (result, text) = restore_run(&mut cloud, &store, "20260101T000000Z", true);
            assert!(matches!(
                result.unwrap_err(),
                CommandError::Backup(BackupError::InvalidBackup { .. })
            ));
            assert!(text.is_empty());
            assert!(cloud.mutating_calls().is_empty());
            assert_eq!(names(&store), ["20260101T000000Z"]);
        }

        #[test]
        fn missing_and_invalid_names_are_errors() {
            let (_dir, store, mut cloud, _) = setup();
            let (result, text) = restore_run(&mut cloud, &store, "20200101T000000Z", true);
            assert!(matches!(
                result.unwrap_err(),
                CommandError::Backup(BackupError::NotFound { .. })
            ));
            assert!(text.is_empty());
            let (result, _) = restore_run(&mut cloud, &store, "nope", true);
            assert!(matches!(
                result.unwrap_err(),
                CommandError::Backup(BackupError::InvalidName { .. })
            ));
        }

        #[test]
        fn backup_dir_prefers_the_explicit_value() {
            let dir = PathBuf::from("some/where");
            assert_eq!(backup_dir(Some(dir.clone())).unwrap(), dir);
        }

        #[test]
        fn backup_dir_default_ends_with_ttskit_cloud_backups() {
            // Nothing to check on machines without a local data directory.
            if let Ok(dir) = backup_dir(None) {
                let parts: Vec<_> = dir.components().rev().take(2).collect();
                assert_eq!(parts[0].as_os_str(), "cloud-backups");
                assert_eq!(parts[1].as_os_str(), "ttskit");
            }
        }
    }
}
