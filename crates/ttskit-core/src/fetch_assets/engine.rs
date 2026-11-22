//! The download engine: one URL at a time, with redirects, retries and
//! atomic file placement.

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use sha2::{Digest, Sha256};

use super::detect::type_from_extension;
use super::store::{self, IndexEntry, Store};
use super::{
    CacheDir, Download, DownloadError, DownloadResponse, FetchConfig, Fetched, Source, detect_type,
    hex,
};
use crate::check::{HostGate, html_reason, is_redirect, is_retryable, resolve_location};
use crate::save::assets::host_of;

/// Bytes of the body kept for type and HTML detection.
const PREFIX_BYTES: usize = 1024;
/// The index is written after this many newly completed downloads.
const INDEX_FLUSH_EVERY: usize = 50;

pub(super) struct Engine<'a> {
    client: &'a dyn Download,
    cache: Option<&'a CacheDir>,
    store: &'a Store,
    config: &'a FetchConfig,
    gate: HostGate,
    next_temp: AtomicU64,
    completed: AtomicUsize,
    index_error: Mutex<Option<String>>,
}

/// A temporary file that is removed when dropped (unless it was renamed).
struct Partial {
    path: PathBuf,
}

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Writes to a file while hashing, counting and keeping the first bytes.
struct HashingWriter {
    file: BufWriter<File>,
    hasher: Sha256,
    size: u64,
    prefix: Vec<u8>,
    max_size: u64,
    exceeded: bool,
}

impl Write for HashingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size.saturating_add(buf.len() as u64) > self.max_size {
            self.exceeded = true;
            return Err(io::Error::other("size limit exceeded"));
        }
        self.file.write_all(buf)?;
        self.hasher.update(buf);
        let room = PREFIX_BYTES.saturating_sub(self.prefix.len());
        self.prefix.extend_from_slice(&buf[..room.min(buf.len())]);
        self.size += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// A fully written temporary file.
struct Written {
    partial: Partial,
    size: u64,
    sha256: String,
    prefix: Vec<u8>,
}

enum CacheTry {
    Stored(Fetched),
    Failed(String),
    /// The cache file could not be read; the network may still work.
    Unreadable(String),
}

enum Attempt {
    Response(DownloadResponse, Written),
    Failed(String),
}

impl<'a> Engine<'a> {
    pub(super) fn new(
        client: &'a dyn Download,
        cache: Option<&'a CacheDir>,
        store: &'a Store,
        config: &'a FetchConfig,
    ) -> Self {
        Engine {
            client,
            cache,
            store,
            config,
            gate: HostGate::new(config.per_host.max(1)),
            next_temp: AtomicU64::new(0),
            completed: AtomicUsize::new(0),
            index_error: Mutex::new(None),
        }
    }

    /// The error of a failed periodic index write, if any.
    pub(super) fn into_index_error(self) -> Option<String> {
        self.index_error.into_inner().unwrap_or(None)
    }

    /// Stores one URL in `files/`: from the cache if it is there, else
    /// downloaded (following redirects).
    pub(super) fn download_one(&self, url: &str) -> Result<Fetched, String> {
        let mut unreadable = None;
        if let Some(path) = self.cache.and_then(|c| c.lookup(url)) {
            match self.copy_from_cache(url, path) {
                CacheTry::Stored(f) => return Ok(f),
                CacheTry::Failed(reason) => return Err(reason),
                CacheTry::Unreadable(e) => unreadable = Some(e),
            }
        }
        if self.config.offline {
            return Err(match unreadable {
                Some(e) => format!("cannot read the cache file: {e} (offline)"),
                None => "not in the cache (offline)".into(),
            });
        }
        let _permit = self.gate.acquire(&host_of(url));
        let mut current = url.to_owned();
        let mut hops = 0;
        let (response, written) = loop {
            let (response, written) = match self.request(&current) {
                Attempt::Response(r, w) => (r, w),
                Attempt::Failed(reason) => return Err(reason),
            };
            if !is_redirect(response.status) {
                break (response, written);
            }
            drop(written);
            let Some(next) = response
                .location
                .as_deref()
                .and_then(|l| resolve_location(&current, l))
            else {
                return Err(format!(
                    "redirect ({}) without a usable Location",
                    response.status
                ));
            };
            hops += 1;
            if hops > self.config.max_redirects {
                return Err("too many redirects".into());
            }
            current = next;
        };
        if !(200..300).contains(&response.status) {
            return Err(format!("HTTP {}", response.status));
        }
        if let Some(detail) = html_reason(response.content_type.as_deref(), &written.prefix) {
            return Err(format!("HTML page, not an asset ({detail})"));
        }
        if written.size == 0 {
            return Err("empty response body".into());
        }
        let file_type = detect_type(&written.prefix, response.content_type.as_deref(), url);
        self.place(url, &written, file_type, Source::Network)
    }

    /// Moves a finished temporary file to `files/<sha256>.<type>`.
    fn place(
        &self,
        url: &str,
        written: &Written,
        file_type: &str,
        source: Source,
    ) -> Result<Fetched, String> {
        let file = format!("{}/{}.{file_type}", store::FILES_DIR, written.sha256);
        let target = self.store.root().join(&file);
        std::fs::create_dir_all(self.store.files_dir())
            .map_err(|e| format!("cannot create the files directory: {e}"))?;
        // Replaces a file of the same name: it has the same content, unless an
        // earlier copy was damaged, which this repairs.
        std::fs::rename(&written.partial.path, &target)
            .map_err(|e| format!("cannot store the file: {e}"))?;
        Ok(Fetched {
            url: url.to_owned(),
            file,
            size: written.size,
            sha256: written.sha256.clone(),
            file_type: file_type.to_owned(),
            source,
        })
    }

    /// Copies a cache file through the hashing writer and stores it.
    fn copy_from_cache(&self, url: &str, path: &Path) -> CacheTry {
        let mut src = match File::open(path) {
            Ok(f) => f,
            Err(e) => return CacheTry::Unreadable(e.to_string()),
        };
        let fail = |e: io::Error| CacheTry::Failed(format!("cannot create a temporary file: {e}"));
        let partial = match self.new_partial() {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        let file = match File::create(&partial.path) {
            Ok(f) => f,
            Err(e) => return fail(e),
        };
        let mut writer = HashingWriter {
            file: BufWriter::new(file),
            hasher: Sha256::new(),
            size: 0,
            prefix: Vec::new(),
            max_size: self.config.max_size,
            exceeded: false,
        };
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return CacheTry::Unreadable(e.to_string()),
            };
            if let Err(e) = writer.write_all(&buf[..n]) {
                return CacheTry::Failed(if writer.exceeded {
                    format!(
                        "larger than the size limit of {} bytes",
                        self.config.max_size
                    )
                } else {
                    format!("cannot write the file: {e}")
                });
            }
        }
        let written = match finish(writer, partial) {
            Ok(w) => w,
            Err(e) => return CacheTry::Failed(format!("cannot write the file: {e}")),
        };
        if written.size == 0 {
            return CacheTry::Failed("cache file is empty".into());
        }
        if html_reason(None, &written.prefix).is_some() {
            return CacheTry::Failed("cached file is an HTML page".into());
        }
        let mut file_type = detect_type(&written.prefix, None, url);
        if file_type == "bin"
            && let Some(t) = CacheDir::extension_of(path).and_then(|e| type_from_extension(&e))
        {
            file_type = t;
        }
        match self.place(url, &written, file_type, Source::Cache) {
            Ok(f) => CacheTry::Stored(f),
            Err(e) => CacheTry::Failed(e),
        }
    }

    /// One request with retries, written to a fresh temporary file.
    fn request(&self, url: &str) -> Attempt {
        let mut n = 0;
        loop {
            let partial = match self.new_partial() {
                Ok(p) => p,
                Err(e) => return Attempt::Failed(format!("cannot create a temporary file: {e}")),
            };
            let file = match File::create(&partial.path) {
                Ok(f) => f,
                Err(e) => return Attempt::Failed(format!("cannot create a temporary file: {e}")),
            };
            let mut writer = HashingWriter {
                file: BufWriter::new(file),
                hasher: Sha256::new(),
                size: 0,
                prefix: Vec::new(),
                max_size: self.config.max_size,
                exceeded: false,
            };
            let result = self.client.download(url, self.config.max_size, &mut writer);
            let too_large = || {
                format!(
                    "larger than the size limit of {} bytes",
                    self.config.max_size
                )
            };
            let retry = match &result {
                Ok(r) => is_retryable(r.status),
                Err(DownloadError::Timeout | DownloadError::Network(_)) => true,
                Err(DownloadError::TooLarge) => return Attempt::Failed(too_large()),
                Err(DownloadError::Io(_)) if writer.exceeded => {
                    return Attempt::Failed(too_large());
                }
                Err(DownloadError::Io(m)) => {
                    return Attempt::Failed(format!("cannot write the file: {m}"));
                }
            };
            if !retry || n >= self.config.retries {
                return match result {
                    Ok(response) => match finish(writer, partial) {
                        Ok(written) => Attempt::Response(response, written),
                        Err(e) => Attempt::Failed(format!("cannot write the file: {e}")),
                    },
                    Err(e) => Attempt::Failed(e.to_string()),
                };
            }
            drop(writer);
            drop(partial);
            n += 1;
            let wait = self.config.backoff * n;
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
        }
    }

    fn new_partial(&self) -> io::Result<Partial> {
        let dir = self.store.partial_dir();
        std::fs::create_dir_all(&dir)?;
        let n = self.next_temp.fetch_add(1, Ordering::Relaxed);
        Ok(Partial {
            path: dir.join(format!("{}-{n}.part", std::process::id())),
        })
    }

    /// Adds a finished download to the index and saves it now and then.
    pub(super) fn record(&self, f: &Fetched) {
        self.store.insert(
            &f.url,
            IndexEntry {
                file: f.file.clone(),
                size: f.size,
                sha256: f.sha256.clone(),
                file_type: f.file_type.clone(),
            },
        );
        let done = self.completed.fetch_add(1, Ordering::SeqCst) + 1;
        if done.is_multiple_of(INDEX_FLUSH_EVERY)
            && let Err(e) = self.store.write_index()
        {
            *self
                .index_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
        }
    }
}

fn finish(writer: HashingWriter, partial: Partial) -> io::Result<Written> {
    let HashingWriter {
        file,
        hasher,
        size,
        prefix,
        ..
    } = writer;
    let file = file.into_inner().map_err(io::IntoInnerError::into_error)?;
    file.sync_all()?;
    drop(file);
    Ok(Written {
        partial,
        size,
        sha256: hex(&hasher.finalize()),
        prefix,
    })
}
