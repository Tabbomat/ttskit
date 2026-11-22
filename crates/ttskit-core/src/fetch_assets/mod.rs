//! Downloads the asset URLs of a save into a local directory.
//!
//! [`fetch_assets`] downloads every URL that is not yet in the output
//! directory and records it in `index.json`. The HTTP client is the
//! [`Download`] trait, so the engine (redirects, retries, per-host limits,
//! size limit, HTML detection, atomic file placement) is tested with a mock.
//! The engine follows redirects itself: a [`Download`] implementation must
//! not.
//!
//! Output layout:
//!
//! ```text
//! <out>/index.json
//! <out>/files/<sha256 hex>.<type>
//! <out>/.partial/            downloads in progress, removed at the end
//! ```
//!
//! File names derive only from the content hash and a fixed list of
//! extensions, so no URL text reaches the file system. Identical content
//! from several URLs is stored once.

mod cache;
mod detect;
mod engine;
mod probe;
mod store;

use std::fs::File;
use std::io::{self, Write};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::check::skip_reason;
use engine::Engine;

use crate::save::AssetUrl;

pub use cache::{CACHE_FOLDERS, CacheDir, CacheError, cache_key};
pub use detect::detect_type;
pub use probe::{Size, probe_sizes};
pub use store::{Index, IndexEntry, Store, StoreError};

/// The answer to a [`Download::download`] call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadResponse {
    pub status: u16,
    /// The `Location` header, if any.
    pub location: Option<String>,
    /// The `Content-Type` header, if any.
    pub content_type: Option<String>,
}

/// Why a download produced no HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    #[error("timed out")]
    Timeout,
    /// Connection, DNS, TLS or protocol error, with a short description.
    #[error("{0}")]
    Network(String),
    /// The body is larger than the allowed size.
    #[error("larger than the size limit")]
    TooLarge,
    /// Writing to the sink failed.
    #[error("write failed: {0}")]
    Io(String),
}

/// The HTTP client the engine uses. Must be callable from several threads.
pub trait Download: Sync {
    /// Requests `url` with `GET`. Redirects must not be followed. The body
    /// is written to `sink` only for `2xx` answers, and the call must fail
    /// with [`DownloadError::TooLarge`] as soon as more than `max_size`
    /// bytes would be written. A write error from `sink` is
    /// [`DownloadError::Io`].
    fn download(
        &self,
        url: &str,
        max_size: u64,
        sink: &mut dyn Write,
    ) -> Result<DownloadResponse, DownloadError>;
}

/// Settings for [`fetch_assets`] and [`plan`].
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// Most downloads in flight at once, over all hosts.
    pub concurrency: usize,
    /// Most downloads in flight at once from one host.
    pub per_host: usize,
    /// Extra attempts after a timeout, network error, `429` or `5xx`.
    pub retries: u32,
    /// Wait before retry `n` is `backoff * n`.
    pub backoff: Duration,
    /// Most redirects followed per URL.
    pub max_redirects: u32,
    /// Largest accepted body in bytes.
    pub max_size: u64,
    /// If not empty, only URLs on these hosts (or their subdomains) are
    /// downloaded; the rest are skipped.
    pub only_hosts: Vec<String>,
    /// URLs on these hosts (or their subdomains) are skipped.
    pub skip_hosts: Vec<String>,
    /// Rehash files that are already present instead of only comparing
    /// their size.
    pub verify: bool,
    /// Never use the network: URLs that are not in the cache fail.
    pub offline: bool,
}

impl Default for FetchConfig {
    fn default() -> Self {
        FetchConfig {
            concurrency: 16,
            per_host: 4,
            retries: 3,
            backoff: Duration::from_millis(500),
            max_redirects: 5,
            max_size: 512 * 1024 * 1024,
            only_hosts: Vec::new(),
            skip_hosts: Vec::new(),
            verify: false,
            offline: false,
        }
    }
}

/// A URL that is not downloaded on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub url: String,
    pub reason: String,
}

/// What a run would do. Lists are sorted by URL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchPlan {
    /// Number of URLs considered.
    pub total: usize,
    /// Not http(s) or excluded by the host filters.
    pub skipped: Vec<Skipped>,
    /// Already in the output directory and intact.
    pub present: Vec<String>,
    /// With `verify`: how many of `present` were rehashed and matched.
    pub verified: usize,
    /// In the index, but the file is missing or wrong; these are also in
    /// `to_download`.
    pub invalid: Vec<(String, String)>,
    pub to_download: Vec<AssetUrl>,
}

/// Where the bytes of a stored file came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Network,
    Cache,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Network => "network",
            Source::Cache => "cache",
        }
    }
}

/// One stored file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub url: String,
    /// Path relative to the output directory, forward slashes.
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub file_type: String,
    pub source: Source,
}

/// A URL that could not be downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub asset: AssetUrl,
    pub reason: String,
}

/// The result of [`fetch_assets`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchReport {
    pub plan: FetchPlan,
    /// Sorted by URL.
    pub downloaded: Vec<Fetched>,
    /// Sorted by URL.
    pub failed: Vec<Failed>,
    /// Set if `index.json` could not be written (the files are still there).
    pub index_error: Option<String>,
}

impl FetchReport {
    /// Whether any URL failed, or the index could not be saved.
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty() || self.index_error.is_some()
    }

    /// How many of `downloaded` were copied from the cache.
    pub fn from_cache(&self) -> usize {
        self.downloaded
            .iter()
            .filter(|f| f.source == Source::Cache)
            .count()
    }
}

/// Decides what a run would download, without changing anything. With
/// `config.verify`, present files are rehashed.
pub fn plan(urls: Vec<AssetUrl>, store: &Store, config: &FetchConfig) -> FetchPlan {
    let mut plan = FetchPlan {
        total: urls.len(),
        ..FetchPlan::default()
    };
    for asset in urls {
        if let Some(reason) = skip_reason(&asset.url, &config.only_hosts, &config.skip_hosts) {
            plan.skipped.push(Skipped {
                url: asset.url,
                reason,
            });
            continue;
        }
        let Some(entry) = store.lookup(&asset.url) else {
            plan.to_download.push(asset);
            continue;
        };
        match check_present(store, &entry, config.verify) {
            Ok(()) => {
                if config.verify {
                    plan.verified += 1;
                }
                plan.present.push(asset.url);
            }
            Err(reason) => {
                plan.invalid.push((asset.url.clone(), reason));
                plan.to_download.push(asset);
            }
        }
    }
    plan.skipped.sort_by(|a, b| a.url.cmp(&b.url));
    plan.present.sort();
    plan.invalid.sort();
    plan.to_download.sort_by(|a, b| a.url.cmp(&b.url));
    plan
}

fn check_present(store: &Store, entry: &IndexEntry, verify: bool) -> Result<(), String> {
    let path = store.file_path(entry);
    let meta = std::fs::metadata(&path).map_err(|_| "file is missing".to_owned())?;
    if !meta.is_file() || meta.len() != entry.size {
        return Err(format!(
            "size is {} bytes, the index says {}",
            meta.len(),
            entry.size
        ));
    }
    if verify {
        let hash = hash_file(&path).map_err(|e| format!("cannot read file: {e}"))?;
        if hash != entry.sha256 {
            return Err("content does not match the recorded sha256".into());
        }
    }
    Ok(())
}

fn hash_file(path: &std::path::Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Stores every URL in `urls` (as returned by `Save::asset_urls`) that is
/// not yet in `store`, then writes `index.json`. Each URL is taken from
/// `cache` if it is there, else downloaded (or, with `config.offline`,
/// failed).
pub fn fetch_assets(
    urls: Vec<AssetUrl>,
    client: &dyn Download,
    cache: Option<&CacheDir>,
    store: &Store,
    config: &FetchConfig,
) -> FetchReport {
    let p = plan(urls, store, config);
    fetch_assets_with_plan(p, client, cache, store, config)
}

/// Like [`fetch_assets`], but runs a plan made earlier by [`plan`] (with the
/// same `store` and `config`), so that a caller that prints the plan first
/// does not have to compute it, and with `verify` rehash the files, twice.
pub fn fetch_assets_with_plan(
    mut plan: FetchPlan,
    client: &dyn Download,
    cache: Option<&CacheDir>,
    store: &Store,
    config: &FetchConfig,
) -> FetchReport {
    let queue = std::mem::take(&mut plan.to_download);
    let mut report = FetchReport {
        plan,
        ..FetchReport::default()
    };
    if queue.is_empty() {
        report.plan.to_download = queue;
        return report;
    }
    report.plan.to_download = queue.clone();
    // Leftovers of an interrupted run.
    let _ = std::fs::remove_dir_all(store.partial_dir());

    let engine = Engine::new(client, cache, store, config);
    let queue: Mutex<std::collections::VecDeque<AssetUrl>> = Mutex::new(queue.into());
    let results: Mutex<Vec<Result<Fetched, Failed>>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..config.concurrency.max(1) {
            scope.spawn(|| {
                loop {
                    let Some(asset) = queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .pop_front()
                    else {
                        break;
                    };
                    let result = match engine.download_one(&asset.url) {
                        Ok(fetched) => {
                            engine.record(&fetched);
                            Ok(fetched)
                        }
                        Err(reason) => Err(Failed { asset, reason }),
                    };
                    results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(result);
                }
            });
        }
    });
    for result in results.into_inner().unwrap_or_else(PoisonError::into_inner) {
        match result {
            Ok(f) => report.downloaded.push(f),
            Err(f) => report.failed.push(f),
        }
    }
    report.downloaded.sort_by(|a, b| a.url.cmp(&b.url));
    report.failed.sort_by(|a, b| a.asset.url.cmp(&b.asset.url));
    let mut index_error = engine.into_index_error();
    if !report.downloaded.is_empty() {
        index_error = store.write_index().err().map(|e| e.to_string());
    }
    report.index_error = index_error;
    // Only succeeds when nothing is left in it.
    let _ = std::fs::remove_dir(store.partial_dir());
    report
}

#[cfg(test)]
mod tests;
