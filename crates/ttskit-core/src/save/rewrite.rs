//! Rewriting asset URLs in a save.
//!
//! [`Save::rewrite_urls`] replaces URL strings from a mapping and reports what
//! it touched. Only the matched string values change; everything else in the
//! value, and so in the written file, stays as it was (see the round-trip
//! notes of [`crate::save`]).
//!
//! # What is rewritten
//!
//! - A string value that is exactly an old URL is replaced as a whole. This is
//!   the default and covers `FaceURL`, `ImageURL`, `MeshURL`, `CustomUIAssets`
//!   and every other URL field, wherever it is.
//! - Inside the script strings (`LuaScript`, `LuaScriptState` and `XmlUI`, at
//!   any depth) URLs are substrings of a longer text. They are always counted
//!   and reported separately, but only replaced with
//!   [`RewriteOptions::in_scripts`]. A URL inside a script is the maximal run
//!   of URL characters after `http://` or `https://`, minus trailing
//!   punctuation (`.,;:!?)'*`). An old URL containing other characters (a
//!   space, a quote, non-ASCII) is never found inside a script. In `XmlUI`
//!   `&amp;` stands for `&`.
//! - Rules never chain: each string is matched once against the original
//!   mapping, so `A -> B` together with `B -> A` swaps them.

use std::collections::HashMap;

use serde_json::Value;

use super::Save;

/// Keys whose string values are scripts or UI text, not URL fields.
const SCRIPT_KEYS: [&str; 3] = ["LuaScript", "LuaScriptState", "XmlUI"];

/// The legacy Steam Cloud URL prefix and the one the game writes now.
/// Only the prefix changes; the rest of the URL is kept as it is.
pub const LEGACY_STEAM_PREFIX: &str = "http://cloud-3.steamusercontent.com/ugc/";
/// The current Steam Cloud URL prefix.
pub const CURRENT_STEAM_PREFIX: &str = "https://steamusercontent-a.akamaihd.net/ugc/";

/// One mapping entry: replace `old` by `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlRule {
    /// The URL to find.
    pub old: String,
    /// The URL to put in its place.
    pub new: String,
    /// Where the rule comes from, for messages: `line 3`, `manifest`, ...
    pub origin: String,
}

/// A mapping that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    /// The same old URL is mapped to two different new URLs.
    #[error(
        "{old} is mapped to two different URLs: {first} ({first_origin}) and {second} ({second_origin})"
    )]
    Conflict {
        /// The old URL.
        old: String,
        /// New URL of the first rule.
        first: String,
        /// Origin of the first rule.
        first_origin: String,
        /// New URL of the second rule.
        second: String,
        /// Origin of the second rule.
        second_origin: String,
    },
}

/// A validated mapping: unique old URLs, in the order they were given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlRules {
    rules: Vec<UrlRule>,
    /// Origins of rules dropped because an identical rule came earlier.
    duplicates: Vec<UrlRule>,
}

impl UrlRules {
    /// Builds the mapping. An old URL given again with the same new URL is
    /// kept once (the repeat is listed in [`UrlRules::duplicates`]); with a
    /// different new URL it is an error.
    pub fn new(rules: Vec<UrlRule>) -> Result<UrlRules, RuleError> {
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut kept: Vec<UrlRule> = Vec::new();
        let mut duplicates = Vec::new();
        for rule in rules {
            match seen.get(&rule.old) {
                None => {
                    seen.insert(rule.old.clone(), kept.len());
                    kept.push(rule);
                }
                Some(&i) if kept[i].new == rule.new => duplicates.push(rule),
                Some(&i) => {
                    return Err(RuleError::Conflict {
                        old: rule.old,
                        first: kept[i].new.clone(),
                        first_origin: kept[i].origin.clone(),
                        second: rule.new,
                        second_origin: rule.origin,
                    });
                }
            }
        }
        Ok(UrlRules {
            rules: kept,
            duplicates,
        })
    }

    /// The rules, without repeats.
    pub fn rules(&self) -> &[UrlRule] {
        &self.rules
    }

    /// Repeated rules that were merged into an earlier identical one.
    pub fn duplicates(&self) -> &[UrlRule] {
        &self.duplicates
    }
}

/// A mapping line that is not valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapProblem {
    /// 1-based line number.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

/// Parses a mapping file: one `old URL<TAB>new URL` per line.
///
/// Blank lines and lines starting with `#` are skipped; a BOM and CRLF line
/// endings are fine. Both columns must be `http://` or `https://` URLs.
/// Every bad line is reported, with its line number.
pub fn parse_map_tsv(text: &str) -> Result<Vec<UrlRule>, Vec<MapProblem>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rules = Vec::new();
    let mut problems = Vec::new();
    for (i, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut problem = |message: &str| {
            problems.push(MapProblem {
                line: i + 1,
                message: message.to_owned(),
            });
        };
        let mut cols = line.split('\t');
        let (old, new) = (cols.next().unwrap_or(""), cols.next());
        let Some(new) = new else {
            problem("expected two columns separated by a tab, found no tab");
            continue;
        };
        if cols.next().is_some() {
            problem("expected two columns, found more than one tab");
        } else if !is_http_url(old) {
            problem("the old URL is empty or does not start with http:// or https://");
        } else if !is_http_url(new) {
            problem("the new URL is empty or does not start with http:// or https://");
        } else {
            rules.push(UrlRule {
                old: old.to_owned(),
                new: new.to_owned(),
                origin: format!("line {}", i + 1),
            });
        }
    }
    if problems.is_empty() {
        Ok(rules)
    } else {
        Err(problems)
    }
}

fn is_http_url(s: &str) -> bool {
    let starts = |p: &str| s.get(..p.len()).is_some_and(|x| x.eq_ignore_ascii_case(p));
    (starts("http://") || starts("https://")) && !s.chars().any(char::is_control)
}

/// What to rewrite besides whole URL values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RewriteOptions {
    /// Also rewrite the legacy Steam Cloud prefix
    /// ([`LEGACY_STEAM_PREFIX`]) to the current one, for URLs no rule
    /// matched.
    pub normalize_steam_hosts: bool,
    /// Also replace URLs inside `LuaScript`, `LuaScriptState` and `XmlUI`.
    pub in_scripts: bool,
}

/// One place that a rule touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touched {
    /// JSON path of the string (see [`crate::Occurrence::path`]).
    pub path: String,
    /// `GUID` of the owning object, `None` for the save itself.
    pub guid: Option<String>,
    /// `Nickname` of the owning object, if non-empty.
    pub nickname: Option<String>,
    /// How many URLs in this string: 1 for a value, any number in a script.
    pub count: usize,
}

/// What one mapping rule did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleReport {
    /// The rule.
    pub rule: UrlRule,
    /// Whole string values it matched, in document order.
    pub values: Vec<Touched>,
    /// Script strings it matched in, in document order. Replaced only with
    /// [`RewriteOptions::in_scripts`].
    pub scripts: Vec<Touched>,
}

impl RuleReport {
    /// Number of whole values.
    pub fn value_count(&self) -> usize {
        self.values.iter().map(|t| t.count).sum()
    }

    /// Number of URLs inside scripts.
    pub fn script_count(&self) -> usize {
        self.scripts.iter().map(|t| t.count).sum()
    }

    /// Whether the rule found nothing at all, values or scripts.
    pub fn is_unmatched(&self) -> bool {
        self.values.is_empty() && self.scripts.is_empty()
    }
}

/// The plan and result of a rewrite; both are the same thing, because a
/// rewrite is computed on a copy before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteReport {
    /// One entry per mapping rule, in mapping order.
    pub rules: Vec<RuleReport>,
    /// The Steam host rule, when `normalize_steam_hosts` was on.
    pub normalize: Option<RuleReport>,
    /// The options used.
    pub options: RewriteOptions,
}

impl RewriteReport {
    fn all(&self) -> impl Iterator<Item = &RuleReport> {
        self.rules.iter().chain(self.normalize.as_ref())
    }

    /// Whole values that were replaced.
    pub fn values_changed(&self) -> usize {
        self.all()
            .filter(|r| r.rule.old != r.rule.new)
            .map(RuleReport::value_count)
            .sum()
    }

    /// URLs inside scripts that were replaced (0 without `in_scripts`).
    pub fn scripts_changed(&self) -> usize {
        if !self.options.in_scripts {
            return 0;
        }
        self.all()
            .filter(|r| r.rule.old != r.rule.new)
            .map(RuleReport::script_count)
            .sum()
    }

    /// URLs inside scripts that matched but were left alone.
    pub fn scripts_skipped(&self) -> usize {
        if self.options.in_scripts {
            0
        } else {
            self.all().map(RuleReport::script_count).sum()
        }
    }

    /// The mapping rules that matched nothing.
    pub fn unmatched(&self) -> impl Iterator<Item = &RuleReport> {
        self.rules.iter().filter(|r| r.is_unmatched())
    }
}

struct Ctx<'a> {
    index: HashMap<&'a str, usize>,
    options: RewriteOptions,
    reports: Vec<RuleReport>,
    normalize: Option<RuleReport>,
}

type Owner = Option<(String, Option<String>)>;

impl Save {
    /// Returns a copy of the save with the URLs of `rules` replaced, and a
    /// report of what matched. The save itself is not changed.
    pub fn rewrite_urls(&self, rules: &UrlRules, options: RewriteOptions) -> (Save, RewriteReport) {
        let mut value = self.value().clone();
        let mut ctx = Ctx {
            index: rules
                .rules()
                .iter()
                .enumerate()
                .map(|(i, r)| (r.old.as_str(), i))
                .collect(),
            options,
            reports: rules
                .rules()
                .iter()
                .map(|rule| RuleReport {
                    rule: rule.clone(),
                    values: Vec::new(),
                    scripts: Vec::new(),
                })
                .collect(),
            normalize: options.normalize_steam_hosts.then(|| RuleReport {
                rule: UrlRule {
                    old: LEGACY_STEAM_PREFIX.to_owned(),
                    new: CURRENT_STEAM_PREFIX.to_owned(),
                    origin: "--normalize-steam-hosts".to_owned(),
                },
                values: Vec::new(),
                scripts: Vec::new(),
            }),
        };
        walk(&mut value, None, &mut String::new(), None, &mut ctx);
        let report = RewriteReport {
            rules: ctx.reports,
            normalize: ctx.normalize,
            options,
        };
        let save = Save::from_value(value).expect("an object stays an object");
        (save, report)
    }
}

fn walk(
    value: &mut Value,
    key: Option<&str>,
    path: &mut String,
    owner: Option<&Owner>,
    ctx: &mut Ctx<'_>,
) {
    match value {
        Value::String(s) => {
            let owner = owner.and_then(Option::as_ref);
            let touched = |count| Touched {
                path: path.clone(),
                guid: owner.map(|(g, _)| g.clone()),
                nickname: owner.and_then(|(_, n)| n.clone()),
                count,
            };
            if key.is_some_and(|k| SCRIPT_KEYS.contains(&k)) {
                rewrite_script(s, key == Some("XmlUI"), &touched, ctx);
            } else if let Some(&i) = ctx.index.get(s.as_str()) {
                ctx.reports[i].values.push(touched(1));
                s.clone_from(&ctx.reports[i].rule.new);
            } else if let Some(report) = ctx.normalize.as_mut()
                && let Some(rest) = strip_prefix_ignore_case(s, LEGACY_STEAM_PREFIX)
            {
                report.values.push(touched(1));
                *s = format!("{CURRENT_STEAM_PREFIX}{rest}");
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                let len = path.len();
                path.push_str(&format!("[{i}]"));
                walk(item, key, path, owner, ctx);
                path.truncate(len);
            }
        }
        Value::Object(map) => {
            let own: Owner = match map.get("GUID") {
                Some(Value::String(guid)) => Some((
                    guid.clone(),
                    map.get("Nickname")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                        .map(str::to_owned),
                )),
                _ => None,
            };
            let owner = if own.is_some() { Some(&own) } else { owner };
            for (k, child) in map.iter_mut() {
                let len = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(k);
                walk(child, Some(k), path, owner, ctx);
                path.truncate(len);
            }
        }
        _ => {}
    }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn is_url_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b)
}

/// Finds the URLs in `text`: `(start, end)` byte ranges.
fn find_urls(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let at_start = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let scheme = if !at_start || !bytes[i].eq_ignore_ascii_case(&b'h') {
            0
        } else if text[i..].len() >= 8
            && text.is_char_boundary(i + 8)
            && text[i..i + 8].eq_ignore_ascii_case("https://")
        {
            8
        } else if text[i..].len() >= 7
            && text.is_char_boundary(i + 7)
            && text[i..i + 7].eq_ignore_ascii_case("http://")
        {
            7
        } else {
            0
        };
        if scheme == 0 {
            i += 1;
            continue;
        }
        let mut end = i + scheme;
        while end < bytes.len() && is_url_char(bytes[end]) {
            end += 1;
        }
        while end > i + scheme && b".,;:!?)'*".contains(&bytes[end - 1]) {
            end -= 1;
        }
        out.push((i, end));
        i = end.max(i + 1);
    }
    out
}

fn rewrite_script(
    s: &mut String,
    xml: bool,
    touched: &dyn Fn(usize) -> Touched,
    ctx: &mut Ctx<'_>,
) {
    // (start, end, replacement or None) per URL, plus the rule it belongs to.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut counts = vec![0usize; ctx.reports.len()];
    let mut normalize_count = 0usize;
    for (start, end) in find_urls(s) {
        let token = &s[start..end];
        let mut hit = ctx
            .index
            .get(token)
            .map(|&i| (i, ctx.reports[i].rule.new.clone()));
        if hit.is_none() && xml && token.contains("&amp;") {
            let plain = token.replace("&amp;", "&");
            hit = ctx
                .index
                .get(plain.as_str())
                .map(|&i| (i, ctx.reports[i].rule.new.replace('&', "&amp;")));
        }
        if let Some((i, new)) = hit {
            counts[i] += 1;
            edits.push((start, end, new));
        } else if ctx.normalize.is_some()
            && let Some(rest) = strip_prefix_ignore_case(token, LEGACY_STEAM_PREFIX)
        {
            normalize_count += 1;
            edits.push((start, end, format!("{CURRENT_STEAM_PREFIX}{rest}")));
        }
    }
    for (i, n) in counts.into_iter().enumerate() {
        if n > 0 {
            ctx.reports[i].scripts.push(touched(n));
        }
    }
    if normalize_count > 0
        && let Some(report) = ctx.normalize.as_mut()
    {
        report.scripts.push(touched(normalize_count));
    }
    if ctx.options.in_scripts && !edits.is_empty() {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for (start, end, new) in edits {
            out.push_str(&s[last..start]);
            out.push_str(&new);
            last = end;
        }
        out.push_str(&s[last..]);
        *s = out;
    }
}
