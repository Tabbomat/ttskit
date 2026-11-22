//! `ttskit save fetch-assets`: downloads the asset URLs of a save into a
//! local directory.

use std::io::{self, Write};
use std::path::Path;

use serde_json::{Value, json};
use ttskit_core::{
    AssetUrl, Download, DownloadError, DownloadResponse, FetchConfig, FetchPlan, FetchReport, Store,
};

use crate::http::HttpClient;

/// Passes bytes to `inner` and fails once more than `max` bytes arrive.
struct Limited<'a> {
    inner: &'a mut dyn Write,
    max: u64,
    written: u64,
    too_large: bool,
    sink_failed: bool,
}

impl Write for Limited<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written.saturating_add(buf.len() as u64) > self.max {
            self.too_large = true;
            return Err(io::Error::other("larger than the size limit"));
        }
        match self.inner.write(buf) {
            Ok(n) => {
                self.written += n as u64;
                Ok(n)
            }
            Err(e) => {
                self.sink_failed = true;
                Err(e)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().inspect_err(|_| self.sink_failed = true)
    }
}

/// Whether a body read error is a timeout.
fn is_timeout(e: &io::Error) -> bool {
    if let Some(inner) = e.get_ref()
        && let Some(r) = inner.downcast_ref::<reqwest::Error>()
        && r.is_timeout()
    {
        return true;
    }
    e.kind() == io::ErrorKind::TimedOut || e.to_string().to_ascii_lowercase().contains("timed out")
}

impl Download for HttpClient {
    fn download(
        &self,
        url: &str,
        max_size: u64,
        sink: &mut dyn Write,
    ) -> Result<DownloadResponse, DownloadError> {
        let mut response = self.client.get(url).send().map_err(|e| {
            if e.is_timeout() {
                DownloadError::Timeout
            } else {
                DownloadError::Network(crate::http::describe(&e.without_url()))
            }
        })?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let answer = DownloadResponse {
            status: response.status().as_u16(),
            location: header("location"),
            content_type: header("content-type"),
        };
        if !response.status().is_success() {
            return Ok(answer);
        }
        if response.content_length().is_some_and(|n| n > max_size) {
            return Err(DownloadError::TooLarge);
        }
        let mut limited = Limited {
            inner: sink,
            max: max_size,
            written: 0,
            too_large: false,
            sink_failed: false,
        };
        match io::copy(&mut response, &mut limited) {
            Ok(_) => Ok(answer),
            Err(_) if limited.too_large => Err(DownloadError::TooLarge),
            Err(e) if limited.sink_failed => Err(DownloadError::Io(e.to_string())),
            Err(e) if is_timeout(&e) => Err(DownloadError::Timeout),
            Err(e) => Err(DownloadError::Network(crate::http::describe_io(&e))),
        }
    }
}

/// Parses a size such as `512m`: bytes with an optional `k`, `m` or `g`
/// suffix (binary multiples, case-insensitive; a trailing `b` is allowed).
pub fn parse_size(text: &str) -> Result<u64, String> {
    let lower = text.trim().to_ascii_lowercase();
    let lower = lower.strip_suffix('b').unwrap_or(&lower);
    let (digits, factor) = match lower.chars().last() {
        Some('k') => (&lower[..lower.len() - 1], 1u64 << 10),
        Some('m') => (&lower[..lower.len() - 1], 1 << 20),
        Some('g') => (&lower[..lower.len() - 1], 1 << 30),
        _ => (lower, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "invalid size '{text}': use a number of bytes with an optional k, m or g suffix"
        ));
    }
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("invalid size '{text}': too large"))?;
    let size = n
        .checked_mul(factor)
        .ok_or_else(|| format!("invalid size '{text}': too large"))?;
    if size == 0 {
        return Err("the size limit must be at least 1 byte".into());
    }
    Ok(size)
}

/// What the command reports for the exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchStatus {
    /// Everything is downloaded or already present.
    Clean,
    /// At least one URL failed, or the index could not be written.
    Problems,
}

/// What the command was asked to do.
pub struct Options<'a> {
    pub save: &'a Path,
    /// The output directory as given on the command line.
    pub out: &'a Path,
    pub dry_run: bool,
    pub json: bool,
    pub config: FetchConfig,
}

fn display_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Reads the save, prints the plan, downloads and prints the result.
pub fn fetch_assets(
    options: &Options<'_>,
    client: &dyn Download,
    out: &mut dyn Write,
) -> Result<FetchStatus, String> {
    let Options {
        save: path,
        out: out_dir,
        dry_run,
        json,
        config,
    } = options;
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let save =
        ttskit_core::Save::from_slice(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    if out_dir.exists() && !out_dir.is_dir() {
        return Err(format!("{} is not a directory", out_dir.display()));
    }
    let store = Store::open(*out_dir).map_err(|e| e.to_string())?;
    let urls: Vec<AssetUrl> = save.asset_urls();
    let plan = ttskit_core::plan_fetch(urls, &store, config);
    let io = |e: io::Error| e.to_string();
    if !json {
        write_plan(&plan, config.verify, *dry_run, out).map_err(io)?;
        out.flush().map_err(io)?;
    }
    let report = if *dry_run {
        FetchReport {
            plan,
            ..FetchReport::default()
        }
    } else {
        ttskit_core::fetch_assets_with_plan(plan, client, &store, config)
    };
    if *json {
        let mut text = serde_json::to_string_pretty(&report_json(path, out_dir, &report))
            .map_err(|e| e.to_string())?;
        text.push('\n');
        out.write_all(text.as_bytes()).map_err(io)?;
    } else if !dry_run {
        write_result(&report, out_dir, out).map_err(io)?;
    }
    Ok(if report.has_failures() {
        FetchStatus::Problems
    } else {
        FetchStatus::Clean
    })
}

fn write_plan(
    plan: &FetchPlan,
    verify: bool,
    dry_run: bool,
    out: &mut dyn Write,
) -> io::Result<()> {
    writeln!(
        out,
        "{} URLs: {} already present, {} to download, {} skipped",
        plan.total,
        plan.present.len(),
        plan.to_download.len(),
        plan.skipped.len()
    )?;
    if verify {
        writeln!(out, "{} verified", plan.verified)?;
    }
    if !plan.invalid.is_empty() {
        writeln!(
            out,
            "{} invalid (will be downloaded again)",
            plan.invalid.len()
        )?;
        for (url, reason) in &plan.invalid {
            writeln!(out, "  {url}: {reason}")?;
        }
    }
    if dry_run {
        for asset in &plan.to_download {
            writeln!(out, "would download: {}", asset.url)?;
        }
    }
    Ok(())
}

fn write_result(report: &FetchReport, out_dir: &Path, out: &mut dyn Write) -> io::Result<()> {
    for f in &report.failed {
        writeln!(out, "failed: {}: {}", f.asset.url, f.reason)?;
        for o in &f.asset.occurrences {
            let nickname = o.nickname.as_deref().unwrap_or("-");
            let guid = o.guid.as_deref().unwrap_or("-");
            writeln!(out, "    {}  (nickname: {nickname}, guid: {guid})", o.path)?;
        }
    }
    writeln!(
        out,
        "downloaded {}, failed {} (files in {}, index {})",
        report.downloaded.len(),
        report.failed.len(),
        display_path(&out_dir.join("files")),
        display_path(&out_dir.join("index.json")),
    )?;
    if let Some(e) = &report.index_error {
        writeln!(out, "error: cannot write the index: {e}")?;
    }
    Ok(())
}

fn report_json(path: &Path, out_dir: &Path, report: &FetchReport) -> Value {
    let p = &report.plan;
    let downloaded: Vec<Value> = report
        .downloaded
        .iter()
        .map(|d| {
            json!({
                "url": d.url,
                "file": d.file,
                "size": d.size,
                "sha256": d.sha256,
                "type": d.file_type,
            })
        })
        .collect();
    let failed: Vec<Value> = report
        .failed
        .iter()
        .map(|f| {
            let occurrences: Vec<Value> = f
                .asset
                .occurrences
                .iter()
                .map(|o| json!({"path": o.path, "nickname": o.nickname, "guid": o.guid}))
                .collect();
            json!({
                "url": f.asset.url,
                "reason": f.reason,
                "occurrences": occurrences,
            })
        })
        .collect();
    json!({
        "save": display_path(path),
        "out": display_path(out_dir),
        "plan": {
            "total": p.total,
            "present": p.present.len(),
            "to_download": p.to_download.len(),
            "skipped": p.skipped.len(),
            "invalid": p.invalid.len(),
            "verified": p.verified,
        },
        "downloaded": downloaded,
        "failed": failed,
        "index_error": report.index_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1"), Ok(1));
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("1k"), Ok(1024));
        assert_eq!(parse_size("2K"), Ok(2048));
        assert_eq!(parse_size("512m"), Ok(512 << 20));
        assert_eq!(parse_size("512MB"), Ok(512 << 20));
        assert_eq!(parse_size("1g"), Ok(1 << 30));
        assert_eq!(parse_size(" 3g "), Ok(3 << 30));
    }

    #[test]
    fn bad_sizes() {
        for bad in [
            "", "0", "k", "m", "-1", "1.5m", "1x", "abc", "1 m", "1kk", "b",
        ] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
        assert!(parse_size("99999999999999999999").is_err());
        assert!(parse_size("17179869184g").is_err());
    }

    #[test]
    fn limiter_stops_after_the_limit() {
        let mut sink = Vec::new();
        let mut limited = Limited {
            inner: &mut sink,
            max: 4,
            written: 0,
            too_large: false,
            sink_failed: false,
        };
        assert!(limited.write_all(b"abcd").is_ok());
        assert!(limited.write_all(b"e").is_err());
        assert!(limited.too_large);
        assert!(!limited.sink_failed);
    }
}
