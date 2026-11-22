//! URL field rules: `empty-url`, `invalid-url`, `insecure-url`,
//! `legacy-steam-url` and `local-url`.
//!
//! A URL field is a string value under a key whose name ends in `url`
//! (ASCII case-insensitive), for example `FaceURL` or `PDFUrl`.
//!
//! Two special value shapes are handled before the rules apply. A value like
//! `<Name>file.png` is a placeholder for a game-internal asset and is skipped.
//! A value that starts with a language marker such as `{de}` holds one URL per
//! language (`{de}URL{en}URL`); each segment is checked on its own and findings
//! name the marker. A leading `{verifycache}` flag is dropped first, also at
//! the start of each segment.

use super::RuleInfo;
use crate::config::Severity;
use crate::ctx::{Ctx, Field};

pub(super) const EMPTY: RuleInfo = RuleInfo {
    id: "empty-url",
    default: Severity::Error,
    summary: "a required URL field is empty",
};

pub(super) const INVALID: RuleInfo = RuleInfo {
    id: "invalid-url",
    default: Severity::Error,
    summary: "a URL field is not a valid http(s) URL",
};

pub(super) const INSECURE: RuleInfo = RuleInfo {
    id: "insecure-url",
    default: Severity::Warning,
    summary: "a URL field uses http instead of https",
};

pub(super) const LEGACY_STEAM: RuleInfo = RuleInfo {
    id: "legacy-steam-url",
    default: Severity::Info,
    summary: "a URL uses the legacy http://cloud-3.steamusercontent.com host; `ttskit save rewrite-urls --normalize-steam-hosts` fixes it",
};

pub(super) const LOCAL: RuleInfo = RuleInfo {
    id: "local-url",
    default: Severity::Error,
    summary: "a URL field points at a file on the local machine",
};

pub(super) const ALL: [RuleInfo; 5] = [EMPTY, INVALID, INSECURE, LEGACY_STEAM, LOCAL];

/// The Steam host older saves store with an `http` scheme.
const LEGACY_STEAM_HOST: &str = "cloud-3.steamusercontent.com";

/// Keys that must not be empty. Other URL keys (`DiffuseURL`, `NormalURL`,
/// `ColliderURL`, `LutURL`, the `Secondary` ones, ...) are legitimately empty.
const REQUIRED: [&str; 6] = [
    "FaceURL",
    "BackURL",
    "ImageURL",
    "MeshURL",
    "AssetbundleURL",
    "PDFUrl",
];

/// Local-path markers, compared against the lowercased value with `\` turned
/// into `/`.
const LOCAL_DIRS: [&str; 4] = [
    "mods/images",
    "mods/models",
    "mods/assetbundles",
    "mods/pdf",
];

fn is_url_key(key: &str) -> bool {
    let b = key.as_bytes();
    b.len() >= 3 && b[b.len() - 3..].eq_ignore_ascii_case(b"url")
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Whether `value` points at the local machine.
fn is_local(value: &str) -> bool {
    let v = value.trim();
    let b = v.as_bytes();
    if starts_with_ci(v, "file:")
        || v.starts_with("\\\\")
        || v.starts_with(['/', '~'])
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'\\' | b'/'))
    {
        return true;
    }
    let lower = v.to_ascii_lowercase().replace('\\', "/");
    (lower.contains("tabletop simulator") && v.contains(['/', '\\']))
        || LOCAL_DIRS.iter().any(|d| lower.contains(d))
}

/// Host of `rest` (the text after `://`): no userinfo, no port.
fn host_of(rest: &str) -> &str {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host_port.starts_with('[') {
        host_port
            .find(']')
            .map_or(host_port, |end| &host_port[..=end])
    } else {
        host_port.split(':').next().unwrap_or("")
    }
}

/// Whether a non-local, non-empty value is not a usable http(s) URL.
fn is_invalid(value: &str) -> bool {
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return true;
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return true;
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return true;
    }
    host_of(rest).is_empty()
}

/// Whether `s` starts with a `<Name>` placeholder: a non-empty name without
/// `>`, `<` or whitespace.
fn is_placeholder(s: &str) -> bool {
    let Some(rest) = s.trim_start().strip_prefix('<') else {
        return false;
    };
    rest.split_once('>').is_some_and(|(name, _)| {
        !name.is_empty() && !name.contains(char::is_whitespace) && !name.contains('<')
    })
}

/// Length of the language marker (`{` + 2-3 ASCII letters, `-` or `_` + `}`)
/// at the start of `s`, if any.
fn marker_len(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('{')?;
    let end = rest.find('}')?;
    let name = &rest[..end];
    ((2..=3).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'_'))
    .then_some(end + 2)
}

/// Splits a localized value into `(marker, text)` segments. Returns `None`
/// unless the value starts with a marker.
fn localized_segments(value: &str) -> Option<Vec<(&str, &str)>> {
    let mut len = marker_len(value)?;
    let mut segments = Vec::new();
    let mut start = 0;
    loop {
        let marker = &value[start..start + len];
        let text_start = start + len;
        let mut next = text_start;
        let mut next_len = None;
        while next < value.len() {
            if value.as_bytes()[next] == b'{'
                && let Some(l) = marker_len(&value[next..])
            {
                next_len = Some(l);
                break;
            }
            next += 1;
        }
        segments.push((marker, &value[text_start..next]));
        match next_len {
            Some(l) => {
                start = next;
                len = l;
            }
            None => return Some(segments),
        }
    }
}

/// Drops a leading `{verifycache}` flag (case-insensitive), which asks the game
/// to re-download the asset and is not part of the URL.
fn strip_verify_cache(s: &str) -> &str {
    const FLAG: &str = "{verifycache}";
    if starts_with_ci(s, FLAG) {
        &s[FLAG.len()..]
    } else {
        s
    }
}

pub(super) fn check(ctx: &mut Ctx<'_>, field: &Field<'_, '_>) {
    if !is_url_key(field.key) {
        return;
    }
    let value = strip_verify_cache(field.value);
    match localized_segments(value) {
        Some(segments) => {
            for (marker, text) in segments {
                check_value(ctx, field, strip_verify_cache(text), Some(marker));
            }
        }
        None => check_value(ctx, field, value, None),
    }
}

/// Applies every rule to one URL, a whole field value or one language segment.
fn check_value(ctx: &mut Ctx<'_>, field: &Field<'_, '_>, value: &str, marker: Option<&str>) {
    let name = field.key;
    let at = marker.map_or_else(String::new, |m| format!(" in segment {m}"));
    if value.trim().is_empty() {
        let required = REQUIRED.iter().any(|k| k.eq_ignore_ascii_case(name))
            || (field.ui_asset_item && name.eq_ignore_ascii_case("url"));
        if required {
            ctx.report(EMPTY.id, field.path, field.owner, || {
                format!("{name} is empty{at}")
            });
        }
        return;
    }
    if is_placeholder(value) {
        return;
    }
    if is_local(value) {
        ctx.report(LOCAL.id, field.path, field.owner, || {
            format!("{name} points at the local machine{at}: {value}")
        });
        return;
    }
    if is_invalid(value) {
        ctx.report(INVALID.id, field.path, field.owner, || {
            format!("{name} is not a valid http(s) URL{at}: {value}")
        });
    }
    if starts_with_ci(value, "http://") {
        let host = host_of(&value["http://".len()..]);
        if host.eq_ignore_ascii_case(LEGACY_STEAM_HOST) {
            ctx.report(LEGACY_STEAM.id, field.path, field.owner, || {
                format!("{name} uses the legacy Steam host{at}: {value}")
            });
        } else {
            ctx.report(INSECURE.id, field.path, field.owner, || {
                format!("{name} uses http instead of https{at}: {value}")
            });
        }
    }
}
