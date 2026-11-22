//! `ttskit save lint`: checks a save for common mistakes (local only).

use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};
use ttskit_core::Save;
use ttskit_lint::{Finding, LintConfig, RULES, Setting, Severity};

/// Project config file looked for in the current directory.
const CONFIG_FILE: &str = "ttskit.toml";

/// What the command was asked to do.
pub struct Options<'a> {
    pub save: &'a Path,
    pub config: Option<&'a Path>,
    /// Rules to switch off, then to warning, then to error (after the config).
    pub allow: &'a [String],
    pub warn: &'a [String],
    pub deny: &'a [String],
    /// Only findings at or above this severity are shown and counted.
    pub level: Severity,
    pub json: bool,
}

/// Outcome of a successful run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No finding at or above the level.
    Clean,
    /// At least one finding at or above the level.
    Findings,
}

/// Parses a `--level` value.
pub fn parse_level(text: &str) -> Result<Severity, String> {
    text.parse::<Severity>()
        .map_err(|_| format!("invalid level `{text}` (expected info, warning or error)"))
}

/// The configuration: the config file, then the command line overrides.
fn load_config(options: &Options<'_>) -> Result<LintConfig, String> {
    let path = match options.config {
        Some(path) => Some(path),
        None => Some(Path::new(CONFIG_FILE)).filter(|p| p.is_file()),
    };
    let mut config = match path {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
            LintConfig::from_toml_table(&text)
                .map_err(|e| format!("invalid config {}: {e}", path.display()))?
        }
        None => LintConfig::default(),
    };
    let overrides = [
        (options.allow, Setting::Off),
        (options.warn, Setting::On(Severity::Warning)),
        (options.deny, Setting::On(Severity::Error)),
    ];
    for (ids, setting) in overrides {
        for id in ids {
            config.set(id, setting).map_err(|e| e.to_string())?;
        }
    }
    Ok(config)
}

/// Findings grouped by owner, in order of first appearance. `None` is the
/// group of findings without an owning object.
fn group<'a>(findings: &[&'a Finding]) -> Vec<(Option<&'a Finding>, Vec<&'a Finding>)> {
    let mut groups: Vec<(Option<&Finding>, Vec<&Finding>)> = Vec::new();
    for &finding in findings {
        let owner = finding.guid.as_deref();
        match groups
            .iter_mut()
            .find(|(first, _)| first.and_then(|f| f.guid.as_deref()) == owner)
        {
            Some((_, list)) => list.push(finding),
            None => groups.push((owner.map(|_| finding), vec![finding])),
        }
    }
    groups
}

/// Header of a group: `Nickname (GUID)`, `(GUID)` or `(save)`.
fn header(first: Option<&Finding>) -> String {
    match first {
        None | Some(Finding { guid: None, .. }) => "(save)".to_owned(),
        Some(Finding {
            guid: Some(guid),
            nickname: Some(nick),
            ..
        }) => format!("{nick} ({guid})"),
        Some(Finding {
            guid: Some(guid), ..
        }) => format!("({guid})"),
    }
}

fn count(findings: &[&Finding], severity: Severity) -> usize {
    findings.iter().filter(|f| f.severity == severity).count()
}

fn write_text(
    out: &mut dyn Write,
    shown: &[&Finding],
    hidden: usize,
    level: Severity,
) -> std::io::Result<()> {
    if shown.is_empty() {
        writeln!(out, "no findings at or above {}", level.as_str())?;
        if hidden > 0 {
            writeln!(out, "{hidden} below {} hidden", level.as_str())?;
        }
        return Ok(());
    }
    for (first, list) in group(shown) {
        writeln!(out, "{}", header(first))?;
        for f in list {
            writeln!(
                out,
                "  {:<7}  {}  {}: {}",
                f.severity.as_str(),
                f.rule,
                f.path,
                f.message
            )?;
        }
    }
    writeln!(
        out,
        "{} findings at or above {}: {} error, {} warning, {} info; {hidden} below {} hidden",
        shown.len(),
        level.as_str(),
        count(shown, Severity::Error),
        count(shown, Severity::Warning),
        count(shown, Severity::Info),
        level.as_str(),
    )
}

fn write_json(
    out: &mut dyn Write,
    save: &Path,
    shown: &[&Finding],
    level: Severity,
) -> std::io::Result<()> {
    let findings: Vec<Value> = shown
        .iter()
        .map(|f| {
            json!({
                "rule": f.rule,
                "severity": f.severity.as_str(),
                "message": f.message,
                "path": f.path,
                "nickname": f.nickname,
                "guid": f.guid,
            })
        })
        .collect();
    let value = json!({
        "save": save.display().to_string(),
        "level": level.as_str(),
        "summary": {
            "error": count(shown, Severity::Error),
            "warning": count(shown, Severity::Warning),
            "info": count(shown, Severity::Info),
        },
        "findings": findings,
    });
    writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&value).expect("JSON value serializes")
    )
}

/// Lints the save and prints the findings.
pub fn lint(options: &Options<'_>, out: &mut dyn Write) -> Result<Status, String> {
    let config = load_config(options)?;
    let bytes = std::fs::read(options.save)
        .map_err(|e| format!("cannot read {}: {e}", options.save.display()))?;
    let save = Save::from_slice(&bytes)
        .map_err(|e| format!("{} is not a valid save: {e}", options.save.display()))?;
    let all = ttskit_lint::lint(&save, &config);
    let shown: Vec<&Finding> = all.iter().filter(|f| f.severity >= options.level).collect();
    let hidden = all.len() - shown.len();
    if options.json {
        write_json(out, options.save, &shown, options.level)
    } else {
        write_text(out, &shown, hidden, options.level)
    }
    .map_err(|e| e.to_string())?;
    Ok(if shown.is_empty() {
        Status::Clean
    } else {
        Status::Findings
    })
}

/// Prints the rule table: id, default severity, summary.
pub fn list_rules(out: &mut dyn Write) -> Result<(), String> {
    let width = RULES.iter().map(|r| r.id.len()).max().unwrap_or(0);
    for rule in RULES {
        writeln!(
            out,
            "{:<width$}  {:<7}  {}",
            rule.id,
            rule.default.as_str(),
            rule.summary
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(rule: &'static str, severity: Severity, guid: Option<&str>) -> Finding {
        Finding {
            rule,
            severity,
            message: "m".into(),
            path: "p".into(),
            nickname: guid.map(|g| format!("N{g}")),
            guid: guid.map(str::to_owned),
        }
    }

    #[test]
    fn groups_keep_first_appearance_order() {
        let list = [
            finding("a", Severity::Error, Some("x")),
            finding("b", Severity::Warning, None),
            finding("c", Severity::Info, Some("y")),
            finding("d", Severity::Info, Some("x")),
        ];
        let refs: Vec<&Finding> = list.iter().collect();
        let groups = group(&refs);
        let shape: Vec<_> = groups
            .iter()
            .map(|(first, l)| (header(*first), l.iter().map(|f| f.rule).collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            shape,
            [
                ("Nx (x)".to_owned(), vec!["a", "d"]),
                ("(save)".to_owned(), vec!["b"]),
                ("Ny (y)".to_owned(), vec!["c"]),
            ]
        );
    }

    #[test]
    fn text_output_has_lines_and_a_summary() {
        let list = [
            finding("rule-a", Severity::Error, Some("x")),
            finding("rule-b", Severity::Warning, None),
        ];
        let refs: Vec<&Finding> = list.iter().collect();
        let mut out = Vec::new();
        write_text(&mut out, &refs, 3, Severity::Warning).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            "Nx (x)\n  error    rule-a  p: m\n(save)\n  warning  rule-b  p: m\n\
             2 findings at or above warning: 1 error, 1 warning, 0 info; 3 below warning hidden\n"
        );
    }

    #[test]
    fn clean_output_is_one_line() {
        let mut out = Vec::new();
        write_text(&mut out, &[], 0, Severity::Error).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "no findings at or above error\n"
        );
    }

    #[test]
    fn level_parsing() {
        assert_eq!(parse_level("info").unwrap(), Severity::Info);
        assert!(parse_level("loud").unwrap_err().contains("loud"));
    }
}
