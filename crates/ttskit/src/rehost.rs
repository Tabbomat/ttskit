//! `ttskit save rehost`: moves the assets of a save that live on other hosts
//! into the project's Steam Cloud folder and points the save at the new URLs.
//!
//! The steps, each reusing an existing piece:
//!
//! 1. select the URLs to move: every http(s) URL of the save except the ones
//!    that already point into the user's own cloud, narrowed by `--host` and
//!    `--skip-host`;
//! 2. look up the sizes with `HEAD` and print the plan; `--dry-run` stops
//!    here, otherwise the user confirms;
//! 3. download the files into `<local dir>/<folder>/files` (as `save
//!    fetch-assets` does);
//! 4. upload and share them in the cloud folder `<cloud folder>/<folder>`
//!    (as `cloud sync push` does);
//! 5. record them in the project manifest;
//! 6. rewrite the save (as `save rewrite-urls` does) and write it to a new
//!    file, or over the input with `--in-place --yes`.
//!
//! A run that stops part way is run again with the same arguments: files that
//! are already downloaded or already in the cloud are not fetched or written
//! again. The save is only written after every upload succeeded.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ttskit_cloud::{
    BackupStore, CloudBackend, CloudIndex, IgnoreSet, LocalFile, ManifestEntry, ProjectConfig,
    UgcUrl, format_size, normalize_folder, sha1_hex,
};
use ttskit_core::{
    AssetUrl, Download, Fetch, FetchConfig, IndexEntry, RewriteOptions, Save, Size, Store, UrlRule,
    UrlRules, fetch_assets_with_plan, plan_fetch, probe_sizes,
};

use crate::cloud::{self, Confirm, RunMode};
use crate::rewrite_urls::{Output, check_destination, write_atomic};

/// The cloud and local sub folder used when `--folder` is not given.
pub const DEFAULT_FOLDER: &str = "rehosted";

/// What the command was asked to do.
pub struct Options {
    pub save: PathBuf,
    /// The project config.
    pub config: PathBuf,
    /// The sub folder of the project's cloud folder and local directory.
    pub folder: String,
    /// Only rehost URLs on these hosts or their subdomains; empty for all.
    /// `imgur` stands for `imgur.com`.
    pub hosts: Vec<String>,
    /// Leave URLs on these hosts or their subdomains alone.
    pub skip_hosts: Vec<String>,
    pub out: Option<PathBuf>,
    pub in_place: bool,
    /// Also replace URLs inside script strings.
    pub in_scripts: bool,
    pub yes: bool,
    pub dry_run: bool,
    /// Download settings; `only_hosts` and `skip_hosts` are set from
    /// `hosts` and `skip_hosts` by [`prepare`].
    pub fetch: FetchConfig,
}

/// What the command reports for the exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Everything selected was moved, or there was nothing to do.
    Clean,
    /// Some URLs could not be downloaded (they stay as they are in the
    /// save), or the manifest could not be updated.
    Problems,
}

/// Everything checked and read before connecting to Steam.
pub struct Prepared {
    options: Options,
    save: Save,
    data: Vec<u8>,
    destination: Option<PathBuf>,
    /// The sub folder, normalized: levels joined with `/`.
    sub: String,
    cloud_folder: String,
    local_dir: PathBuf,
    manifest_file: PathBuf,
    store: Store,
}

fn config_dir_of(config: &Path) -> &Path {
    match config.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Checks `--folder`: a relative path of one or more levels.
fn normalize_sub(value: &str) -> Result<String, String> {
    if let Some(c) = value
        .chars()
        .find(|c| c.is_control() || "<>:\"|?*".contains(*c))
    {
        return Err(format!("--folder must not contain {c:?}"));
    }
    let sub = normalize_folder(value);
    if sub.is_empty() {
        return Err("--folder must name a folder".into());
    }
    if sub.split('/').any(|s| s == "." || s == "..") {
        return Err("--folder must not contain `.` or `..` levels".into());
    }
    Ok(sub)
}

/// `imgur` is short for `imgur.com`.
fn expand_host(host: &str) -> String {
    if host.eq_ignore_ascii_case("imgur") {
        "imgur.com".to_owned()
    } else {
        host.to_owned()
    }
}

/// The default output file: `save.json` becomes `save.rehosted.json`.
fn default_out(save: &Path) -> PathBuf {
    let stem = save
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    save.with_file_name(format!("{stem}.rehosted.json"))
}

/// Reads and checks everything local: the arguments, the project config, the
/// save and the download directory.
pub fn prepare(mut options: Options) -> Result<Prepared, String> {
    let sub = normalize_sub(&options.folder)?;
    // Without --out and --in-place the result goes next to the input; it is
    // checked like an explicit --out.
    let out = options
        .out
        .clone()
        .or_else(|| (!options.in_place).then(|| default_out(&options.save)));
    let destination = check_destination(&Output {
        save: &options.save,
        out: out.as_deref(),
        in_place: options.in_place,
        yes: options.yes,
        dry_run: options.dry_run,
    })?;
    let project = ProjectConfig::load(&options.config).map_err(|e| e.to_string())?;
    let config_dir = config_dir_of(&options.config);
    let data = std::fs::read(&options.save)
        .map_err(|e| format!("cannot read {}: {e}", options.save.display()))?;
    let save = Save::from_slice(&data).map_err(|e| format!("{}: {e}", options.save.display()))?;
    let local_dir = project.resolve_local_dir(config_dir).join(&sub);
    if local_dir.exists() && !local_dir.is_dir() {
        return Err(format!("{} is not a directory", local_dir.display()));
    }
    let store = Store::open(&local_dir).map_err(|e| e.to_string())?;
    options.fetch.only_hosts = options.hosts.iter().map(|h| expand_host(h)).collect();
    options.fetch.skip_hosts = options.skip_hosts.iter().map(|h| expand_host(h)).collect();
    Ok(Prepared {
        cloud_folder: format!("{}/{sub}", project.cloud_folder),
        manifest_file: config_dir.join(ttskit_cloud::Manifest::FILE_NAME),
        options,
        save,
        data,
        destination,
        sub,
        local_dir,
        store,
    })
}

fn io_err(e: io::Error) -> String {
    e.to_string()
}

/// One URL that is going to be moved.
struct Row {
    url: String,
    /// How often the save uses it.
    count: usize,
    state: State,
}

enum State {
    /// Already downloaded: the file's index entry.
    Present(IndexEntry),
    /// To be downloaded; what `HEAD` said about its size.
    New(Size),
}

/// Where the bytes of a URL are, once they are in the local directory.
struct Stored {
    /// Path relative to the local folder, `files/<sha256>.<type>`.
    file: String,
}

fn count_label(count: usize) -> String {
    if count > 1 {
        format!(" (used {count} times)")
    } else {
        String::new()
    }
}

/// The file name inside the cloud folder: `<sha256>.<type>`.
fn file_name(file: &str) -> &str {
    file.rsplit_once('/').map_or(file, |(_, n)| n)
}

#[allow(clippy::too_many_arguments)]
fn write_plan(
    p: &Prepared,
    total: usize,
    own: usize,
    skipped: usize,
    rows: &[Row],
    quota: Option<ttskit_cloud::Quota>,
    out: &mut dyn Write,
) -> io::Result<()> {
    let target = format!("{}/files", p.cloud_folder);
    writeln!(
        out,
        "{total} asset URLs: {own} already in your cloud (left alone), {skipped} not selected, {} to rehost",
        rows.len()
    )?;
    let mut download = 0u64;
    let mut unknown = 0usize;
    let mut to_download = 0usize;
    for row in rows {
        let (size, target_text) = match &row.state {
            State::Present(entry) => (
                format!("{} (downloaded)", format_size(entry.size)),
                format!("{target}/{}", file_name(&entry.file)),
            ),
            State::New(Size::Known(n)) => {
                to_download += 1;
                download += n;
                (
                    format_size(*n),
                    format!("{target}/<sha256>.<type> (named after the download)"),
                )
            }
            State::New(Size::Unknown(reason)) => {
                to_download += 1;
                unknown += 1;
                (
                    format!("size unknown ({reason})"),
                    format!("{target}/<sha256>.<type> (named after the download)"),
                )
            }
        };
        writeln!(out, "  {}{}", row.url, count_label(row.count))?;
        writeln!(out, "    {size} -> {target_text}")?;
    }
    if rows.is_empty() {
        return Ok(());
    }
    let unknown_note = if unknown > 0 {
        format!(" plus {unknown} of unknown size")
    } else {
        String::new()
    };
    writeln!(
        out,
        "{to_download} to download: {}{unknown_note}; the upload is at most that, plus the files already downloaded; identical files are uploaded once",
        format_size(download)
    )?;
    if let Some(q) = quota {
        writeln!(out, "{}", cloud::quota_line(q))?;
    }
    writeln!(
        out,
        "note: only rehost assets you may redistribute; others' images and models are not yours to share"
    )
}

/// The cloud URLs of the user's own cloud, as `(handle, SHA1)`.
fn own_cloud(index: &CloudIndex) -> HashSet<(u64, String)> {
    index
        .entries
        .iter()
        .filter_map(|e| e.url.as_deref().and_then(UgcUrl::parse))
        .map(|u| (u.handle.0, u.sha1.to_ascii_uppercase()))
        .collect()
}

fn is_own(url: &str, own: &HashSet<(u64, String)>) -> bool {
    UgcUrl::parse(url).is_some_and(|u| own.contains(&(u.handle.0, u.sha1.to_ascii_uppercase())))
}

/// Runs the command: plan, confirmation, download, upload, rewrite.
#[allow(clippy::too_many_arguments)]
pub fn run(
    p: &Prepared,
    backend: &mut dyn CloudBackend,
    head: &dyn Fetch,
    download: &dyn Download,
    backups: &BackupStore,
    now: SystemTime,
    date: &str,
    confirm: Confirm<'_>,
    out: &mut dyn Write,
) -> Result<Status, String> {
    let config = &p.options.fetch;

    // 1. Select.
    let index = CloudIndex::load(&*backend).map_err(|e| e.to_string())?;
    let own = own_cloud(&index);
    let all = p.save.asset_urls();
    let total = all.len();
    let counts: HashMap<String, usize> = all.iter().map(|a| (a.url.clone(), a.count)).collect();
    let (own_urls, rest): (Vec<AssetUrl>, Vec<AssetUrl>) =
        all.into_iter().partition(|a| is_own(&a.url, &own));
    let plan = plan_fetch(rest, &p.store, config);
    let skipped = plan.skipped.len();
    let count_of = |url: &str| counts.get(url).copied().unwrap_or(1);

    // 2. Look up the sizes of what is not downloaded yet.
    let store_index = p.store.index();
    let urls: Vec<String> = plan.to_download.iter().map(|a| a.url.clone()).collect();
    let sizes = probe_sizes(&urls, head, config);
    let mut rows: Vec<Row> = Vec::new();
    for url in &plan.present {
        let entry = store_index
            .files
            .get(url)
            .cloned()
            .ok_or_else(|| format!("the download index lost {url}"))?;
        rows.push(Row {
            url: url.clone(),
            count: count_of(url),
            state: State::Present(entry),
        });
    }
    for asset in &plan.to_download {
        let size = sizes
            .get(&asset.url)
            .cloned()
            .unwrap_or_else(|| Size::Unknown("not probed".into()));
        rows.push(Row {
            url: asset.url.clone(),
            count: asset.count,
            state: State::New(size),
        });
    }
    rows.sort_by(|a, b| a.url.cmp(&b.url));

    let quota = cloud::read_quota(&*backend);
    write_plan(p, total, own_urls.len(), skipped, &rows, quota, out).map_err(io_err)?;
    out.flush().map_err(io_err)?;
    if rows.is_empty() {
        writeln!(out, "Nothing to rehost; nothing was changed.").map_err(io_err)?;
        return Ok(Status::Clean);
    }

    // The question comes once, before anything is downloaded.
    if p.options.dry_run {
        writeln!(out, "Dry run: nothing was changed.").map_err(io_err)?;
        return Ok(Status::Clean);
    }
    let prompt = format!(
        "Download {} file(s), upload them to {} and write the new save?",
        rows.len(),
        p.cloud_folder
    );
    if !p.options.yes && !confirm(&prompt).map_err(|e| e.to_string())? {
        writeln!(out, "Cancelled: nothing was changed.").map_err(io_err)?;
        return Ok(Status::Clean);
    }

    // 3. Download.
    let report = fetch_assets_with_plan(plan, download, None, &p.store, config);
    let mut stored: BTreeMap<String, Stored> = BTreeMap::new();
    for row in &rows {
        if let State::Present(entry) = &row.state {
            stored.insert(
                row.url.clone(),
                Stored {
                    file: entry.file.clone(),
                },
            );
        }
    }
    for f in &report.downloaded {
        stored.insert(
            f.url.clone(),
            Stored {
                file: f.file.clone(),
            },
        );
    }
    let mut problems = !report.failed.is_empty();
    for f in &report.failed {
        writeln!(out, "failed: {}: {}", f.asset.url, f.reason).map_err(io_err)?;
    }
    if let Some(e) = &report.index_error {
        problems = true;
        writeln!(out, "error: cannot write the download index: {e}").map_err(io_err)?;
    }
    writeln!(
        out,
        "Downloaded {}, failed {}.",
        report.downloaded.len(),
        report.failed.len()
    )
    .map_err(io_err)?;
    if stored.is_empty() {
        writeln!(out, "Nothing could be downloaded; nothing was uploaded.").map_err(io_err)?;
        return Ok(Status::Problems);
    }

    // 4. Upload. Identical content has one file name, so it is uploaded once.
    let mut local: BTreeMap<String, LocalFile> = BTreeMap::new();
    for s in stored.values() {
        if local.contains_key(&s.file) {
            continue;
        }
        let bytes = std::fs::read(p.local_dir.join(&s.file))
            .map_err(|e| format!("cannot read {}: {e}", p.local_dir.join(&s.file).display()))?;
        local.insert(
            s.file.clone(),
            LocalFile {
                path: s.file.clone(),
                size: bytes.len() as u64,
                sha1: sha1_hex(&bytes),
            },
        );
    }
    let local: Vec<LocalFile> = local.into_values().collect();
    let local_dir = p.local_dir.clone();
    cloud::sync_push(
        backend,
        backups,
        &local,
        &move |rel| std::fs::read(local_dir.join(rel)),
        &p.cloud_folder,
        false,
        &IgnoreSet::empty(),
        0,
        None,
        RunMode {
            dry_run: false,
            yes: true,
            json: false,
        },
        now,
        date,
        confirm,
        out,
    )
    .map_err(|e| e.to_string())?;

    // The shared URL of every uploaded file.
    let index = CloudIndex::load(&*backend).map_err(|e| e.to_string())?;
    let cloud_dir = normalize_folder(&format!("{}/files", p.cloud_folder));
    let mut entries: Vec<(&LocalFile, &ttskit_cloud::IndexEntry)> = Vec::new();
    let mut url_of: HashMap<&str, &str> = HashMap::new();
    for file in &local {
        let name = file_name(&file.path);
        let entry = index.entries.iter().find(|e| {
            e.name.eq_ignore_ascii_case(name)
                && normalize_folder(&e.folder).eq_ignore_ascii_case(&cloud_dir)
                && e.sha1().eq_ignore_ascii_case(&file.sha1)
                && e.url.is_some()
        });
        let Some(entry) = entry else {
            return Err(format!(
                "{name} was uploaded but has no shared URL in the cloud index; run the command again"
            ));
        };
        if let Some(url) = &entry.url {
            url_of.insert(&file.path, url);
        }
        entries.push((file, entry));
    }

    // 5. Manifest. A failure is reported after the save is written.
    let manifest_error = cloud::update_manifest(&p.manifest_file, "rehost", |m| {
        for (file, entry) in &entries {
            let path = format!("{}/{}", p.sub, file.path);
            if let Some(e) = ManifestEntry::from_index(entry, Some(path)) {
                m.upsert(e);
            }
        }
        Ok(())
    })
    .err();
    match &manifest_error {
        None => writeln!(out, "Updated manifest {}.", p.manifest_file.display()),
        Some(_) => Ok(()),
    }
    .map_err(io_err)?;

    // 6. Rewrite the save.
    let rules: Vec<UrlRule> = stored
        .iter()
        .filter_map(|(url, s)| {
            url_of.get(s.file.as_str()).map(|new| UrlRule {
                old: url.clone(),
                new: (*new).to_owned(),
                origin: "rehost".into(),
            })
        })
        .collect();
    let rules = UrlRules::new(rules).map_err(|e| format!("{e}; the save was not changed"))?;
    let (new_save, report) = p.save.rewrite_urls(
        &rules,
        RewriteOptions {
            normalize_steam_hosts: false,
            in_scripts: p.options.in_scripts,
        },
    );
    let new_bytes = new_save.to_vec();
    let replaced = report.values_changed() + report.scripts_changed();
    if report.scripts_skipped() > 0 {
        writeln!(
            out,
            "{} URLs in scripts were left alone; use --in-scripts to replace them",
            report.scripts_skipped()
        )
        .map_err(io_err)?;
    }
    match &p.destination {
        Some(dest) if new_bytes != p.data => {
            write_atomic(dest, &new_bytes)?;
            writeln!(
                out,
                "Replaced {replaced} URLs, wrote {}",
                dest.display().to_string().replace('\\', "/")
            )
            .map_err(io_err)?;
        }
        _ => {
            writeln!(
                out,
                "Nothing to replace; {} was not changed.",
                p.options.save.display()
            )
            .map_err(io_err)?;
        }
    }
    if let Some(e) = manifest_error {
        return Err(e.to_string());
    }
    if problems {
        writeln!(
            out,
            "Some URLs could not be downloaded and stay as they are in the save; fix them and run the command again."
        )
        .map_err(io_err)?;
        return Ok(Status::Problems);
    }
    Ok(Status::Clean)
}

#[cfg(test)]
mod tests;
