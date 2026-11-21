//! Unpacking a save into the directory layout of `docs/pack-layout.md`.
//!
//! [`unpack`] is a pure function: it turns the bytes of a save into an ordered
//! list of files held in memory ([`Unpacked`]). [`Unpacked::write_to_dir`]
//! writes them to disk; it overwrites or deletes only when asked to (see
//! [`WriteOptions`]).
//!
//! Every field of the save ends up in exactly one file. Extracted fields keep
//! their key position in `save.json` / `object.json`, where a marker string
//! (`file:script.lua`, `dir:contents`, ...) replaces the value.

pub mod eol;
pub mod names;
pub mod state;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::save::write::{Eol, to_vec_with};
use crate::save::{Save, SaveError};
use names::Siblings;
pub use state::{StateKind, classify_state, compact_state};

/// Version of the layout written by this module (`format` in the metadata).
pub const FORMAT_VERSION: u32 = 1;

/// Path of the metadata file, relative to the output directory.
pub const METADATA_PATH: &str = ".ttskit/unpack.json";

/// Errors from unpacking or writing the result.
#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    /// The save could not be read.
    #[error(transparent)]
    Save(#[from] SaveError),
    /// The output directory exists and is not empty.
    #[error("output directory {} exists and is not empty; nothing was written", .0.display())]
    NotEmpty(PathBuf),
    /// The output path exists but is not a directory.
    #[error("output path {} exists and is not a directory", .0.display())]
    NotADirectory(PathBuf),
    /// The metadata of a previous unpack cannot be used for `--clean`.
    #[error("cannot use {} to clean: {reason}", path.display())]
    BadMetadata {
        /// The metadata file.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// Writing a file or directory failed.
    #[error("cannot write {}: {source}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
}

/// One file of the unpacked tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Path relative to the output directory, `/` separated.
    pub path: String,
    /// The file content.
    pub data: Vec<u8>,
}

/// Summary of an unpack run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnpackReport {
    /// Number of object directories, at any depth.
    pub objects: usize,
    /// Number of files, including the metadata file.
    pub files: usize,
    /// Text files whose field had mixed line endings.
    pub mixed_eol: Vec<String>,
}

/// The result of [`unpack`]: the files to write, in write order, and a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unpacked {
    /// All files; the metadata file is last.
    pub files: Vec<OutputFile>,
    /// Counts and warnings.
    pub report: UnpackReport,
}

/// How [`Unpacked::plan`] treats an existing output directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOptions {
    /// Allow writing into a non-empty directory. Files that unpack creates are
    /// overwritten; every other file is left alone.
    pub force: bool,
    /// Also remove the files a previous unpack created (listed in its
    /// metadata) that this unpack no longer creates. Never removes anything
    /// else.
    pub clean: bool,
}

/// What writing an [`Unpacked`] tree into a directory would do. All paths are
/// relative to the directory and `/` separated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WritePlan {
    /// Files that do not exist yet.
    pub create: Vec<String>,
    /// Existing files whose content differs.
    pub change: Vec<String>,
    /// Existing files with identical content.
    pub unchanged: Vec<String>,
    /// Stale files of a previous unpack (only with [`WriteOptions::clean`]).
    pub remove: Vec<String>,
}

/// Reads the `files` list of a previous metadata file. Entries that are not
/// plain relative paths (absolute, `..`, empty components) are rejected, so a
/// damaged or hand-made file can never point outside the directory. A missing
/// metadata file means nothing was tracked.
fn previous_files(dir: &Path) -> Result<Vec<String>, UnpackError> {
    let path = dir.join(METADATA_PATH);
    let bad = |reason: String| UnpackError::BadMetadata {
        path: path.clone(),
        reason,
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(UnpackError::Io {
                path: path.clone(),
                source,
            });
        }
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| bad(e.to_string()))?;
    let Some(Value::Array(files)) = value.get("files") else {
        return Err(bad("missing `files` list".to_owned()));
    };
    let mut out = Vec::with_capacity(files.len());
    for entry in files {
        let Value::String(name) = entry else {
            return Err(bad("`files` must hold strings".to_owned()));
        };
        let safe = name
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', ':']));
        if !safe {
            return Err(bad(format!("unsafe path `{name}` in `files`")));
        }
        out.push(name.clone());
    }
    Ok(out)
}

impl Unpacked {
    /// Writes the tree into `dir`.
    ///
    /// `dir` may be missing or an empty directory. If it is a non-empty
    /// directory the call fails before anything is written. Nothing is ever
    /// overwritten or deleted. Use [`Unpacked::plan`] and [`Unpacked::apply`]
    /// to overwrite or clean.
    pub fn write_to_dir(&self, dir: &Path) -> Result<(), UnpackError> {
        let plan = self.plan(dir, WriteOptions::default())?;
        self.apply(dir, &plan)
    }

    /// Works out what writing into `dir` would do, without changing anything.
    ///
    /// Fails if `dir` is a file, if it is a non-empty directory and `force` is
    /// not set, if a file of the tree would land on something that is not a
    /// regular file, or (with `clean`) if the previous metadata is unusable.
    pub fn plan(&self, dir: &Path, options: WriteOptions) -> Result<WritePlan, UnpackError> {
        let io_err = |path: &Path| {
            let path = path.to_owned();
            move |source| UnpackError::Io { path, source }
        };
        let mut exists = false;
        match std::fs::metadata(dir) {
            Ok(meta) if !meta.is_dir() => return Err(UnpackError::NotADirectory(dir.to_owned())),
            Ok(_) => {
                exists = true;
                if !options.force {
                    let mut entries = std::fs::read_dir(dir).map_err(io_err(dir))?;
                    if entries.next().is_some() {
                        return Err(UnpackError::NotEmpty(dir.to_owned()));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(dir)(e)),
        }
        let mut plan = WritePlan::default();
        for file in &self.files {
            let mut path = dir.to_owned();
            path.extend(file.path.split('/'));
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    plan.create.push(file.path.clone());
                }
                Err(e) => return Err(io_err(&path)(e)),
                Ok(meta) if !meta.is_file() => {
                    return Err(io_err(&path)(io::Error::other(
                        "exists and is not a regular file",
                    )));
                }
                Ok(_) => {
                    let current = std::fs::read(&path).map_err(io_err(&path))?;
                    if current == file.data {
                        plan.unchanged.push(file.path.clone());
                    } else {
                        plan.change.push(file.path.clone());
                    }
                }
            }
        }
        if options.clean && exists {
            let keep: std::collections::HashSet<&str> =
                self.files.iter().map(|f| f.path.as_str()).collect();
            for name in previous_files(dir)? {
                if keep.contains(name.as_str()) {
                    continue;
                }
                let mut path = dir.to_owned();
                path.extend(name.split('/'));
                // Only regular files: a missing one is already gone, and a
                // directory or link is not something unpack created.
                if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                    plan.remove.push(name);
                }
            }
        }
        Ok(plan)
    }

    /// Carries out a plan made by [`Unpacked::plan`] for the same `dir`:
    /// creates and overwrites files, then removes the stale ones and the
    /// directories that this leaves empty.
    pub fn apply(&self, dir: &Path, plan: &WritePlan) -> Result<(), UnpackError> {
        let io_err = |path: &Path| {
            let path = path.to_owned();
            move |source| UnpackError::Io { path, source }
        };
        std::fs::create_dir_all(dir).map_err(io_err(dir))?;
        for file in &self.files {
            if plan.unchanged.contains(&file.path) {
                continue;
            }
            let mut path = dir.to_owned();
            path.extend(file.path.split('/'));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(io_err(parent))?;
            }
            let mut open = std::fs::OpenOptions::new();
            open.write(true);
            if plan.create.contains(&file.path) {
                open.create_new(true);
            } else {
                open.truncate(true);
            }
            let mut out = open.open(&path).map_err(io_err(&path))?;
            out.write_all(&file.data).map_err(io_err(&path))?;
        }
        for name in &plan.remove {
            let mut path = dir.to_owned();
            path.extend(name.split('/'));
            std::fs::remove_file(&path).map_err(io_err(&path))?;
            // Remove directories that became empty, never the root. One that
            // still holds anything fails to go and stays.
            let mut parent = path.parent();
            while let Some(p) = parent.filter(|p| *p != dir) {
                if std::fs::remove_dir(p).is_err() {
                    break;
                }
                parent = p.parent();
            }
        }
        Ok(())
    }
}

/// Unpacks a save. `eol` is the line ending of every file written (the native
/// one in the command line tool).
pub fn unpack(save_bytes: &[u8], eol: Eol) -> Result<Unpacked, UnpackError> {
    let save = Save::from_slice(save_bytes)?;
    let layout_eol = eol::detect_layout(save_bytes);
    let Value::Object(root) = save.into_value() else {
        unreachable!("Save invariant: top level is an object");
    };
    let mut ctx = Ctx {
        eol,
        files: Vec::new(),
        objects: 0,
        eol_records: Map::new(),
        mixed_eol: Vec::new(),
        pretty_state: Vec::new(),
    };
    ctx.unpack_root(root);
    Ok(ctx.finish(layout_eol))
}

struct Ctx {
    eol: Eol,
    files: Vec<OutputFile>,
    objects: usize,
    eol_records: Map<String, Value>,
    mixed_eol: Vec<String>,
    pretty_state: Vec<String>,
}

fn is_non_empty_string(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(s)) if !s.is_empty())
}

impl Ctx {
    fn push(&mut self, path: String, data: Vec<u8>) -> usize {
        self.files.push(OutputFile { path, data });
        self.files.len() - 1
    }

    fn json_file(&mut self, path: String, value: &Value) {
        let data = to_vec_with(value, self.eol, true);
        self.push(path, data);
    }

    /// Records the line ending style of the original text of `path`.
    fn record_eol(&mut self, path: &str, text: &str) {
        if let Some(style) = eol::detect(text) {
            self.eol_records
                .insert(path.to_owned(), json!(style.eol.name()));
            if style.mixed {
                self.mixed_eol.push(path.to_owned());
            }
        }
    }

    /// Writes `text` as a text file with converted line breaks.
    fn text_file(&mut self, path: String, text: &str) {
        self.record_eol(&path, text);
        let data = eol::convert(text, self.eol);
        self.push(path, data);
    }

    fn unpack_root(&mut self, mut map: Map<String, Value>) {
        let slot = self.push("save.json".to_owned(), Vec::new());
        self.extract_texts(&mut map, "");
        self.extract_notebook(&mut map);
        self.extract_list(&mut map, "ObjectStates", "objects", "");
        self.files[slot].data = to_vec_with(&Value::Object(map), self.eol, true);
    }

    fn unpack_object(&mut self, dir: &str, mut map: Map<String, Value>) {
        self.objects += 1;
        let slot = self.push(format!("{dir}object.json"), Vec::new());
        self.extract_texts(&mut map, dir);
        self.extract_list(&mut map, "ContainedObjects", "contents", dir);
        self.extract_states(&mut map, dir);
        self.extract_list(&mut map, "ChildObjects", "children", dir);
        self.files[slot].data = to_vec_with(&Value::Object(map), self.eol, true);
    }

    /// Replaces the value at `key` with `marker` (keeping the key position)
    /// and returns the old value.
    fn replace_with_marker(map: &mut Map<String, Value>, key: &str, marker: String) -> Value {
        map.insert(key.to_owned(), Value::String(marker))
            .expect("the key was checked to exist")
    }

    /// `LuaScript`, `XmlUI` and `LuaScriptState` of the root or an object.
    fn extract_texts(&mut self, map: &mut Map<String, Value>, dir: &str) {
        for (key, file) in [("LuaScript", "script.lua"), ("XmlUI", "ui.xml")] {
            if is_non_empty_string(map.get(key)) {
                let Value::String(text) =
                    Self::replace_with_marker(map, key, format!("file:{file}"))
                else {
                    unreachable!("checked to be a string");
                };
                self.text_file(format!("{dir}{file}"), &text);
            }
        }
        if is_non_empty_string(map.get("LuaScriptState")) {
            let Some(Value::String(text)) = map.get("LuaScriptState").cloned() else {
                unreachable!("checked to be a string");
            };
            let kind = classify_state(&text);
            let file = match kind {
                StateKind::Pretty(_) | StateKind::Json => "state.json",
                StateKind::Text => "state.txt",
            };
            Self::replace_with_marker(map, "LuaScriptState", format!("file:{file}"));
            let path = format!("{dir}{file}");
            match kind {
                StateKind::Pretty(value) => {
                    self.record_eol(&path, &text);
                    self.pretty_state.push(path.clone());
                    self.json_file(path, &value);
                }
                StateKind::Json | StateKind::Text => self.text_file(path, &text),
            }
        }
    }

    /// `TabStates` of the root: the `body` of each entry.
    fn extract_notebook(&mut self, map: &mut Map<String, Value>) {
        let Some(Value::Object(tabs)) = map.get_mut("TabStates") else {
            return;
        };
        let mut names = Siblings::empty();
        let mut pages: Vec<(String, String)> = Vec::new();
        for (key, entry) in tabs.iter_mut() {
            let Value::Object(entry) = entry else {
                continue;
            };
            if !is_non_empty_string(entry.get("body")) {
                continue;
            }
            let title = match entry.get("title") {
                Some(Value::String(t)) => Some(t.as_str()),
                _ => None,
            };
            let name = names.claim(&names::notebook_stem(key, title), ".txt");
            let path = format!("notebook/{name}");
            let Value::String(body) =
                Self::replace_with_marker(entry, "body", format!("file:{path}"))
            else {
                unreachable!("checked to be a string");
            };
            pages.push((path, body));
        }
        for (path, body) in pages {
            self.text_file(path, &body);
        }
    }

    /// `ObjectStates`, `ContainedObjects` or `ChildObjects`: a non-empty array
    /// of objects becomes a directory `sub` with an `index.json`.
    fn extract_list(&mut self, map: &mut Map<String, Value>, key: &str, sub: &str, dir: &str) {
        let extractable = matches!(
            map.get(key),
            Some(Value::Array(items)) if !items.is_empty() && items.iter().all(Value::is_object)
        );
        if !extractable {
            return;
        }
        let Value::Array(items) = Self::replace_with_marker(map, key, format!("dir:{sub}")) else {
            unreachable!("checked to be an array");
        };
        let base = format!("{dir}{sub}/");
        let mut siblings = Siblings::with_index();
        let mut children = Vec::with_capacity(items.len());
        for item in items {
            let Value::Object(child) = item else {
                unreachable!("checked to be objects");
            };
            let name = siblings.claim(&names::object_dir_name(&child, None), "");
            children.push((name, child));
        }
        let index: Vec<Value> = children.iter().map(|(n, _)| json!(n)).collect();
        self.json_file(format!("{base}index.json"), &Value::Array(index));
        for (name, child) in children {
            self.unpack_object(&format!("{base}{name}/"), child);
        }
    }

    /// `States`: a non-empty map of objects becomes `states/` with an
    /// `index.json` of keys and directories.
    fn extract_states(&mut self, map: &mut Map<String, Value>, dir: &str) {
        let extractable = matches!(
            map.get("States"),
            Some(Value::Object(states)) if !states.is_empty() && states.values().all(Value::is_object)
        );
        if !extractable {
            return;
        }
        let Value::Object(states) =
            Self::replace_with_marker(map, "States", "dir:states".to_owned())
        else {
            unreachable!("checked to be an object");
        };
        let base = format!("{dir}states/");
        let mut siblings = Siblings::with_index();
        let mut children = Vec::with_capacity(states.len());
        for (key, value) in states {
            let Value::Object(child) = value else {
                unreachable!("checked to be objects");
            };
            let name = siblings.claim(&names::object_dir_name(&child, Some(&key)), "");
            children.push((key, name, child));
        }
        let index: Vec<Value> = children
            .iter()
            .map(|(key, name, _)| json!({"key": key, "dir": name}))
            .collect();
        self.json_file(format!("{base}index.json"), &Value::Array(index));
        for (_, name, child) in children {
            self.unpack_object(&format!("{base}{name}/"), child);
        }
    }

    fn finish(mut self, layout_eol: Option<Eol>) -> Unpacked {
        let listed: Vec<Value> = self.files.iter().map(|f| json!(f.path)).collect();
        let mut meta = Map::new();
        meta.insert("format".to_owned(), json!(FORMAT_VERSION));
        meta.insert("ttskit".to_owned(), json!(env!("CARGO_PKG_VERSION")));
        if let Some(layout) = layout_eol {
            meta.insert("layout_eol".to_owned(), json!(layout.name()));
        }
        meta.insert("files".to_owned(), Value::Array(listed));
        meta.insert("eol".to_owned(), Value::Object(self.eol_records.clone()));
        meta.insert("mixed_eol".to_owned(), json!(self.mixed_eol));
        meta.insert("pretty_state".to_owned(), json!(self.pretty_state));
        self.json_file(METADATA_PATH.to_owned(), &Value::Object(meta));
        let report = UnpackReport {
            objects: self.objects,
            files: self.files.len(),
            mixed_eol: self.mixed_eol,
        };
        Unpacked {
            files: self.files,
            report,
        }
    }
}
