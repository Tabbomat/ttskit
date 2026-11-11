//! The `cloud` subcommands.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};

use serde_json::json;
use ttskit_cloud::{CloudBackend, CloudError, CloudIndex, IndexError, is_game_internal};

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
    /// Writing the output failed.
    #[error("cannot write output: {0}")]
    Io(#[from] io::Error),
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

/// Splits on `/`, drops empty segments and joins the rest again.
fn normalize(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
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
    let filter = match folder.map(normalize) {
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
        let entry_folder = normalize(&entry.folder);
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
}
