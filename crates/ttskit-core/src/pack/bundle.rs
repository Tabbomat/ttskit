//! Inlining of shared Lua modules and XML includes (`--include`).
//!
//! Lua: `require("Name")` calls are collected from a script, the modules are
//! found in the include directories and everything is wrapped in the bundle
//! format of the Tabletop Simulator editor plugins (`__bundle_register`),
//! which has real `require` semantics at runtime: modules run once, their
//! result is cached and cycles are safe.
//!
//! XML: `<Include src="Name"/>` is replaced by the content of `Name.xml`,
//! recursively.
//!
//! All text handled here uses LF; the caller converts the result.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::PackError;

/// A script that contains this marker is already bundled and left alone.
const BUNDLE_MARKER: &str = "__bundle_register";

const PRELUDE: &str = r#"-- Bundled by ttskit (module registry compatible with the Tabletop Simulator editor plugins)
local __bundle_require, __bundle_loaded, __bundle_register, __bundle_modules = (function(superRequire)
	local loadingPlaceholder = {[{}] = true}

	local register
	local modules = {}

	local require
	local loaded = {}

	register = function(name, body)
		if not modules[name] then
			modules[name] = body
		end
	end

	require = function(name)
		local loadedModule = loaded[name]

		if loadedModule then
			if loadedModule == loadingPlaceholder then
				return nil
			end
		else
			if not modules[name] then
				if not superRequire then
					local identifier = type(name) == 'string' and '"' .. name .. '"' or tostring(name)
					error('Tried to require ' .. identifier .. ', but no such module has been registered')
				else
					return superRequire(name)
				end
			end

			loaded[name] = loadingPlaceholder
			loadedModule = modules[name](require, loaded, register, modules)
			loaded[name] = loadedModule
		end

		return loadedModule
	end

	return require, loaded, register, modules
end)(require)
"#;

const MODULE_ARGS: &str = "require, _LOADED, __bundle_register, __bundle_modules";

fn err(path: &Path, message: String) -> PackError {
    PackError::Bundle {
        path: path.to_owned(),
        message,
    }
}

fn read_utf8(path: &Path) -> Result<String, PackError> {
    let bytes = std::fs::read(path).map_err(|source| PackError::Io {
        path: path.to_owned(),
        source,
    })?;
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let text = std::str::from_utf8(body).map_err(|_| PackError::NotUtf8(path.to_owned()))?;
    Ok(text.replace("\r\n", "\n"))
}

/// Splits a module name into path segments: `.` and `/` both separate
/// directories when `dots` is set, otherwise only `/` does. Names that could
/// leave the include directory are rejected.
fn segments(name: &str, dots: bool) -> Option<Vec<&str>> {
    if name.is_empty() || name.contains(['\\', ':', '\0']) || name.starts_with('/') {
        return None;
    }
    let parts: Vec<&str> = if dots {
        name.split(['.', '/']).collect()
    } else {
        name.split('/').collect()
    };
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return None;
    }
    Some(parts)
}

/// The first existing `<dir>/<name><ext>`, searching directories in order.
fn find(dirs: &[PathBuf], parts: &[&str], exts: &[&str]) -> Option<PathBuf> {
    for dir in dirs {
        for ext in exts {
            let mut path = dir.clone();
            path.extend(parts);
            let mut file = path.into_os_string();
            file.push(ext);
            let path = PathBuf::from(file);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn searched(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------- Lua ----

/// Names of the `require("literal")` calls in `src`, in order, without
/// duplicates. Comments and strings are skipped; `require(name)` with a
/// non-literal argument, `x.require(...)` and `x:require(...)` are not
/// requires of a module file and are ignored.
pub fn find_requires(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut found: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            i += 2;
            if let Some(level) = long_open(b, i) {
                i = skip_long(b, i, level);
            } else {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
        } else if c == b'"' || c == b'\'' {
            i = short_string(b, i).1;
        } else if c == b'[' && long_open(b, i).is_some() {
            i = skip_long(b, i, long_open(b, i).unwrap());
        } else if is_ident_start(c) {
            let start = i;
            while i < b.len() && is_ident(b[i]) {
                i += 1;
            }
            if &src[start..i] == "require"
                && !is_member_access(b, start)
                && let Some((name, end)) = require_arg(src, i)
            {
                if !found.contains(&name) {
                    found.push(name);
                }
                i = end;
            }
        } else {
            i += 1;
        }
    }
    found
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// True when the identifier at `start` is written `x.name` or `x:name`
/// (but not after a `..` concatenation).
fn is_member_access(b: &[u8], start: usize) -> bool {
    let mut j = start;
    while j > 0 && b[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    match j.checked_sub(1).map(|k| b[k]) {
        Some(b':') => true,
        Some(b'.') => !(j >= 2 && b[j - 2] == b'.'),
        _ => false,
    }
}

/// `[` `=`* `[` at `i`: the number of `=`.
fn long_open(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    while b.get(j) == Some(&b'=') {
        j += 1;
    }
    (b.get(j) == Some(&b'[')).then_some(j - i - 1)
}

/// The index after the long bracket opened at `i`.
fn skip_long(b: &[u8], i: usize, level: usize) -> usize {
    let mut j = i + level + 2;
    while j < b.len() {
        if b[j] == b']' {
            let mut k = j + 1;
            while b.get(k) == Some(&b'=') {
                k += 1;
            }
            if k - j - 1 == level && b.get(k) == Some(&b']') {
                return k + 1;
            }
        }
        j += 1;
    }
    b.len()
}

/// A quoted string starting at `i`: its value when it has no escapes, and
/// the index after it.
fn short_string(b: &[u8], i: usize) -> (Option<String>, usize) {
    let quote = b[i];
    let mut j = i + 1;
    let mut plain = true;
    while j < b.len() && b[j] != quote && b[j] != b'\n' {
        if b[j] == b'\\' {
            plain = false;
            j += 1;
        }
        j += 1;
    }
    let end = (j + 1).min(b.len());
    let value = (plain && b.get(j) == Some(&quote))
        .then(|| String::from_utf8_lossy(&b[i + 1..j]).into_owned());
    (value, end)
}

fn skip_space(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// The literal argument of a `require` call whose name ends at `i`.
fn require_arg(src: &str, i: usize) -> Option<(String, usize)> {
    let b = src.as_bytes();
    let mut j = skip_space(b, i);
    let paren = b.get(j) == Some(&b'(');
    if paren {
        j = skip_space(b, j + 1);
    }
    let (name, mut end) = match b.get(j)? {
        b'"' | b'\'' => {
            let (value, end) = short_string(b, j);
            (value?, end)
        }
        b'[' => {
            let level = long_open(b, j)?;
            let end = skip_long(b, j, level);
            let inner = src.get(j + level + 2..end.checked_sub(level + 2)?)?;
            (inner.strip_prefix('\n').unwrap_or(inner).to_owned(), end)
        }
        _ => return None,
    };
    if paren {
        end = skip_space(b, end);
        if b.get(end) != Some(&b')') {
            return None;
        }
        end += 1;
    }
    Some((name, end))
}

struct Module {
    name: String,
    body: String,
}

struct LuaBundle<'a> {
    dirs: &'a [PathBuf],
    modules: Vec<Module>,
    /// Canonical file -> name it is registered under.
    by_file: HashMap<PathBuf, String>,
    registered: HashSet<String>,
}

impl LuaBundle<'_> {
    fn add(&mut self, src: &str, origin: &Path) -> Result<(), PackError> {
        for name in find_requires(src) {
            let missing = || {
                err(
                    origin,
                    format!(
                        "module \"{name}\" is required here but not found as `{}` in the include directories ({})",
                        name.replace('.', "/") + ".lua or .ttslua",
                        searched(self.dirs)
                    ),
                )
            };
            let parts = segments(&name, true).ok_or_else(|| {
                err(
                    origin,
                    format!("module name \"{name}\" is not a valid relative module name"),
                )
            })?;
            let file = find(self.dirs, &parts, &[".lua", ".ttslua"]).ok_or_else(missing)?;
            let canonical = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
            if let Some(existing) = self.by_file.get(&canonical).cloned() {
                // The same file under another spelling (`a.b` and `a/b`).
                if self.registered.insert(name.clone()) {
                    self.modules.push(Module {
                        body: format!("return require(\"{}\")", escape(&existing)),
                        name,
                    });
                }
                continue;
            }
            let text = read_utf8(&file)?;
            self.by_file.insert(canonical, name.clone());
            self.registered.insert(name.clone());
            // Registered before its own requires are followed, so cycles end.
            self.modules.push(Module {
                name,
                body: text.clone(),
            });
            self.add(&text, &file)?;
        }
        Ok(())
    }
}

fn escape(name: &str) -> String {
    name.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Bundles `main` (the script in `origin`) with the modules it requires.
/// Returns `main` unchanged when it requires nothing or is already bundled.
pub fn bundle_lua(main: &str, origin: &Path, dirs: &[PathBuf]) -> Result<String, PackError> {
    if main.contains(BUNDLE_MARKER) {
        return Ok(main.to_owned());
    }
    let mut bundle = LuaBundle {
        dirs,
        modules: Vec::new(),
        by_file: HashMap::new(),
        registered: HashSet::new(),
    };
    bundle.add(main, origin)?;
    if bundle.modules.is_empty() {
        return Ok(main.to_owned());
    }
    let mut out = String::from(PRELUDE);
    for module in &bundle.modules {
        register(&mut out, &module.name, &module.body);
    }
    register(&mut out, "__root", main);
    out.push_str("return __bundle_require(\"__root\")\n");
    Ok(out)
}

fn register(out: &mut String, name: &str, body: &str) {
    out.push_str(&format!(
        "__bundle_register(\"{}\", function({MODULE_ARGS})\n",
        escape(name)
    ));
    out.push_str(body);
    out.push_str("\nend)\n");
}

// ---------------------------------------------------------------- XML ----

/// Replaces every `<Include src="Name"/>` in `src` (the UI in `origin`) with
/// the content of `Name.xml`, recursively. Include cycles are an error.
pub fn bundle_xml(src: &str, origin: &Path, dirs: &[PathBuf]) -> Result<String, PackError> {
    expand_xml(src, origin, dirs, &mut Vec::new())
}

fn expand_xml(
    src: &str,
    origin: &Path,
    dirs: &[PathBuf],
    stack: &mut Vec<PathBuf>,
) -> Result<String, PackError> {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = next_include(rest) {
        out.push_str(&rest[..at.start]);
        let tag = &rest[at.start..at.tag_end];
        let name = attribute(tag, "src").ok_or_else(|| {
            err(
                origin,
                format!("`{tag}` has no src attribute; expected `<Include src=\"Name\"/>`"),
            )
        })?;
        let parts = segments(&name, false).ok_or_else(|| {
            err(
                origin,
                format!("include name \"{name}\" is not a valid relative name"),
            )
        })?;
        let file = find(dirs, &parts, &[".xml"]).ok_or_else(|| {
            err(
                origin,
                format!(
                    "include \"{name}\" is required here but not found as `{name}.xml` in the include directories ({})",
                    searched(dirs)
                ),
            )
        })?;
        let canonical = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
        if let Some(pos) = stack.iter().position(|p| *p == canonical) {
            let mut chain: Vec<String> = stack[pos..]
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            chain.push(canonical.display().to_string());
            return Err(err(
                origin,
                format!("include cycle: {}", chain.join(" -> ")),
            ));
        }
        stack.push(canonical);
        let text = read_utf8(&file)?;
        out.push_str(&expand_xml(&text, &file, dirs, stack)?);
        stack.pop();
        rest = &rest[at.end..];
    }
    out.push_str(rest);
    Ok(out)
}

struct IncludeAt {
    start: usize,
    /// End of the opening tag.
    tag_end: usize,
    /// End of the whole element (after `</Include>` if there is one).
    end: usize,
}

/// The next `<Include .../>` (or `<Include ...></Include>`) outside comments
/// and CDATA sections.
fn next_include(s: &str) -> Option<IncludeAt> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if s[i..].starts_with("<!--") {
            i = s[i..].find("-->").map_or(b.len(), |n| i + n + 3);
        } else if s[i..].starts_with("<![CDATA[") {
            i = s[i..].find("]]>").map_or(b.len(), |n| i + n + 3);
        } else if s[i..].starts_with("<Include")
            && b.get(i + 8)
                .is_some_and(|c| c.is_ascii_whitespace() || *c == b'/' || *c == b'>')
        {
            let tag_end = tag_end(b, i)?;
            let mut end = tag_end;
            if b[tag_end - 2] != b'/' {
                let after = skip_space(b, tag_end);
                if s[after..].starts_with("</Include>") {
                    end = after + "</Include>".len();
                }
            }
            return Some(IncludeAt {
                start: i,
                tag_end,
                end,
            });
        } else {
            i += 1;
        }
    }
    None
}

/// The index after the `>` closing the tag that starts at `i`, skipping
/// quoted attribute values.
fn tag_end(b: &[u8], i: usize) -> Option<usize> {
    let mut quote = None;
    for (j, &c) in b.iter().enumerate().skip(i) {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(c),
            (None, b'>') => return Some(j + 1),
            _ => {}
        }
    }
    None
}

/// The value of attribute `name` in the tag text.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let b = tag.as_bytes();
    let mut i = "<Include".len();
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'>' {
            i += 1;
        }
        let key = &tag[start..i];
        i = skip_space(b, i);
        if b.get(i) != Some(&b'=') {
            if key.is_empty() {
                break;
            }
            continue;
        }
        i = skip_space(b, i + 1);
        let quote = *b.get(i)?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let value_start = i + 1;
        let len = tag[value_start..].find(quote as char)?;
        i = value_start + len + 1;
        if key == name {
            return Some(tag[value_start..value_start + len].to_owned());
        }
    }
    None
}
