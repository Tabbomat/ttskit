//! The `cloud` subcommands.

use std::io;

use serde_json::json;
use ttskit_cloud::{CloudBackend, CloudError, CloudFile};

/// Errors of the `cloud` subcommands.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// The cloud backend failed.
    #[error(transparent)]
    Cloud(#[from] CloudError),
    /// Writing the output failed.
    #[error("cannot write output: {0}")]
    Io(#[from] io::Error),
}

fn total_size(files: &[CloudFile]) -> u64 {
    files.iter().fold(0, |sum, f| sum.saturating_add(f.size))
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

/// Lists the cloud files as a table, or as JSON when `json` is set.
pub fn list(
    backend: &dyn CloudBackend,
    json: bool,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let files = backend.list()?;
    let total = total_size(&files);
    if json {
        write_json(&files, total, out)
    } else {
        write_table(&files, total, out)
    }
}

fn write_json(
    files: &[CloudFile],
    total: u64,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    let files: Vec<_> = files
        .iter()
        .map(|f| json!({ "name": f.name, "size": f.size }))
        .collect();
    let value = json!({ "files": files, "total_size": total });
    let text = serde_json::to_string_pretty(&value).map_err(io::Error::from)?;
    writeln!(out, "{text}")?;
    Ok(())
}

fn write_table(
    files: &[CloudFile],
    total: u64,
    out: &mut dyn io::Write,
) -> Result<(), CommandError> {
    if !files.is_empty() {
        let sizes: Vec<String> = files.iter().map(|f| f.size.to_string()).collect();
        let name_width = files
            .iter()
            .map(|f| f.name.chars().count())
            .max()
            .unwrap_or(0)
            .max("NAME".len());
        let size_width = sizes
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(0)
            .max("SIZE".len());
        writeln!(out, "{:<name_width$}  {:>size_width$}", "NAME", "SIZE")?;
        for (file, size) in files.iter().zip(&sizes) {
            writeln!(out, "{:<name_width$}  {size:>size_width$}", file.name)?;
        }
    }
    writeln!(
        out,
        "{} {}, {total} bytes",
        files.len(),
        plural(files.len())
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use ttskit_cloud::{MemoryCloud, Op};

    use super::*;

    fn run(cloud: &MemoryCloud, json: bool) -> String {
        let mut out = Vec::new();
        list(cloud, json, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn table_aligns_columns() {
        let cloud = MemoryCloud::new()
            .with_file("a.txt", vec![0; 5])
            .with_file("longer_name.json", vec![0; 12345]);
        assert_eq!(
            run(&cloud, false),
            [
                "NAME               SIZE",
                "a.txt                 5",
                "longer_name.json  12345",
                "2 files, 12350 bytes",
                "",
            ]
            .join("\n")
        );
    }

    #[test]
    fn short_names_use_header_width() {
        let cloud = MemoryCloud::new().with_file("a", vec![0; 1]);
        assert_eq!(
            run(&cloud, false),
            "NAME  SIZE\na        1\n1 file, 1 bytes\n"
        );
    }

    #[test]
    fn empty_cloud_prints_only_the_footer() {
        let cloud = MemoryCloud::new();
        assert_eq!(run(&cloud, false), "0 files, 0 bytes\n");
    }

    #[test]
    fn json_output() {
        let cloud = MemoryCloud::new()
            .with_file("b.txt", vec![0; 3])
            .with_file("a.txt", vec![0; 2]);
        let text = run(&cloud, true);
        assert!(text.ends_with("}\n"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value,
            json!({
                "files": [
                    {"name": "a.txt", "size": 2},
                    {"name": "b.txt", "size": 3},
                ],
                "total_size": 5,
            })
        );
    }

    #[test]
    fn empty_cloud_json() {
        let value: serde_json::Value =
            serde_json::from_str(&run(&MemoryCloud::new(), true)).unwrap();
        assert_eq!(value, json!({"files": [], "total_size": 0}));
    }

    #[test]
    fn total_saturates() {
        let files = [
            CloudFile {
                name: "a".into(),
                size: u64::MAX,
            },
            CloudFile {
                name: "b".into(),
                size: 1,
            },
        ];
        assert_eq!(total_size(&files), u64::MAX);
    }

    #[test]
    fn list_failure_propagates_without_output() {
        let cloud = MemoryCloud::new().with_file("a.txt", vec![0; 1]);
        for json in [false, true] {
            cloud.fail_once(Op::List, None, CloudError::Backend("boom".into()));
            let mut out = Vec::new();
            let err = list(&cloud, json, &mut out).unwrap_err();
            assert!(matches!(err, CommandError::Cloud(CloudError::Backend(_))));
            assert!(out.is_empty());
        }
    }
}
