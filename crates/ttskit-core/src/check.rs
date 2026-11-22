//! Dead-link check for the asset URLs of a save.
//!
//! [`check_urls`] requests every distinct URL once and classifies the answer
//! as an [`Outcome`]. The HTTP client is the [`Fetch`] trait, so the engine
//! (redirects, retries, `HEAD` fallback, per-host limits, content sniffing)
//! is tested with a mock and needs no network. The engine follows redirects
//! itself: a [`Fetch`] implementation must not.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::save::AssetUrl;
use crate::save::assets::host_of;

/// How a URL is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMethod {
    /// `HEAD`: no body.
    Head,
    /// `GET` with `Range: bytes=0-(max_body - 1)`; the response body is cut
    /// to at most `max_body` bytes.
    GetRange { max_body: usize },
}

/// One request; redirects must not be followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest<'a> {
    pub url: &'a str,
    pub method: FetchMethod,
}

/// The answer to a [`FetchRequest`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchResponse {
    pub status: u16,
    /// The `Location` header, if any.
    pub location: Option<String>,
    /// The `Content-Type` header, if any.
    pub content_type: Option<String>,
    /// The first bytes of the body (empty for `HEAD`).
    pub body: Vec<u8>,
}

/// Why no HTTP answer was received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The request timed out.
    Timeout,
    /// Connection, DNS, TLS or protocol error, with a short description.
    Network(String),
}

/// The HTTP client the engine uses. Must be callable from several threads.
pub trait Fetch: Sync {
    fn fetch(&self, request: &FetchRequest<'_>) -> Result<FetchResponse, FetchError>;
}

/// Settings for [`check_urls`].
#[derive(Debug, Clone)]
pub struct CheckConfig {
    /// Most requests in flight at once, over all hosts.
    pub concurrency: usize,
    /// Most requests in flight at once to one host.
    pub per_host: usize,
    /// Extra attempts after a timeout, network error, `429` or `5xx`.
    pub retries: u32,
    /// Wait before retry `n` is `backoff * n`.
    pub backoff: Duration,
    /// Most redirects followed per URL.
    pub max_redirects: u32,
    /// Download the start of the body and reject HTML error pages.
    pub verify_content: bool,
    /// If not empty, only URLs on these hosts (or their subdomains) are
    /// checked; the rest are skipped.
    pub only_hosts: Vec<String>,
    /// URLs on these hosts (or their subdomains) are skipped.
    pub skip_hosts: Vec<String>,
}

impl Default for CheckConfig {
    fn default() -> Self {
        CheckConfig {
            concurrency: 16,
            per_host: 4,
            retries: 3,
            backoff: Duration::from_millis(500),
            max_redirects: 5,
            verify_content: false,
            only_hosts: Vec::new(),
            skip_hosts: Vec::new(),
        }
    }
}

/// The result for one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Answered with `2xx`.
    Ok { status: u16 },
    /// Redirected, and the final target answered with `2xx`. Not a problem.
    Redirect { status: u16, target: String },
    /// Final answer was `4xx`/`5xx`, or `2xx` with content that is not an
    /// asset (`detail` says why).
    Dead { status: u16, detail: Option<String> },
    /// No usable answer: network error, timeout, or a broken redirect.
    Unreachable { reason: String },
    /// Not checked: not http/https, or excluded by host.
    Skipped { reason: String },
}

impl Outcome {
    /// Stable lowercase name: `ok`, `redirect`, `dead`, `unreachable`, `skipped`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Ok { .. } => "ok",
            Outcome::Redirect { .. } => "redirect",
            Outcome::Dead { .. } => "dead",
            Outcome::Unreachable { .. } => "unreachable",
            Outcome::Skipped { .. } => "skipped",
        }
    }

    /// Whether this result makes the check fail.
    pub fn is_problem(&self) -> bool {
        matches!(self, Outcome::Dead { .. } | Outcome::Unreachable { .. })
    }
}

/// One URL with its outcome and where the save uses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlResult {
    pub asset: AssetUrl,
    pub outcome: Outcome,
}

/// All results, sorted by URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub results: Vec<UrlResult>,
}

impl CheckReport {
    /// Whether any URL is dead or unreachable.
    pub fn has_problems(&self) -> bool {
        self.results.iter().any(|r| r.outcome.is_problem())
    }
}

/// Bytes downloaded for content checks.
const SNIFF_BYTES: usize = 1024;

/// Checks every URL in `urls` (as returned by `Save::asset_urls`).
pub fn check_urls(urls: Vec<AssetUrl>, client: &dyn Fetch, config: &CheckConfig) -> CheckReport {
    let shared = Shared {
        client,
        config,
        gate: HostGate::new(config.per_host.max(1)),
        head_refused: Mutex::new(HashSet::new()),
    };
    let queue: Mutex<VecDeque<(usize, AssetUrl)>> =
        Mutex::new(urls.into_iter().enumerate().collect());
    let results: Mutex<Vec<(usize, UrlResult)>> = Mutex::new(Vec::new());
    let workers = config.concurrency.max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some((index, asset)) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let outcome = shared.check_one(&asset.url);
                    results
                        .lock()
                        .unwrap()
                        .push((index, UrlResult { asset, outcome }));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| a.1.asset.url.cmp(&b.1.asset.url).then(a.0.cmp(&b.0)));
    CheckReport {
        results: results.into_iter().map(|(_, r)| r).collect(),
    }
}

struct Shared<'a> {
    client: &'a dyn Fetch,
    config: &'a CheckConfig,
    gate: HostGate,
    /// Hosts that answered `HEAD` with a refusal; they get `GET` right away.
    head_refused: Mutex<HashSet<String>>,
}

/// Final answer of one request chain, before it is turned into an outcome.
enum Answer {
    Response(FetchResponse),
    Failed(FetchError),
}

fn host_matches(host: &str, listed: &[String]) -> bool {
    listed.iter().any(|h| {
        let h = h.trim_start_matches('.').to_ascii_lowercase();
        host == h || host.ends_with(&format!(".{h}"))
    })
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn is_retryable(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

impl Shared<'_> {
    fn check_one(&self, url: &str) -> Outcome {
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Outcome::Skipped {
                reason: "not an http or https URL".into(),
            };
        }
        let host = host_of(url);
        if !self.config.only_hosts.is_empty() && !host_matches(&host, &self.config.only_hosts) {
            return Outcome::Skipped {
                reason: "host not selected by --host".into(),
            };
        }
        if host_matches(&host, &self.config.skip_hosts) {
            return Outcome::Skipped {
                reason: "host excluded by --skip-host".into(),
            };
        }
        let _permit = self.gate.acquire(&host);
        let mut current = url.to_owned();
        let mut hops = 0;
        loop {
            let response = match self.request_with_fallback(&current, &host_of(&current)) {
                Answer::Response(r) => r,
                Answer::Failed(e) => return unreachable(&e),
            };
            if is_redirect(response.status) {
                let Some(next) = response
                    .location
                    .as_deref()
                    .and_then(|l| resolve_location(&current, l))
                else {
                    return Outcome::Unreachable {
                        reason: format!("redirect ({}) without a usable Location", response.status),
                    };
                };
                hops += 1;
                if hops > self.config.max_redirects {
                    return Outcome::Unreachable {
                        reason: "too many redirects".into(),
                    };
                }
                current = next;
                continue;
            }
            return self.classify(url, &current, &response);
        }
    }

    fn classify(&self, original: &str, final_url: &str, r: &FetchResponse) -> Outcome {
        if !(200..300).contains(&r.status) {
            return Outcome::Dead {
                status: r.status,
                detail: None,
            };
        }
        if self.config.verify_content
            && let Some(detail) = not_an_asset(r)
        {
            return Outcome::Dead {
                status: r.status,
                detail: Some(detail),
            };
        }
        if final_url == original {
            Outcome::Ok { status: r.status }
        } else {
            Outcome::Redirect {
                status: r.status,
                target: final_url.to_owned(),
            }
        }
    }

    /// One logical request: `HEAD` (or ranged `GET` when content is verified
    /// or the host refuses `HEAD`), falling back to a ranged `GET`.
    fn request_with_fallback(&self, url: &str, host: &str) -> Answer {
        let get = FetchMethod::GetRange {
            max_body: SNIFF_BYTES,
        };
        let refused = self.head_refused.lock().unwrap().contains(host);
        if self.config.verify_content || refused {
            return self.attempt(url, get);
        }
        match self.attempt(url, FetchMethod::Head) {
            Answer::Response(r) if matches!(r.status, 400 | 403 | 405 | 501) => {
                self.head_refused.lock().unwrap().insert(host.to_owned());
                self.attempt(url, get)
            }
            other => other,
        }
    }

    /// One request with retries.
    fn attempt(&self, url: &str, method: FetchMethod) -> Answer {
        let mut n = 0;
        loop {
            let result = self.client.fetch(&FetchRequest { url, method });
            let retry = match &result {
                Ok(r) => is_retryable(r.status),
                Err(_) => true,
            };
            if !retry || n >= self.config.retries {
                return match result {
                    Ok(r) => Answer::Response(r),
                    Err(e) => Answer::Failed(e),
                };
            }
            n += 1;
            let wait = self.config.backoff * n;
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
        }
    }
}

fn unreachable(e: &FetchError) -> Outcome {
    Outcome::Unreachable {
        reason: match e {
            FetchError::Timeout => "timed out".into(),
            FetchError::Network(m) => m.clone(),
        },
    }
}

/// Why a `2xx` answer is not an image, model or asset bundle, if so: an HTML
/// page (by `Content-Type` or by what the body starts with).
fn not_an_asset(r: &FetchResponse) -> Option<String> {
    let ct = r
        .content_type
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if ct.starts_with("text/html") || ct.starts_with("application/xhtml") {
        return Some(format!(
            "content type is {}",
            ct.split(';').next().unwrap_or("")
        ));
    }
    let body = r.body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&r.body);
    let start: Vec<u8> = body
        .iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .take(15)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    if start.starts_with(b"<!doctype html") || start.starts_with(b"<html") {
        return Some("body is an HTML page".into());
    }
    None
}

/// Resolves a `Location` header against the URL that sent it.
fn resolve_location(base: &str, location: &str) -> Option<String> {
    let location = location.trim();
    if location.is_empty() {
        return None;
    }
    let lower = location.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(location.to_owned());
    }
    let (scheme, rest) = base.split_once("://")?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if let Some(network_path) = location.strip_prefix("//") {
        return Some(format!("{scheme}://{network_path}"));
    }
    if location.contains("://") {
        return None; // another scheme, e.g. ftp://
    }
    if location.starts_with('/') {
        return Some(format!("{scheme}://{authority}{location}"));
    }
    let path = rest[authority_end..].split(['?', '#']).next().unwrap_or("");
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    Some(format!("{scheme}://{authority}{dir}/{location}"))
}

/// Limits the requests in flight per host.
struct HostGate {
    limit: usize,
    busy: Mutex<HashMap<String, usize>>,
    freed: Condvar,
}

struct Permit<'a> {
    gate: &'a HostGate,
    host: String,
}

impl HostGate {
    fn new(limit: usize) -> Self {
        HostGate {
            limit,
            busy: Mutex::new(HashMap::new()),
            freed: Condvar::new(),
        }
    }

    fn acquire(&self, host: &str) -> Permit<'_> {
        let mut busy = self.busy.lock().unwrap();
        while busy.get(host).copied().unwrap_or(0) >= self.limit {
            busy = self.freed.wait(busy).unwrap();
        }
        *busy.entry(host.to_owned()).or_insert(0) += 1;
        Permit {
            gate: self,
            host: host.to_owned(),
        }
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut busy = self.gate.busy.lock().unwrap();
        if let Some(n) = busy.get_mut(&self.host) {
            *n -= 1;
        }
        self.gate.freed.notify_all();
    }
}

#[cfg(test)]
mod tests;
