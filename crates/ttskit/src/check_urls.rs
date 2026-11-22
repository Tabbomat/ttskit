//! `ttskit save check-urls`: finds the asset URLs of a save that no longer load.

use crate::http::{HttpClient, describe};
use std::io::{Read, Write};
use std::path::Path;

use serde_json::{Value, json};
use ttskit_core::{
    CheckConfig, CheckReport, Fetch, FetchError, FetchMethod, FetchRequest, FetchResponse, Outcome,
    UrlResult,
};

fn map_error(e: reqwest::Error) -> FetchError {
    if e.is_timeout() {
        FetchError::Timeout
    } else {
        FetchError::Network(describe(&e.without_url()))
    }
}

impl Fetch for HttpClient {
    fn fetch(&self, request: &FetchRequest<'_>) -> Result<FetchResponse, FetchError> {
        let builder = match request.method {
            FetchMethod::Head => self.client.head(request.url),
            FetchMethod::GetRange { max_body } => self
                .client
                .get(request.url)
                .header("Range", format!("bytes=0-{}", max_body.saturating_sub(1))),
        };
        let response = builder.send().map_err(map_error)?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let status = response.status().as_u16();
        let location = header("location");
        let content_type = header("content-type");
        let mut body = Vec::new();
        if let FetchMethod::GetRange { max_body } = request.method {
            let mut response = response;
            (&mut response)
                .take(max_body as u64)
                .read_to_end(&mut body)
                .map_err(|e| {
                    if e.to_string().to_ascii_lowercase().contains("timed out") {
                        FetchError::Timeout
                    } else {
                        FetchError::Network(e.to_string())
                    }
                })?;
        }
        Ok(FetchResponse {
            status,
            location,
            content_type,
            body,
        })
    }
}

/// What the command reports for the exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Every URL is ok, redirected or skipped.
    Clean,
    /// At least one URL is dead or unreachable.
    Problems,
}

/// Reads the save at `path`, checks its URLs with `client` and prints the report.
pub fn check_urls(
    path: &Path,
    client: &dyn Fetch,
    config: &CheckConfig,
    json: bool,
    out: &mut dyn Write,
) -> Result<CheckStatus, String> {
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let save =
        ttskit_core::Save::from_slice(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let report = ttskit_core::check_urls(save.asset_urls(), client, config);
    let io = |e: std::io::Error| e.to_string();
    if json {
        let mut text =
            serde_json::to_string_pretty(&report_json(path, &report)).map_err(|e| e.to_string())?;
        text.push('\n');
        out.write_all(text.as_bytes()).map_err(io)?;
    } else {
        write_text(&report, out).map_err(io)?;
    }
    Ok(if report.has_problems() {
        CheckStatus::Problems
    } else {
        CheckStatus::Clean
    })
}

const STATES: [&str; 5] = ["ok", "redirect", "dead", "unreachable", "skipped"];

fn counts(report: &CheckReport) -> [usize; 5] {
    let mut counts = [0; 5];
    for r in &report.results {
        let i = STATES.iter().position(|s| *s == r.outcome.as_str());
        counts[i.expect("known state")] += 1;
    }
    counts
}

fn outcome_json(outcome: &Outcome) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("result".into(), outcome.as_str().into());
    match outcome {
        Outcome::Ok { status } => {
            m.insert("status".into(), (*status).into());
        }
        Outcome::Redirect { status, target } => {
            m.insert("status".into(), (*status).into());
            m.insert("target".into(), target.as_str().into());
        }
        Outcome::Dead { status, detail } => {
            m.insert("status".into(), (*status).into());
            m.insert("detail".into(), json!(detail));
        }
        Outcome::Unreachable { reason } | Outcome::Skipped { reason } => {
            m.insert("reason".into(), reason.as_str().into());
        }
    }
    m
}

fn report_json(path: &Path, report: &CheckReport) -> Value {
    let urls: Vec<Value> = report
        .results
        .iter()
        .map(|r| {
            let mut entry = serde_json::Map::new();
            entry.insert("url".into(), r.asset.url.as_str().into());
            entry.append(&mut outcome_json(&r.outcome));
            entry.insert("labels".into(), json!(r.asset.labels));
            let occurrences: Vec<Value> = r
                .asset
                .occurrences
                .iter()
                .map(|o| json!({"path": o.path, "nickname": o.nickname, "guid": o.guid}))
                .collect();
            entry.insert("occurrences".into(), occurrences.into());
            Value::Object(entry)
        })
        .collect();
    let mut summary = serde_json::Map::new();
    summary.insert("total".into(), report.results.len().into());
    for (state, n) in STATES.iter().zip(counts(report)) {
        summary.insert((*state).into(), n.into());
    }
    json!({
        "save": path.display().to_string().replace('\\', "/"),
        "summary": summary,
        "urls": urls,
    })
}

fn write_occurrences(r: &UrlResult, out: &mut dyn Write) -> std::io::Result<()> {
    for o in &r.asset.occurrences {
        let nickname = o.nickname.as_deref().unwrap_or("-");
        let guid = o.guid.as_deref().unwrap_or("-");
        writeln!(out, "    {}  (nickname: {nickname}, guid: {guid})", o.path)?;
    }
    Ok(())
}

fn write_text(report: &CheckReport, out: &mut dyn Write) -> std::io::Result<()> {
    for r in report.results.iter().filter(|r| r.outcome.is_problem()) {
        let what = match &r.outcome {
            Outcome::Dead {
                status,
                detail: None,
            } => format!("dead (HTTP {status})"),
            Outcome::Dead {
                status,
                detail: Some(d),
            } => format!("dead (HTTP {status}, {d})"),
            Outcome::Unreachable { reason } => format!("unreachable ({reason})"),
            _ => unreachable!("only problems are listed here"),
        };
        writeln!(out, "{what}: {}", r.asset.url)?;
        write_occurrences(r, out)?;
    }
    for r in &report.results {
        if let Outcome::Redirect { target, .. } = &r.outcome {
            writeln!(out, "redirect: {} -> {target}", r.asset.url)?;
        }
    }
    let c = counts(report);
    writeln!(
        out,
        "{} URLs: {} ok, {} redirected, {} dead, {} unreachable, {} skipped",
        report.results.len(),
        c[0],
        c[1],
        c[2],
        c[3],
        c[4]
    )
}
