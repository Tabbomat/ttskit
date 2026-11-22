//! Script syntax rules: `lua-syntax` for `LuaScript` and `xml-malformed` for
//! `XmlUI`. Scripts are only parsed, never run.

use std::collections::hash_map::Entry;

use super::RuleInfo;
use crate::config::Severity;
use crate::ctx::{Ctx, Field};

pub(super) const LUA: RuleInfo = RuleInfo {
    id: "lua-syntax",
    default: Severity::Warning,
    summary: "a LuaScript has a syntax error by Lua 5.2 grammar (the game also accepts `!=` and `|x| expr` lambdas, which are reported)",
};

pub(super) const XML: RuleInfo = RuleInfo {
    id: "xml-malformed",
    default: Severity::Error,
    summary: "an XmlUI is not well-formed XML",
};

pub(super) const ALL: [RuleInfo; 2] = [LUA, XML];

/// Blanks out the game's own `#include ...` lines, keeping line numbers.
fn blank_includes(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.split_inclusive('\n') {
        if line.trim_start().starts_with("#include") {
            if line.ends_with("\r\n") {
                out.push_str("\r\n");
            } else if line.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    out
}

/// First syntax error of a Lua script as `line:column: message`.
fn lua_error(src: &str) -> Option<String> {
    let blanked;
    let code = if src.contains("#include") {
        blanked = blank_includes(src);
        blanked.as_str()
    } else {
        src
    };
    let errors = full_moon::parse(code).err()?;
    let error = errors.first()?;
    let (start, _) = error.range();
    let text = error.to_string();
    // The library prefixes the position; keep only the description.
    let message = text
        .split_once(": ")
        .map_or(text.as_str(), |(_, rest)| rest)
        .trim();
    // Drop the library's own repeat of the position range.
    let message = message.rsplit_once(" (").map_or(message, |(m, _)| m);
    let message = message.trim_end_matches('.');
    Some(format!("{}:{}: {message}", start.line(), start.character()))
}

/// First well-formedness error of an XML fragment as `line:column: message`.
///
/// `XmlUI` has several top-level elements, so the text is wrapped in a root
/// element placed on its own line; line numbers are corrected for that.
fn xml_error(src: &str) -> Option<String> {
    let mut text = src.replace('\u{feff}', " ");
    let trimmed = text.trim_start();
    if trimmed.starts_with("<?xml")
        && let Some(end) = trimmed.find("?>")
    {
        // Same length, so positions stay valid.
        let start = text.len() - trimmed.len();
        let blank: String = text[start..start + end + 2]
            .chars()
            .map(|c| if c == '\n' || c == '\r' { c } else { ' ' })
            .collect();
        text.replace_range(start..start + end + 2, &blank);
    }
    let wrapped = format!("<root>\n{text}\n</root>");
    let error = roxmltree::Document::parse(&wrapped).err()?;
    let pos = error.pos();
    let full = error.to_string();
    let message = full.rsplit_once(" at ").map_or(full.as_str(), |(m, _)| m);
    Some(format!(
        "{}:{}: {message}",
        pos.row.saturating_sub(1).max(1),
        pos.col
    ))
}

pub(super) fn check<'a>(ctx: &mut Ctx<'a>, field: &Field<'a, '_>) {
    let (info, cache_is_lua, what) = match field.key {
        "LuaScript" => (LUA, true, "has a syntax error"),
        "XmlUI" => (XML, false, "is not well-formed XML"),
        _ => return,
    };
    if field.value.trim().is_empty() || !ctx.enabled(info.id) {
        return;
    }
    let cache = if cache_is_lua {
        &mut ctx.lua_cache
    } else {
        &mut ctx.xml_cache
    };
    let error = match cache.entry(field.value) {
        Entry::Occupied(e) => e.get().clone(),
        Entry::Vacant(e) => {
            let result = if cache_is_lua {
                lua_error(field.value)
            } else {
                xml_error(field.value)
            };
            e.insert(result).clone()
        }
    };
    if let Some(error) = error {
        let name = field.key;
        ctx.report(info.id, field.path, field.owner, || {
            format!("{name} {what} at {error}")
        });
    }
}
