//! `ttskit save rewrite-urls`: replaces asset URLs in a save from a mapping.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use ttskit_cloud::{Manifest, UgcUrl};
use ttskit_core::{
    RewriteOptions, RewriteReport, RuleReport, Save, Touched, UrlRule, UrlRules, parse_map_tsv,
};

/// Most objects listed per mapping line in the text output.
const MAX_OBJECTS_LISTED: usize = 8;

/// What the command was asked to do.
pub struct Options<'a> {
    pub save: &'a Path,
    pub map: Option<&'a Path>,
    pub manifest: Option<&'a Path>,
    pub normalize_steam_hosts: bool,
    pub in_scripts: bool,
    /// Where to write; `None` with `in_place` or a dry run.
    pub out: Option<&'a Path>,
    pub in_place: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

fn read_text(path: &Path, what: &str) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read {what} {}: {e}", path.display()))?;
    String::from_utf8(bytes).map_err(|_| format!("{what} {} is not UTF-8 text", path.display()))
}

/// Rules from the mapping file.
fn map_rules(path: &Path) -> Result<Vec<UrlRule>, String> {
    let text = read_text(path, "mapping")?;
    parse_map_tsv(&text).map_err(|problems| {
        let mut msg = format!("{} is not a valid mapping:", path.display());
        for p in problems {
            msg.push_str(&format!("\n  line {}: {}", p.line, p.message));
        }
        msg.push_str("\nnothing was changed");
        msg
    })
}

/// Rules from a manifest: a Steam Cloud URL of the save whose SHA1 is in the
/// manifest is replaced by the manifest's URL for that content.
fn manifest_rules(path: &Path, save: &Save) -> Result<Vec<UrlRule>, String> {
    if !path.is_file() {
        return Err(format!(
            "cannot read manifest {}: no such file",
            path.display()
        ));
    }
    let manifest = Manifest::load(path).map_err(|e| e.to_string())?;
    let mut by_sha: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for entry in &manifest.entries {
        let urls = by_sha.entry(entry.sha1.as_str()).or_default();
        if !urls.contains(&entry.url.as_str()) {
            urls.push(&entry.url);
        }
    }
    let mut rules = Vec::new();
    for asset in save.asset_urls() {
        let Some(ugc) = UgcUrl::parse(&asset.url) else {
            continue;
        };
        let Some(urls) = by_sha.get(ugc.sha1.to_ascii_uppercase().as_str()) else {
            continue;
        };
        // Already one of the manifest's URLs: nothing to do.
        if urls.contains(&asset.url.as_str()) {
            continue;
        }
        match urls.as_slice() {
            [new] => rules.push(UrlRule {
                old: asset.url,
                new: (*new).to_owned(),
                origin: format!("manifest {}", path.display()),
            }),
            many => {
                return Err(format!(
                    "{} matches {} different URLs in {}: {}; pick one with --map",
                    asset.url,
                    many.len(),
                    path.display(),
                    many.join(", ")
                ));
            }
        }
    }
    Ok(rules)
}

/// The output-related arguments of a command that rewrites a save.
pub(crate) struct Output<'a> {
    pub save: &'a Path,
    pub out: Option<&'a Path>,
    pub in_place: bool,
    pub yes: bool,
    pub dry_run: bool,
}

/// Checks the output arguments before anything is read or written.
pub(crate) fn check_destination(o: &Output<'_>) -> Result<Option<PathBuf>, String> {
    if o.in_place && o.out.is_some() {
        return Err("use either --out or --in-place, not both".into());
    }
    if o.in_place {
        if !o.dry_run && !o.yes {
            return Err(format!(
                "--in-place replaces {}; add --yes to confirm, or --dry-run to preview",
                o.save.display()
            ));
        }
        return Ok(Some(o.save.to_owned()));
    }
    match o.out {
        Some(out) => {
            if out.exists() {
                if !out.is_file() {
                    return Err(format!("{} exists and is not a file", out.display()));
                }
                if same_file(out, o.save) {
                    return Err(format!(
                        "{} is the input; use --in-place --yes to replace it",
                        out.display()
                    ));
                }
                if !o.dry_run && !o.yes {
                    return Err(format!(
                        "{} already exists; add --yes to overwrite it, or --dry-run to preview",
                        out.display()
                    ));
                }
            }
            Ok(Some(out.to_owned()))
        }
        None if o.dry_run => Ok(None),
        None => Err("give --out FILE for the new save, or --in-place --yes, or --dry-run".into()),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Writes through a temporary file next to the target, so a failure leaves
/// the old file intact.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{} is not a file name", path.display()))?;
    let tmp = path.with_file_name(format!(".{name}.ttskit-tmp"));
    let result = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(())
}

/// Reads the inputs, prints the plan and, unless `--dry-run`, writes the save.
pub fn rewrite_urls(options: &Options<'_>, out: &mut dyn Write) -> Result<(), String> {
    if options.map.is_none() && options.manifest.is_none() && !options.normalize_steam_hosts {
        return Err(
            "nothing to do: give --map FILE, --from-manifest FILE or --normalize-steam-hosts"
                .into(),
        );
    }
    let destination = check_destination(&Output {
        save: options.save,
        out: options.out,
        in_place: options.in_place,
        yes: options.yes,
        dry_run: options.dry_run,
    })?;
    let data = std::fs::read(options.save)
        .map_err(|e| format!("cannot read {}: {e}", options.save.display()))?;
    let save = Save::from_slice(&data).map_err(|e| format!("{}: {e}", options.save.display()))?;

    let mut rules = Vec::new();
    if let Some(map) = options.map {
        rules.extend(map_rules(map)?);
    }
    if let Some(manifest) = options.manifest {
        rules.extend(manifest_rules(manifest, &save)?);
    }
    let rules = UrlRules::new(rules).map_err(|e| format!("{e}; nothing was changed"))?;
    let rewrite_options = RewriteOptions {
        normalize_steam_hosts: options.normalize_steam_hosts,
        in_scripts: options.in_scripts,
    };
    let (new_save, report) = save.rewrite_urls(&rules, rewrite_options);
    let new_bytes = new_save.to_vec();
    // Input that is not in the game's own layout is rewritten in it.
    let reformatted = save.to_vec() != data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&data);
    let changed = new_bytes != data;
    // A save that has nothing to replace is not rewritten in place.
    let will_write = !options.dry_run
        && destination
            .as_ref()
            .is_some_and(|_| !options.in_place || changed);

    let io = |e: io::Error| e.to_string();
    if options.json {
        let value = json_report(options, &rules, &report, will_write, reformatted);
        let mut text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        text.push('\n');
        out.write_all(text.as_bytes()).map_err(io)?;
    } else {
        write_plan(options, &rules, &report, reformatted, out).map_err(io)?;
    }
    if will_write && let Some(dest) = &destination {
        write_atomic(dest, &new_bytes)?;
    }
    if !options.json {
        let total = report.values_changed() + report.scripts_changed();
        if options.dry_run {
            writeln!(out, "Dry run: nothing was written.").map_err(io)?;
        } else if will_write && let Some(dest) = &destination {
            writeln!(out, "Replaced {total} URLs, wrote {}", display(dest)).map_err(io)?;
        } else {
            writeln!(
                out,
                "Nothing to replace; {} was not changed.",
                display(options.save)
            )
            .map_err(io)?;
        }
    }
    Ok(())
}

fn display(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Distinct owners of `touched`, in document order, with their URL counts.
fn owners(touched: &[Touched]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for t in touched {
        let label = match (&t.nickname, &t.guid) {
            (Some(n), Some(g)) => format!("{n} ({g})"),
            (None, Some(g)) => g.clone(),
            _ => "(save)".to_owned(),
        };
        match out.iter_mut().find(|(l, _)| *l == label) {
            Some((_, n)) => *n += t.count,
            None => out.push((label, t.count)),
        }
    }
    out
}

fn write_owners(touched: &[Touched], out: &mut dyn Write) -> io::Result<()> {
    let owners = owners(touched);
    for (label, n) in owners.iter().take(MAX_OBJECTS_LISTED) {
        writeln!(out, "    {label}: {n}")?;
    }
    if owners.len() > MAX_OBJECTS_LISTED {
        writeln!(
            out,
            "    ... and {} more objects (--json lists all)",
            owners.len() - MAX_OBJECTS_LISTED
        )?;
    }
    Ok(())
}

fn write_rule(r: &RuleReport, in_scripts: bool, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{}: {} -> {}", r.rule.origin, r.rule.old, r.rule.new)?;
    if r.is_unmatched() {
        return writeln!(out, "  matches nothing");
    }
    if !r.values.is_empty() {
        let n = owners(&r.values).len();
        writeln!(out, "  {} in values, in {n} objects", r.value_count())?;
        write_owners(&r.values, out)?;
    }
    if !r.scripts.is_empty() {
        let verb = if in_scripts {
            "replaced"
        } else {
            "not replaced, use --in-scripts"
        };
        writeln!(out, "  {} in scripts ({verb})", r.script_count())?;
        write_owners(&r.scripts, out)?;
    }
    Ok(())
}

fn write_plan(
    options: &Options<'_>,
    rules: &UrlRules,
    report: &RewriteReport,
    reformatted: bool,
    out: &mut dyn Write,
) -> io::Result<()> {
    let merged = rules.duplicates().len();
    let repeats = if merged == 0 {
        String::new()
    } else {
        format!(", {merged} repeated lines merged")
    };
    writeln!(out, "{} mapping lines{repeats}", rules.rules().len())?;
    for d in rules.duplicates() {
        writeln!(
            out,
            "  note: {}: {} is mapped again to the same URL",
            d.origin, d.old
        )?;
    }
    for r in &report.rules {
        write_rule(r, options.in_scripts, out)?;
    }
    if let Some(n) = &report.normalize {
        write_rule(n, options.in_scripts, out)?;
    }
    let unmatched: Vec<_> = report.unmatched().collect();
    if !unmatched.is_empty() {
        writeln!(out, "{} mapping lines match nothing:", unmatched.len())?;
        for r in unmatched {
            writeln!(out, "  {}: {}", r.rule.origin, r.rule.old)?;
        }
    }
    writeln!(
        out,
        "{} URL values to replace, {} in scripts to replace, {} in scripts left alone",
        report.values_changed(),
        report.scripts_changed(),
        report.scripts_skipped()
    )?;
    if reformatted {
        writeln!(
            out,
            "note: the input is not in the game's own layout; the output is written in it, so other lines may differ too"
        )?;
    }
    Ok(())
}

fn touched_json(t: &Touched) -> Value {
    json!({"path": t.path, "guid": t.guid, "nickname": t.nickname, "count": t.count})
}

fn rule_json(r: &RuleReport) -> Value {
    json!({
        "origin": r.rule.origin,
        "old": r.rule.old,
        "new": r.rule.new,
        "matched": !r.is_unmatched(),
        "values": r.value_count(),
        "scripts": r.script_count(),
        "value_uses": r.values.iter().map(touched_json).collect::<Vec<_>>(),
        "script_uses": r.scripts.iter().map(touched_json).collect::<Vec<_>>(),
    })
}

fn json_report(
    options: &Options<'_>,
    rules: &UrlRules,
    report: &RewriteReport,
    written: bool,
    reformatted: bool,
) -> Value {
    json!({
        "save": display(options.save),
        "out": if options.in_place {
            Some(display(options.save))
        } else {
            options.out.map(display)
        },
        "in_place": options.in_place,
        "dry_run": options.dry_run,
        "written": written,
        "reformatted": reformatted,
        "options": {
            "normalize_steam_hosts": options.normalize_steam_hosts,
            "in_scripts": options.in_scripts,
        },
        "rules": report.rules.iter().map(rule_json).collect::<Vec<_>>(),
        "normalize": report.normalize.as_ref().map(rule_json),
        "repeated": rules
            .duplicates()
            .iter()
            .map(|d| json!({"origin": d.origin, "old": d.old}))
            .collect::<Vec<_>>(),
        "unmatched": report.unmatched().map(|r| r.rule.origin.clone()).collect::<Vec<_>>(),
        "values_changed": report.values_changed(),
        "scripts_changed": report.scripts_changed(),
        "scripts_skipped": report.scripts_skipped(),
    })
}
