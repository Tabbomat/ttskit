//! Packing an unpacked directory (`docs/pack-layout.md`) back into a save.
//!
//! [`pack`] reads the tree, resolves the markers (`file:script.lua`,
//! `dir:contents`, ...) and returns the bytes of the save. It never writes
//! anything; all errors are found before the caller writes the result.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::save::write::{Eol, to_vec_with};
use crate::unpack::{FORMAT_VERSION, METADATA_PATH, compact_state, eol};

/// Errors from packing. Each one names the offending path.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// The input path is not a directory.
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    /// A file or directory could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// A JSON file is malformed (the message holds line and column).
    #[error("{}: invalid JSON: {source}", path.display())]
    Json {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: serde_json::Error,
    },
    /// A text file is not valid UTF-8.
    #[error("{}: not valid UTF-8", .0.display())]
    NotUtf8(PathBuf),
    /// The metadata was written by an unknown layout version.
    #[error(
        "{}: unknown layout format {found}; this ttskit reads format {FORMAT_VERSION}",
        path.display()
    )]
    UnknownFormat {
        /// The metadata file.
        path: PathBuf,
        /// The value found.
        found: String,
    },
    /// The tree does not follow the layout.
    #[error("{}: {message}", path.display())]
    Layout {
        /// The file or directory at fault.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
}

/// Options for [`pack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackOptions {
    /// Keep the line endings recorded at unpack (`--keep-eol`) instead of
    /// writing LF everywhere.
    pub keep_eol: bool,
    /// The line ending used by `keep_eol` where nothing was recorded.
    pub native: Eol,
}

impl PackOptions {
    /// Options with the native line ending of this system.
    pub fn new(keep_eol: bool) -> PackOptions {
        PackOptions {
            keep_eol,
            native: Eol::native(),
        }
    }
}

/// The result of [`pack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packed {
    /// The save, ready to be written to a file.
    pub bytes: Vec<u8>,
    /// Number of object directories read, at any depth.
    pub objects: usize,
    /// Files and directories that were ignored.
    pub warnings: Vec<String>,
}

/// What `.ttskit/unpack.json` tells pack. Everything is optional.
#[derive(Default)]
struct Meta {
    layout_eol: Option<Eol>,
    eol: HashMap<String, Eol>,
    pretty_state: HashSet<String>,
}

fn eol_from_name(name: &str) -> Option<Eol> {
    match name {
        "crlf" => Some(Eol::Crlf),
        "lf" => Some(Eol::Lf),
        _ => None,
    }
}

/// Packs the tree in `dir` into a save.
pub fn pack(dir: &Path, options: &PackOptions) -> Result<Packed, PackError> {
    if !dir.is_dir() {
        return Err(PackError::NotADirectory(dir.to_owned()));
    }
    let mut packer = Packer {
        root: dir.to_owned(),
        options: *options,
        meta: Meta::default(),
        warnings: Vec::new(),
        objects: 0,
    };
    packer.read_meta()?;
    let root = packer.object("", "save.json", true)?;
    let layout = if options.keep_eol {
        packer.meta.layout_eol.unwrap_or(options.native)
    } else {
        Eol::Lf
    };
    Ok(Packed {
        bytes: to_vec_with(&Value::Object(root), layout, false),
        objects: packer.objects,
        warnings: packer.warnings,
    })
}

struct Packer {
    root: PathBuf,
    options: PackOptions,
    meta: Meta,
    warnings: Vec<String>,
    objects: usize,
}

/// How a field of `save.json` / `object.json` relates to its file or directory.
enum Slot<'a> {
    /// A marker; holds the part after the prefix.
    Marker(&'a str),
    /// An empty value (`""`, `[]`, `{}`) that unpack leaves inline.
    Empty,
    /// Any other value, to be taken literally.
    Literal,
    /// No such key.
    Absent,
}

fn classify<'a>(value: Option<&'a Value>, prefix: &str, is_empty: fn(&Value) -> bool) -> Slot<'a> {
    match value {
        None => Slot::Absent,
        Some(Value::String(s)) if s.starts_with(prefix) => Slot::Marker(&s[prefix.len()..]),
        Some(v) if is_empty(v) => Slot::Empty,
        Some(_) => Slot::Literal,
    }
}

fn empty_string(v: &Value) -> bool {
    matches!(v, Value::String(s) if s.is_empty())
}

fn empty_array(v: &Value) -> bool {
    matches!(v, Value::Array(a) if a.is_empty())
}

fn empty_object(v: &Value) -> bool {
    matches!(v, Value::Object(m) if m.is_empty())
}

/// Converts line breaks of inline strings to LF (the default pack output).
fn normalize_inline(value: &mut Value) {
    match value {
        Value::String(s) if s.contains("\r\n") => {
            *s = String::from_utf8(eol::convert(s, Eol::Lf))
                .expect("converting line breaks keeps UTF-8");
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_inline),
        Value::Object(map) => map.values_mut().for_each(normalize_inline),
        _ => {}
    }
}

/// A name from an `index.json` must be a single plain path component.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
        && !name.eq_ignore_ascii_case("index.json")
}

impl Packer {
    fn disk(&self, rel: &str) -> PathBuf {
        let mut path = self.root.clone();
        path.extend(rel.split('/').filter(|p| !p.is_empty()));
        path
    }

    fn layout_err(&self, rel: &str, message: String) -> PackError {
        PackError::Layout {
            path: self.disk(rel),
            message,
        }
    }

    fn read_bytes(&self, rel: &str) -> Result<Vec<u8>, PackError> {
        let path = self.disk(rel);
        std::fs::read(&path).map_err(|source| PackError::Io { path, source })
    }

    fn read_json(&self, rel: &str) -> Result<Value, PackError> {
        let bytes = self.read_bytes(rel)?;
        let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        serde_json::from_slice(body).map_err(|source| PackError::Json {
            path: self.disk(rel),
            source,
        })
    }

    /// Reads a UTF-8 file without a BOM, as a string with the line breaks
    /// this run writes: LF by default, the recorded style with `--keep-eol`.
    fn read_text(&self, rel: &str) -> Result<String, PackError> {
        let bytes = self.read_bytes(rel)?;
        let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        let text = std::str::from_utf8(body).map_err(|_| PackError::NotUtf8(self.disk(rel)))?;
        let style = if self.options.keep_eol {
            self.meta
                .eol
                .get(rel)
                .copied()
                .unwrap_or(self.options.native)
        } else {
            Eol::Lf
        };
        Ok(String::from_utf8(eol::convert(text, style))
            .expect("converting line breaks keeps UTF-8"))
    }

    fn read_meta(&mut self) -> Result<(), PackError> {
        if !self.disk(METADATA_PATH).is_file() {
            return Ok(());
        }
        let value = self.read_json(METADATA_PATH)?;
        let Value::Object(map) = value else {
            return Err(self.layout_err(METADATA_PATH, "must contain a JSON object".to_owned()));
        };
        match map.get("format") {
            None => {
                return Err(self.layout_err(METADATA_PATH, "missing `format`".to_owned()));
            }
            Some(v) if v.as_u64() == Some(u64::from(FORMAT_VERSION)) => {}
            Some(v) => {
                return Err(PackError::UnknownFormat {
                    path: self.disk(METADATA_PATH),
                    found: v.to_string(),
                });
            }
        }
        self.meta.layout_eol = map
            .get("layout_eol")
            .and_then(Value::as_str)
            .and_then(eol_from_name);
        if let Some(Value::Object(records)) = map.get("eol") {
            for (path, style) in records {
                if let Some(style) = style.as_str().and_then(eol_from_name) {
                    self.meta.eol.insert(path.clone(), style);
                }
            }
        }
        if let Some(Value::Array(paths)) = map.get("pretty_state") {
            self.meta
                .pretty_state
                .extend(paths.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        Ok(())
    }

    fn warn(&mut self, rel: &str, what: &str) {
        self.warnings.push(format!("ignored unknown {what} {rel}"));
    }

    /// The names of the entries of `rel`, sorted, without hidden ones.
    fn entries(&self, rel: &str) -> Result<Vec<(String, bool)>, PackError> {
        let path = self.disk(rel);
        let io_err = |source| PackError::Io {
            path: path.clone(),
            source,
        };
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&path).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = entry.path().is_dir();
            out.push((name, is_dir));
        }
        out.sort();
        Ok(out)
    }

    /// Warns about every entry of the directory `dir` that is not in `known`.
    fn warn_unknown(&mut self, dir: &str, known: &[&str]) -> Result<(), PackError> {
        for (name, is_dir) in self.entries(dir)? {
            if !known.contains(&name.as_str()) {
                let what = if is_dir { "directory" } else { "file" };
                self.warn(&format!("{dir}{name}"), what);
            }
        }
        Ok(())
    }

    /// Which of `names` exists as a file (or directory) in `dir`.
    fn existing(
        &self,
        dir: &str,
        names: &[&'static str],
        want_dir: bool,
    ) -> Result<Option<&'static str>, PackError> {
        let found: Vec<&'static str> = names
            .iter()
            .copied()
            .filter(|n| {
                let path = self.disk(&format!("{dir}{n}"));
                if want_dir {
                    path.is_dir()
                } else {
                    path.is_file()
                }
            })
            .collect();
        match found.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one)),
            _ => Err(self.layout_err(
                dir,
                format!("{} exist side by side; keep only one", found.join(" and ")),
            )),
        }
    }

    /// Applies the editing rules of the layout to one field of `map`.
    ///
    /// `names` are the allowed marker targets, `existing` the one that exists
    /// on disk, `load` reads it. `json` is the file holding `map`, for errors.
    #[allow(clippy::too_many_arguments)]
    fn resolve_field(
        &mut self,
        map: &mut Map<String, Value>,
        key: &str,
        prefix: &str,
        names: &[&str],
        existing: Option<&str>,
        is_empty: fn(&Value) -> bool,
        json: &str,
        load: &mut dyn FnMut(&mut Packer, &str) -> Result<Value, PackError>,
    ) -> Result<(), PackError> {
        let dir = &json[..json.rfind('/').map_or(0, |i| i + 1)];
        let value = match classify(map.get(key), prefix, is_empty) {
            Slot::Marker(name) => {
                if !names.contains(&name) {
                    let allowed: Vec<String> =
                        names.iter().map(|n| format!("`{prefix}{n}`")).collect();
                    return Err(self.layout_err(
                        json,
                        format!(
                            "`{key}` holds `{prefix}{name}`, which is not allowed here; expected {}",
                            allowed.join(" or ")
                        ),
                    ));
                }
                match existing {
                    Some(found) if found == name => load(self, &format!("{dir}{found}"))?,
                    Some(found) => {
                        return Err(self.layout_err(
                            json,
                            format!(
                                "`{key}` names `{prefix}{name}`, but `{dir}{found}` exists instead"
                            ),
                        ));
                    }
                    None => {
                        map.shift_remove(key);
                        return Ok(());
                    }
                }
            }
            Slot::Empty | Slot::Absent => match existing {
                Some(found) => load(self, &format!("{dir}{found}"))?,
                None => return Ok(()),
            },
            Slot::Literal => match existing {
                Some(found) => {
                    return Err(self.layout_err(
                        json,
                        format!(
                            "`{key}` holds a value that is not a marker, but `{dir}{found}` exists; \
                             set it to `{prefix}{found}` or remove `{found}`"
                        ),
                    ));
                }
                None => return Ok(()),
            },
        };
        // Keeps the position of an existing key, appends a new one.
        map.insert(key.to_owned(), value);
        Ok(())
    }

    fn text_fields(
        &mut self,
        map: &mut Map<String, Value>,
        dir: &str,
        json: &str,
    ) -> Result<(), PackError> {
        for (key, file) in [("LuaScript", "script.lua"), ("XmlUI", "ui.xml")] {
            let existing = self.existing(dir, &[file], false)?;
            self.resolve_field(
                map,
                key,
                "file:",
                &[file],
                existing,
                empty_string,
                json,
                &mut |p, rel| p.read_text(rel).map(Value::String),
            )?;
        }
        let existing = self.existing(dir, &["state.json", "state.txt"], false)?;
        self.resolve_field(
            map,
            "LuaScriptState",
            "file:",
            &["state.json", "state.txt"],
            existing,
            empty_string,
            json,
            &mut |p, rel| {
                if p.meta.pretty_state.contains(rel) {
                    let value = p.read_json(rel)?;
                    Ok(Value::String(compact_state(&value)))
                } else {
                    p.read_text(rel).map(Value::String)
                }
            },
        )
    }

    /// Reads `save.json` (root) or `object.json` in `dir` and resolves it.
    fn object(
        &mut self,
        dir: &str,
        json_name: &str,
        root: bool,
    ) -> Result<Map<String, Value>, PackError> {
        let json = format!("{dir}{json_name}");
        let mut value = self.read_json(&json)?;
        if !self.options.keep_eol {
            normalize_inline(&mut value);
        }
        let Value::Object(mut map) = value else {
            return Err(self.layout_err(&json, "must contain a JSON object".to_owned()));
        };
        self.objects += usize::from(!root);
        self.text_fields(&mut map, dir, &json)?;
        let mut known = vec![json_name, "script.lua", "ui.xml", "state.json", "state.txt"];
        if root {
            self.notebook(&mut map)?;
            self.list_field(&mut map, dir, "ObjectStates", "objects", &json)?;
            known.extend(["objects", "notebook"]);
        } else {
            self.list_field(&mut map, dir, "ContainedObjects", "contents", &json)?;
            self.states_field(&mut map, dir, &json)?;
            self.list_field(&mut map, dir, "ChildObjects", "children", &json)?;
            known.extend(["contents", "states", "children"]);
        }
        self.warn_unknown(dir, &known)?;
        Ok(map)
    }

    fn list_field(
        &mut self,
        map: &mut Map<String, Value>,
        dir: &str,
        key: &str,
        sub: &'static str,
        json: &str,
    ) -> Result<(), PackError> {
        let existing = self.existing(dir, &[sub], true)?;
        self.resolve_field(
            map,
            key,
            "dir:",
            &[sub],
            existing,
            empty_array,
            json,
            &mut |p, rel| p.load_list(&format!("{rel}/")),
        )
    }

    fn states_field(
        &mut self,
        map: &mut Map<String, Value>,
        dir: &str,
        json: &str,
    ) -> Result<(), PackError> {
        let existing = self.existing(dir, &["states"], true)?;
        self.resolve_field(
            map,
            "States",
            "dir:",
            &["states"],
            existing,
            empty_object,
            json,
            &mut |p, rel| p.load_states(&format!("{rel}/")),
        )
    }

    /// Checks a directory against its index: every listed directory exists
    /// and every directory is listed.
    fn check_index(&mut self, base: &str, names: &[String]) -> Result<(), PackError> {
        let index = format!("{base}index.json");
        let mut seen = HashSet::new();
        for name in names {
            if !is_plain_name(name) {
                return Err(
                    self.layout_err(&index, format!("`{name}` is not a valid directory name"))
                );
            }
            if !seen.insert(name.as_str()) {
                return Err(self.layout_err(&index, format!("`{name}` is listed twice")));
            }
            if !self.disk(&format!("{base}{name}")).is_dir() {
                return Err(self.layout_err(
                    &index,
                    format!("lists `{name}`, but the directory {base}{name} does not exist"),
                ));
            }
        }
        for (name, is_dir) in self.entries(base)? {
            if name == "index.json" || seen.contains(name.as_str()) {
                continue;
            }
            if is_dir {
                return Err(self.layout_err(
                    &format!("{base}{name}"),
                    "directory is not listed in index.json; add it there or remove it".to_owned(),
                ));
            }
            self.warn(&format!("{base}{name}"), "file");
        }
        Ok(())
    }

    fn load_list(&mut self, base: &str) -> Result<Value, PackError> {
        let index = format!("{base}index.json");
        let Value::Array(entries) = self.read_json(&index)? else {
            return Err(
                self.layout_err(&index, "must be a JSON array of directory names".to_owned())
            );
        };
        let mut names = Vec::with_capacity(entries.len());
        for entry in &entries {
            let Value::String(name) = entry else {
                return Err(self.layout_err(&index, format!("entry {entry} is not a string")));
            };
            names.push(name.clone());
        }
        self.check_index(base, &names)?;
        let mut items = Vec::with_capacity(names.len());
        for name in &names {
            items.push(Value::Object(self.object(
                &format!("{base}{name}/"),
                "object.json",
                false,
            )?));
        }
        Ok(Value::Array(items))
    }

    fn load_states(&mut self, base: &str) -> Result<Value, PackError> {
        let index = format!("{base}index.json");
        let Value::Array(entries) = self.read_json(&index)? else {
            return Err(self.layout_err(
                &index,
                "must be a JSON array of {\"key\", \"dir\"} entries".to_owned(),
            ));
        };
        let mut pairs: Vec<(String, String)> = Vec::with_capacity(entries.len());
        for entry in &entries {
            let field = |name: &str| entry.get(name).and_then(Value::as_str).map(str::to_owned);
            let (Some(key), Some(dir)) = (field("key"), field("dir")) else {
                return Err(self.layout_err(
                    &index,
                    format!("entry {entry} needs string fields `key` and `dir`"),
                ));
            };
            if pairs.iter().any(|(k, _)| *k == key) {
                return Err(self.layout_err(&index, format!("key `{key}` is listed twice")));
            }
            pairs.push((key, dir));
        }
        let dirs: Vec<String> = pairs.iter().map(|(_, d)| d.clone()).collect();
        self.check_index(base, &dirs)?;
        let mut states = Map::new();
        for (key, dir) in pairs {
            let object = self.object(&format!("{base}{dir}/"), "object.json", false)?;
            states.insert(key, Value::Object(object));
        }
        Ok(Value::Object(states))
    }

    /// The `body` markers of `TabStates` and the files in `notebook/`.
    fn notebook(&mut self, map: &mut Map<String, Value>) -> Result<(), PackError> {
        let mut used: HashSet<String> = HashSet::new();
        if let Some(Value::Object(tabs)) = map.get_mut("TabStates") {
            for entry in tabs.values_mut() {
                let Value::Object(entry) = entry else {
                    continue;
                };
                let Some(Value::String(marker)) = entry.get("body") else {
                    continue;
                };
                let Some(path) = marker.strip_prefix("file:").map(str::to_owned) else {
                    continue;
                };
                let name = path
                    .strip_prefix("notebook/")
                    .filter(|n| is_plain_name(n) && n.len() > ".txt".len() && n.ends_with(".txt"))
                    .map(str::to_owned);
                let Some(name) = name else {
                    return Err(self.layout_err(
                        "save.json",
                        format!(
                            "notebook body marker `file:{path}` must be `file:notebook/<name>.txt`"
                        ),
                    ));
                };
                let rel = format!("notebook/{name}");
                if self.disk(&rel).is_file() {
                    let text = self.read_text(&rel)?;
                    entry.insert("body".to_owned(), Value::String(text));
                    used.insert(name);
                } else {
                    entry.shift_remove("body");
                }
            }
        }
        if self.disk("notebook").is_dir() {
            for (name, is_dir) in self.entries("notebook")? {
                if !used.contains(&name) {
                    self.warn(
                        &format!("notebook/{name}"),
                        if is_dir { "directory" } else { "file" },
                    );
                }
            }
        }
        Ok(())
    }
}
