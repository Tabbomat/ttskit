use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::save::Occurrence;

type Reply = Result<(DownloadResponse, Vec<u8>), DownloadError>;

/// Scripted client: each URL has a queue of replies; the last reply
/// repeats. An unscripted URL fails the test. Like a real client it writes
/// the body only for `2xx` and enforces the size limit.
#[derive(Default)]
struct Mock {
    replies: Mutex<HashMap<String, Vec<Reply>>>,
    calls: Mutex<Vec<String>>,
}

impl Mock {
    fn on(self, url: &str, replies: Vec<Reply>) -> Self {
        self.replies.lock().unwrap().insert(url.into(), replies);
        self
    }
    fn count(&self, url: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|u| *u == url)
            .count()
    }
    fn total(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Download for Mock {
    fn download(
        &self,
        url: &str,
        max_size: u64,
        sink: &mut dyn Write,
    ) -> Result<DownloadResponse, DownloadError> {
        self.calls.lock().unwrap().push(url.to_owned());
        let reply = {
            let mut table = self.replies.lock().unwrap();
            let queue = table
                .get_mut(url)
                .unwrap_or_else(|| panic!("unscripted {url}"));
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue[0].clone()
            }
        };
        let (response, body) = reply?;
        if (200..300).contains(&response.status) {
            if body.len() as u64 > max_size {
                return Err(DownloadError::TooLarge);
            }
            sink.write_all(&body)
                .map_err(|e| DownloadError::Io(e.to_string()))?;
        }
        Ok(response)
    }
}

fn ok(body: &[u8]) -> Reply {
    Ok((
        DownloadResponse {
            status: 200,
            ..DownloadResponse::default()
        },
        body.to_vec(),
    ))
}

fn ok_type(content_type: &str, body: &[u8]) -> Reply {
    Ok((
        DownloadResponse {
            status: 200,
            content_type: Some(content_type.into()),
            ..DownloadResponse::default()
        },
        body.to_vec(),
    ))
}

fn status(status: u16) -> Reply {
    Ok((
        DownloadResponse {
            status,
            ..DownloadResponse::default()
        },
        b"error body".to_vec(),
    ))
}

fn redirect(to: &str) -> Reply {
    Ok((
        DownloadResponse {
            status: 302,
            location: Some(to.into()),
            ..DownloadResponse::default()
        },
        Vec::new(),
    ))
}

fn asset(url: &str) -> AssetUrl {
    AssetUrl {
        url: url.into(),
        labels: vec!["FaceURL".into()],
        count: 1,
        occurrences: vec![Occurrence {
            path: "ObjectStates[0].FaceURL".into(),
            nickname: None,
            guid: None,
        }],
    }
}

fn assets(urls: &[&str]) -> Vec<AssetUrl> {
    urls.iter().map(|u| asset(u)).collect()
}

fn config() -> FetchConfig {
    FetchConfig {
        backoff: Duration::ZERO,
        concurrency: 4,
        ..FetchConfig::default()
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage data";

fn sha(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn run(dir: &Path, urls: &[&str], mock: &Mock, config: &FetchConfig) -> FetchReport {
    let store = Store::open(dir).unwrap();
    fetch_assets(assets(urls), mock, &store, config)
}

fn index_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("index.json")).unwrap()
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join("files"))
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn reason<'a>(report: &'a FetchReport, url: &str) -> &'a str {
    &report
        .failed
        .iter()
        .find(|f| f.asset.url == url)
        .unwrap_or_else(|| panic!("{url} did not fail"))
        .reason
}

#[test]
fn fresh_run_stores_files_and_a_sorted_index() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default()
        .on("http://b.test/2.png", vec![ok(PNG)])
        .on("http://a.test/1", vec![ok(b"%PDF-1.4 doc")]);
    let report = run(
        dir.path(),
        &["http://b.test/2.png", "http://a.test/1"],
        &mock,
        &config(),
    );
    assert!(!report.has_failures());
    assert_eq!(report.downloaded.len(), 2);
    assert_eq!(report.downloaded[0].url, "http://a.test/1");
    assert_eq!(report.downloaded[0].file_type, "pdf");
    let png_file = format!("{}.png", sha(PNG));
    assert_eq!(report.downloaded[1].file, format!("files/{png_file}"));
    assert_eq!(report.downloaded[1].size, PNG.len() as u64);
    assert_eq!(
        std::fs::read(dir.path().join("files").join(&png_file)).unwrap(),
        PNG
    );
    let text = index_text(dir.path());
    assert!(text.ends_with("}\n"));
    assert!(!text.contains('\r'));
    assert!(text.find("http://a.test/1").unwrap() < text.find("http://b.test/2.png").unwrap());
    let index = Index::parse(&text).unwrap();
    assert_eq!(index.files.len(), 2);
    assert_eq!(index.files["http://b.test/2.png"].sha256, sha(PNG));
    assert!(!dir.path().join(".partial").exists());
}

#[test]
fn index_is_byte_identical_across_runs_and_input_order() {
    let mock = || {
        Mock::default()
            .on("http://a.test/1", vec![ok(PNG)])
            .on("http://a.test/2", vec![ok(b"OggS data")])
            .on("http://a.test/3", vec![ok(b"plain")])
    };
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    run(
        one.path(),
        &["http://a.test/1", "http://a.test/2", "http://a.test/3"],
        &mock(),
        &config(),
    );
    run(
        two.path(),
        &["http://a.test/3", "http://a.test/1", "http://a.test/2"],
        &mock(),
        &FetchConfig {
            concurrency: 1,
            ..config()
        },
    );
    assert_eq!(index_text(one.path()), index_text(two.path()));
}

#[test]
fn same_content_is_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default()
        .on("http://a.test/x", vec![ok(PNG)])
        .on("http://b.test/y", vec![ok(PNG)]);
    let report = run(
        dir.path(),
        &["http://a.test/x", "http://b.test/y"],
        &mock,
        &config(),
    );
    assert!(!report.has_failures());
    assert_eq!(files_in(dir.path()).len(), 1);
    let index = Index::parse(&index_text(dir.path())).unwrap();
    assert_eq!(index.files.len(), 2);
    assert_eq!(
        index.files["http://a.test/x"].file,
        index.files["http://b.test/y"].file
    );
}

#[test]
fn second_run_makes_no_requests() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default()
        .on("http://a.test/1", vec![ok(PNG)])
        .on("http://a.test/2", vec![ok(b"data")]);
    let urls = ["http://a.test/1", "http://a.test/2"];
    run(dir.path(), &urls, &mock, &config());
    let before = index_text(dir.path());
    let calls = mock.total();
    let report = run(dir.path(), &urls, &mock, &config());
    assert_eq!(mock.total(), calls);
    assert_eq!(report.plan.present.len(), 2);
    assert!(report.downloaded.is_empty());
    assert_eq!(index_text(dir.path()), before);
}

#[test]
fn deleted_and_truncated_files_are_downloaded_again() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default()
        .on("http://a.test/1", vec![ok(PNG)])
        .on("http://a.test/2", vec![ok(b"some data")])
        .on("http://a.test/3", vec![ok(b"kept")]);
    let urls = ["http://a.test/1", "http://a.test/2", "http://a.test/3"];
    let first = run(dir.path(), &urls, &mock, &config());
    std::fs::remove_file(dir.path().join(&first.downloaded[0].file)).unwrap();
    std::fs::write(dir.path().join(&first.downloaded[1].file), b"so").unwrap();
    let report = run(dir.path(), &urls, &mock, &config());
    assert_eq!(report.plan.invalid.len(), 2);
    assert_eq!(report.plan.present, ["http://a.test/3"]);
    assert_eq!(report.downloaded.len(), 2);
    assert_eq!(mock.count("http://a.test/1"), 2);
    assert_eq!(mock.count("http://a.test/3"), 1);
    assert_eq!(
        std::fs::read(dir.path().join(&first.downloaded[1].file)).unwrap(),
        b"some data"
    );
}

#[test]
fn verify_detects_a_corrupted_file_of_the_same_size() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default().on("http://a.test/1", vec![ok(b"abcdef")]);
    let urls = ["http://a.test/1"];
    let first = run(dir.path(), &urls, &mock, &config());
    // Without verify the damage is not noticed.
    std::fs::write(dir.path().join(&first.downloaded[0].file), b"abcdeX").unwrap();
    let report = run(dir.path(), &urls, &mock, &config());
    assert_eq!(report.plan.present.len(), 1);
    assert_eq!(mock.count("http://a.test/1"), 1);
    // With verify it is.
    let verify = FetchConfig {
        verify: true,
        ..config()
    };
    let report = run(dir.path(), &urls, &mock, &verify);
    assert_eq!(report.plan.invalid.len(), 1);
    assert_eq!(report.plan.verified, 0);
    assert_eq!(mock.count("http://a.test/1"), 2);
    // Intact again: verified and not downloaded.
    let report = run(dir.path(), &urls, &mock, &verify);
    assert_eq!(report.plan.verified, 1);
    assert_eq!(mock.count("http://a.test/1"), 2);
}

#[test]
fn failures_are_listed_and_do_not_stop_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default()
        .on("http://a.test/good", vec![ok(PNG)])
        .on("http://a.test/gone", vec![status(404)])
        .on(
            "http://a.test/page",
            vec![ok_type("text/html; charset=utf-8", b"<p>nope</p>")],
        )
        .on(
            "http://a.test/doctype",
            vec![ok(b"  <!DOCTYPE html><html>")],
        )
        .on("http://a.test/empty", vec![ok(b"")])
        .on("http://a.test/big", vec![ok(&[7u8; 100])])
        .on(
            "http://a.test/down",
            vec![Err(DownloadError::Network("refused".into()))],
        )
        .on("http://a.test/slow", vec![Err(DownloadError::Timeout)]);
    let cfg = FetchConfig {
        max_size: 50,
        retries: 2,
        ..config()
    };
    let report = run(
        dir.path(),
        &[
            "http://a.test/good",
            "http://a.test/gone",
            "http://a.test/page",
            "http://a.test/doctype",
            "http://a.test/empty",
            "http://a.test/big",
            "http://a.test/down",
            "http://a.test/slow",
        ],
        &mock,
        &cfg,
    );
    assert!(report.has_failures());
    assert_eq!(report.downloaded.len(), 1);
    assert_eq!(report.failed.len(), 7);
    assert_eq!(reason(&report, "http://a.test/gone"), "HTTP 404");
    assert!(reason(&report, "http://a.test/page").starts_with("HTML page"));
    assert!(reason(&report, "http://a.test/doctype").starts_with("HTML page"));
    assert_eq!(
        reason(&report, "http://a.test/empty"),
        "empty response body"
    );
    assert!(reason(&report, "http://a.test/big").contains("size limit of 50 bytes"));
    assert_eq!(reason(&report, "http://a.test/down"), "refused");
    assert_eq!(reason(&report, "http://a.test/slow"), "timed out");
    assert_eq!(mock.count("http://a.test/down"), 3);
    assert_eq!(mock.count("http://a.test/gone"), 1);
    assert_eq!(mock.count("http://a.test/big"), 1);
    let urls: Vec<_> = report.failed.iter().map(|f| f.asset.url.clone()).collect();
    let mut sorted = urls.clone();
    sorted.sort();
    assert_eq!(urls, sorted);
    let index = Index::parse(&index_text(dir.path())).unwrap();
    assert_eq!(index.files.len(), 1);
    assert!(index.files.contains_key("http://a.test/good"));
    assert_eq!(files_in(dir.path()).len(), 1);
    assert!(!dir.path().join(".partial").exists());
}

#[test]
fn engine_enforces_the_size_limit_on_a_client_that_does_not() {
    struct Greedy;
    impl Download for Greedy {
        fn download(
            &self,
            _: &str,
            _: u64,
            sink: &mut dyn Write,
        ) -> Result<DownloadResponse, DownloadError> {
            sink.write_all(&[1u8; 100])
                .map_err(|e| DownloadError::Io(e.to_string()))?;
            Ok(DownloadResponse {
                status: 200,
                ..DownloadResponse::default()
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let cfg = FetchConfig {
        max_size: 10,
        ..config()
    };
    let report = fetch_assets(assets(&["http://a.test/x"]), &Greedy, &store, &cfg);
    assert!(reason(&report, "http://a.test/x").contains("size limit of 10 bytes"));
    assert!(!dir.path().join(".partial").exists());
    assert!(files_in(dir.path()).is_empty());
}

#[test]
fn redirects_are_followed() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default()
        .on("http://a.test/old", vec![redirect("/new")])
        .on(
            "http://a.test/new",
            vec![redirect("http://b.test/final.png")],
        )
        .on("http://b.test/final.png", vec![ok(PNG)])
        .on("http://a.test/loop", vec![redirect("/loop")])
        .on("http://a.test/nowhere", vec![status(302)]);
    let report = run(
        dir.path(),
        &[
            "http://a.test/old",
            "http://a.test/loop",
            "http://a.test/nowhere",
        ],
        &mock,
        &config(),
    );
    assert_eq!(report.downloaded.len(), 1);
    // Recorded under the URL the save uses, not the redirect target.
    assert_eq!(report.downloaded[0].url, "http://a.test/old");
    assert_eq!(report.downloaded[0].file_type, "png");
    assert_eq!(reason(&report, "http://a.test/loop"), "too many redirects");
    assert!(reason(&report, "http://a.test/nowhere").contains("without a usable Location"));
}

#[test]
fn retries_on_503_then_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default().on(
        "http://a.test/flaky",
        vec![
            status(503),
            Err(DownloadError::Network("reset".into())),
            ok(PNG),
        ],
    );
    let report = run(dir.path(), &["http://a.test/flaky"], &mock, &config());
    assert!(!report.has_failures());
    assert_eq!(mock.count("http://a.test/flaky"), 3);
    assert_eq!(files_in(dir.path()).len(), 1);
}

#[test]
fn persistent_503_fails_after_the_retries() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default().on("http://a.test/x", vec![status(503)]);
    let report = run(dir.path(), &["http://a.test/x"], &mock, &config());
    assert_eq!(reason(&report, "http://a.test/x"), "HTTP 503");
    assert_eq!(mock.count("http://a.test/x"), 4);
    assert!(!dir.path().join(".partial").exists());
}

#[test]
fn non_http_and_filtered_hosts_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default()
        .on("http://a.test/ok", vec![ok(PNG)])
        .on("http://sub.b.test/x", vec![ok(b"x")]);
    let cfg = FetchConfig {
        only_hosts: vec!["a.test".into(), "b.test".into()],
        skip_hosts: vec!["b.test".into()],
        ..config()
    };
    let report = run(
        dir.path(),
        &[
            "http://a.test/ok",
            "ftp://a.test/f",
            "file:///x",
            "http://sub.b.test/x",
            "http://c.test/y",
        ],
        &mock,
        &cfg,
    );
    assert!(!report.has_failures());
    assert_eq!(report.plan.total, 5);
    assert_eq!(report.plan.skipped.len(), 4);
    assert_eq!(report.downloaded.len(), 1);
    assert_eq!(mock.total(), 1);
}

#[test]
fn per_host_limit_is_respected() {
    struct Counting {
        now: AtomicUsize,
        peak: AtomicUsize,
    }
    impl Download for Counting {
        fn download(
            &self,
            url: &str,
            _: u64,
            sink: &mut dyn Write,
        ) -> Result<DownloadResponse, DownloadError> {
            let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
            self.now.fetch_sub(1, Ordering::SeqCst);
            sink.write_all(url.as_bytes()).unwrap();
            Ok(DownloadResponse {
                status: 200,
                ..DownloadResponse::default()
            })
        }
    }
    let client = Counting {
        now: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let urls: Vec<_> = (0..12)
        .map(|i| asset(&format!("http://a.test/{i}")))
        .collect();
    let cfg = FetchConfig {
        concurrency: 8,
        per_host: 2,
        ..config()
    };
    let report = fetch_assets(urls, &client, &store, &cfg);
    assert_eq!(report.downloaded.len(), 12);
    assert!(client.peak.load(Ordering::SeqCst) <= 2);
}

#[test]
fn plan_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("out")).unwrap();
    let p = plan(
        assets(&["http://b.test/2", "http://a.test/1", "mailto:x"]),
        &store,
        &config(),
    );
    assert_eq!(p.total, 3);
    assert_eq!(p.skipped.len(), 1);
    assert!(p.present.is_empty());
    let urls: Vec<_> = p.to_download.iter().map(|a| a.url.as_str()).collect();
    assert_eq!(urls, ["http://a.test/1", "http://b.test/2"]);
    assert!(!dir.path().join("out").exists());
}

#[test]
fn missing_index_is_empty_and_bad_index_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Store::open(dir.path()).unwrap().index().files.is_empty());
    let path = dir.path().join("index.json");
    for bad in [
        "not json",
        "[]",
        r#"{"version":2,"files":[]}"#,
        r#"{"files":[]}"#,
        r#"{"version":1}"#,
        r#"{"version":1,"files":[{"url":"u"}]}"#,
        r#"{"version":1,"files":[{"url":"u","file":"../x","size":1,"sha256":"a","type":"png"}]}"#,
        r#"{"version":1,"files":[{"url":"u","file":"files/../x","size":1,"sha256":"a","type":"png"}]}"#,
    ] {
        std::fs::write(&path, bad).unwrap();
        let err = Store::open(dir.path()).unwrap_err();
        assert!(matches!(err, StoreError::Invalid { .. }), "{bad}: {err}");
    }
}

#[test]
fn entries_of_other_saves_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default()
        .on("http://a.test/save1", vec![ok(PNG)])
        .on("http://a.test/save2", vec![ok(b"other")]);
    run(dir.path(), &["http://a.test/save1"], &mock, &config());
    run(dir.path(), &["http://a.test/save2"], &mock, &config());
    let index = Index::parse(&index_text(dir.path())).unwrap();
    assert_eq!(index.files.len(), 2);
    assert_eq!(files_in(dir.path()).len(), 2);
}

#[test]
fn leftover_partial_files_are_cleaned() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".partial")).unwrap();
    std::fs::write(dir.path().join(".partial/1-0.part"), b"junk").unwrap();
    let mock = Mock::default().on("http://a.test/1", vec![ok(PNG)]);
    run(dir.path(), &["http://a.test/1"], &mock, &config());
    assert!(!dir.path().join(".partial").exists());
}

#[test]
fn index_is_saved_every_fifty_downloads() {
    struct Peek<'a> {
        dir: &'a Path,
        calls: AtomicUsize,
        seen: AtomicUsize,
    }
    impl Download for Peek<'_> {
        fn download(
            &self,
            url: &str,
            _: u64,
            sink: &mut dyn Write,
        ) -> Result<DownloadResponse, DownloadError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 55 {
                // Single worker: 54 downloads are done, 50 of them saved.
                let text = std::fs::read_to_string(self.dir.join("index.json")).unwrap();
                let seen = Index::parse(&text).unwrap().files.len();
                self.seen.store(seen, Ordering::SeqCst);
            }
            sink.write_all(url.as_bytes()).unwrap();
            Ok(DownloadResponse {
                status: 200,
                ..DownloadResponse::default()
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let client = Peek {
        dir: dir.path(),
        calls: AtomicUsize::new(0),
        seen: AtomicUsize::new(0),
    };
    let list: Vec<AssetUrl> = (0..60)
        .map(|i| asset(&format!("http://a.test/{i}")))
        .collect();
    let cfg = FetchConfig {
        concurrency: 1,
        ..config()
    };
    let report = fetch_assets(list, &client, &store, &cfg);
    assert_eq!(report.downloaded.len(), 60);
    assert_eq!(client.seen.load(Ordering::SeqCst), 50);
    let index = Index::parse(&index_text(dir.path())).unwrap();
    assert_eq!(index.files.len(), 60);
}

#[test]
fn detect_type_table() {
    let cases: &[(&[u8], Option<&str>, &str, &str)] = &[
        (b"\x89PNG\r\n\x1a\n", None, "http://x/a", "png"),
        (&[0xFF, 0xD8, 0xFF, 0xE0], None, "http://x/a", "jpg"),
        (b"GIF89a..", None, "http://x/a", "gif"),
        (b"RIFF\0\0\0\0WEBPVP8 ", None, "http://x/a", "webp"),
        (b"RIFF\0\0\0\0WAVEfmt ", None, "http://x/a", "wav"),
        (b"%PDF-1.7", None, "http://x/a", "pdf"),
        (b"ID3\x03", None, "http://x/a", "mp3"),
        (&[0xFF, 0xFB, 0x90], None, "http://x/a", "mp3"),
        (b"OggS\0", None, "http://x/a", "ogg"),
        (b"\0\0\0\x18ftypmp42", None, "http://x/a", "mp4"),
        (b"UnityFS\0", None, "http://x/a", "unity3d"),
        (b"UnityWeb\0", None, "http://x/a", "unity3d"),
        (b"UnityRaw\0", None, "http://x/a", "unity3d"),
        (b"zz", Some("image/png"), "http://x/a", "png"),
        (b"zz", Some("image/jpeg; q=1"), "http://x/a", "jpg"),
        (b"zz", Some("audio/mpeg"), "http://x/a", "mp3"),
        (b"zz", Some("application/pdf"), "http://x/a", "pdf"),
        (b"zz", Some("model/obj"), "http://x/a", "obj"),
        (b"zz", Some("text/plain"), "http://x/m.obj", "obj"),
        (b"zz", Some("text/plain"), "http://x/m.txt", "txt"),
        (b"zz", Some("text/plain"), "http://x/m", "bin"),
        (b"zz", None, "http://x/m.OBJ?v=1#f", "obj"),
        (b"zz", None, "http://x/m.jpeg", "jpg"),
        (b"zz", None, "http://x/m.unity3d", "unity3d"),
        (b"zz", None, "http://x/m.exe", "bin"),
        (b"zz", None, "http://x.png/m", "bin"),
        (b"zz", None, "http://x/dir.png/m", "bin"),
        (b"", None, "http://x/", "bin"),
        // magic bytes win over the Content-Type and the URL
        (
            b"\x89PNG\r\n\x1a\n",
            Some("image/jpeg"),
            "http://x/a.gif",
            "png",
        ),
        // a Content-Type wins over the URL
        (b"zz", Some("image/gif"), "http://x/a.png", "gif"),
    ];
    for (prefix, ct, url, want) in cases {
        assert_eq!(detect_type(prefix, *ct, url), *want, "{ct:?} {url}");
    }
}

#[test]
fn running_a_precomputed_plan_matches_fetch_assets() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default().on("http://a.test/1", vec![ok(PNG)]);
    let store = Store::open(dir.path()).unwrap();
    let p = plan(assets(&["http://a.test/1"]), &store, &config());
    assert_eq!(p.to_download.len(), 1);
    let report = fetch_assets_with_plan(p.clone(), &mock, &store, &config());
    assert_eq!(report.plan, p);
    assert_eq!(report.downloaded.len(), 1);
    assert!(index_text(dir.path()).contains("http://a.test/1"));
}
