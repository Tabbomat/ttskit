use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::save::Occurrence;

type Reply = Result<FetchResponse, FetchError>;

/// Scripted client: each URL has a queue of replies per method; the last
/// reply repeats. An unscripted URL or method fails the test.
#[derive(Default)]
struct Mock {
    head: Mutex<HashMap<String, Vec<Reply>>>,
    get: Mutex<HashMap<String, Vec<Reply>>>,
    calls: Mutex<Vec<(String, FetchMethod)>>,
}

impl Mock {
    fn head(self, url: &str, replies: Vec<Reply>) -> Self {
        self.head.lock().unwrap().insert(url.into(), replies);
        self
    }
    fn get(self, url: &str, replies: Vec<Reply>) -> Self {
        self.get.lock().unwrap().insert(url.into(), replies);
        self
    }
    fn count(&self, url: &str, head: bool) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(u, m)| u == url && (*m == FetchMethod::Head) == head)
            .count()
    }
}

impl Fetch for Mock {
    fn fetch(&self, request: &FetchRequest<'_>) -> Reply {
        self.calls
            .lock()
            .unwrap()
            .push((request.url.to_owned(), request.method));
        let table = match request.method {
            FetchMethod::Head => &self.head,
            FetchMethod::GetRange { .. } => &self.get,
        };
        let mut table = table.lock().unwrap();
        let queue = table
            .get_mut(request.url)
            .unwrap_or_else(|| panic!("unscripted {:?} {}", request.method, request.url));
        if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue[0].clone()
        }
    }
}

fn status(status: u16) -> Reply {
    Ok(FetchResponse {
        status,
        ..FetchResponse::default()
    })
}

fn redirect(to: &str) -> Reply {
    Ok(FetchResponse {
        status: 302,
        location: Some(to.into()),
        ..FetchResponse::default()
    })
}

fn body(status: u16, content_type: &str, body: &[u8]) -> Reply {
    Ok(FetchResponse {
        status,
        content_type: Some(content_type.into()),
        body: body.to_vec(),
        ..FetchResponse::default()
    })
}

fn asset(url: &str) -> AssetUrl {
    AssetUrl {
        url: url.into(),
        labels: vec!["FaceURL".into()],
        count: 1,
        occurrences: vec![Occurrence {
            path: "ObjectStates[0].FaceURL".into(),
            nickname: Some("Card".into()),
            guid: Some("abc123".into()),
        }],
    }
}

fn config() -> CheckConfig {
    CheckConfig {
        backoff: Duration::ZERO,
        concurrency: 4,
        ..CheckConfig::default()
    }
}

fn run(urls: &[&str], mock: &Mock, config: &CheckConfig) -> Vec<(String, Outcome)> {
    let report = check_urls(urls.iter().map(|u| asset(u)).collect(), mock, config);
    report
        .results
        .into_iter()
        .map(|r| (r.asset.url, r.outcome))
        .collect()
}

fn one(url: &str, mock: &Mock, config: &CheckConfig) -> Outcome {
    run(&[url], mock, config).remove(0).1
}

#[test]
fn ok_dead_and_unreachable() {
    let mock = Mock::default()
        .head("http://a.test/ok", vec![status(200)])
        .head("http://a.test/gone", vec![status(404)])
        .head(
            "http://a.test/down",
            vec![Err(FetchError::Network("refused".into()))],
        );
    let got = run(
        &[
            "http://a.test/ok",
            "http://a.test/gone",
            "http://a.test/down",
        ],
        &mock,
        &config(),
    );
    assert_eq!(
        got[1].1,
        Outcome::Dead {
            status: 404,
            detail: None
        }
    );
    assert_eq!(
        got[0].1,
        Outcome::Unreachable {
            reason: "refused".into()
        }
    );
    assert_eq!(got[2].1, Outcome::Ok { status: 200 });
    // sorted by URL: down < gone < ok
    let urls: Vec<_> = got.iter().map(|(u, _)| u.as_str()).collect();
    assert_eq!(
        urls,
        [
            "http://a.test/down",
            "http://a.test/gone",
            "http://a.test/ok"
        ]
    );
}

#[test]
fn results_are_sorted_and_keep_occurrences() {
    let mock = Mock::default()
        .head("http://b.test/2", vec![status(200)])
        .head("http://a.test/1", vec![status(200)]);
    let report = check_urls(
        vec![asset("http://b.test/2"), asset("http://a.test/1")],
        &mock,
        &config(),
    );
    assert_eq!(report.results[0].asset.url, "http://a.test/1");
    assert_eq!(
        report.results[0].asset.occurrences[0].guid.as_deref(),
        Some("abc123")
    );
    assert!(!report.has_problems());
}

#[test]
fn redirect_to_live_target_is_not_a_problem() {
    let mock = Mock::default()
        .head("http://a.test/old", vec![redirect("/new")])
        .head("http://a.test/new", vec![status(200)]);
    let outcome = one("http://a.test/old", &mock, &config());
    assert_eq!(
        outcome,
        Outcome::Redirect {
            status: 200,
            target: "http://a.test/new".into()
        }
    );
    assert!(!outcome.is_problem());
}

#[test]
fn redirect_to_dead_target_is_dead() {
    let mock = Mock::default()
        .head("http://a.test/old", vec![redirect("http://b.test/x")])
        .head("http://b.test/x", vec![status(410)]);
    assert_eq!(
        one("http://a.test/old", &mock, &config()),
        Outcome::Dead {
            status: 410,
            detail: None
        }
    );
}

#[test]
fn redirect_loop_and_missing_location_are_unreachable() {
    let mock = Mock::default()
        .head("http://a.test/a", vec![redirect("/b")])
        .head("http://a.test/b", vec![redirect("/a")])
        .head("http://a.test/nolocation", vec![status(301)]);
    assert_eq!(
        one("http://a.test/a", &mock, &config()),
        Outcome::Unreachable {
            reason: "too many redirects".into()
        }
    );
    assert!(matches!(
        one("http://a.test/nolocation", &mock, &config()),
        Outcome::Unreachable { .. }
    ));
}

#[test]
fn location_resolution() {
    let base = "https://h.test/dir/file.png?x=1";
    assert_eq!(
        resolve_location(base, "https://o.test/a").as_deref(),
        Some("https://o.test/a")
    );
    assert_eq!(
        resolve_location(base, "//o.test/a").as_deref(),
        Some("https://o.test/a")
    );
    assert_eq!(
        resolve_location(base, "/root").as_deref(),
        Some("https://h.test/root")
    );
    assert_eq!(
        resolve_location(base, "other.png").as_deref(),
        Some("https://h.test/dir/other.png")
    );
    assert_eq!(
        resolve_location("http://h.test", "x").as_deref(),
        Some("http://h.test/x")
    );
    assert_eq!(resolve_location(base, "ftp://o.test/a"), None);
    assert_eq!(resolve_location(base, " "), None);
}

#[test]
fn head_refusal_falls_back_to_ranged_get_and_is_remembered() {
    let mock = Mock::default()
        .head("http://a.test/1", vec![status(405)])
        .get("http://a.test/1", vec![status(206)])
        .get("http://a.test/2", vec![status(200)]);
    let cfg = CheckConfig {
        concurrency: 1,
        ..config()
    };
    let got = run(&["http://a.test/1", "http://a.test/2"], &mock, &cfg);
    assert_eq!(got[0].1, Outcome::Ok { status: 206 });
    assert_eq!(got[1].1, Outcome::Ok { status: 200 });
    // The second URL on the same host went straight to GET.
    assert_eq!(mock.count("http://a.test/1", true), 1);
    assert_eq!(mock.count("http://a.test/2", true), 0);
    assert_eq!(mock.count("http://a.test/2", false), 1);
}

#[test]
fn get_after_head_refusal_can_still_be_dead() {
    let mock = Mock::default()
        .head("http://a.test/1", vec![status(403)])
        .get("http://a.test/1", vec![status(404)]);
    assert_eq!(
        one("http://a.test/1", &mock, &config()),
        Outcome::Dead {
            status: 404,
            detail: None
        }
    );
}

#[test]
fn not_found_on_head_is_trusted() {
    let mock = Mock::default().head("http://a.test/1", vec![status(404)]);
    one("http://a.test/1", &mock, &config());
    assert_eq!(mock.count("http://a.test/1", false), 0);
}

#[test]
fn retries_server_errors_then_succeeds() {
    let mock = Mock::default().head(
        "http://a.test/1",
        vec![status(503), status(429), status(200)],
    );
    assert_eq!(
        one("http://a.test/1", &mock, &config()),
        Outcome::Ok { status: 200 }
    );
    assert_eq!(mock.count("http://a.test/1", true), 3);
}

#[test]
fn gives_up_after_the_configured_retries() {
    let mock = Mock::default().head("http://a.test/1", vec![status(500)]);
    let cfg = CheckConfig {
        retries: 2,
        ..config()
    };
    assert_eq!(
        one("http://a.test/1", &mock, &cfg),
        Outcome::Dead {
            status: 500,
            detail: None
        }
    );
    assert_eq!(mock.count("http://a.test/1", true), 3);
}

#[test]
fn zero_retries_means_one_attempt() {
    let mock = Mock::default().head("http://a.test/1", vec![Err(FetchError::Timeout)]);
    let cfg = CheckConfig {
        retries: 0,
        ..config()
    };
    one("http://a.test/1", &mock, &cfg);
    assert_eq!(mock.count("http://a.test/1", true), 1);
}

#[test]
fn timeouts_are_retried_and_end_unreachable() {
    let mock = Mock::default().head("http://a.test/1", vec![Err(FetchError::Timeout)]);
    assert_eq!(
        one("http://a.test/1", &mock, &config()),
        Outcome::Unreachable {
            reason: "timed out".into()
        }
    );
    assert_eq!(mock.count("http://a.test/1", true), 4);
}

#[test]
fn a_timeout_followed_by_success_is_ok() {
    let mock = Mock::default().head(
        "http://a.test/1",
        vec![Err(FetchError::Timeout), status(200)],
    );
    assert_eq!(
        one("http://a.test/1", &mock, &config()),
        Outcome::Ok { status: 200 }
    );
}

#[test]
fn client_errors_are_not_retried() {
    let mock = Mock::default().head("http://a.test/1", vec![status(404)]);
    one("http://a.test/1", &mock, &config());
    assert_eq!(mock.count("http://a.test/1", true), 1);
}

#[test]
fn host_filters_skip_without_requests() {
    let mock = Mock::default().head("http://keep.test/1", vec![status(200)]);
    let cfg = CheckConfig {
        only_hosts: vec!["keep.test".into()],
        skip_hosts: vec!["x.keep.test".into()],
        ..config()
    };
    let got = run(
        &[
            "http://keep.test/1",
            "http://other.test/1",
            "http://x.keep.test/1",
            "ftp://keep.test/1",
        ],
        &mock,
        &cfg,
    );
    let by: HashMap<_, _> = got.into_iter().collect();
    assert_eq!(by["http://keep.test/1"], Outcome::Ok { status: 200 });
    for skipped in [
        "http://other.test/1",
        "http://x.keep.test/1",
        "ftp://keep.test/1",
    ] {
        assert!(matches!(by[skipped], Outcome::Skipped { .. }), "{skipped}");
    }
    assert_eq!(mock.calls.lock().unwrap().len(), 1);
    assert!(
        !Outcome::Skipped {
            reason: String::new()
        }
        .is_problem()
    );
}

#[test]
fn skip_host_matches_subdomains_and_ignores_case() {
    let mock = Mock::default();
    let cfg = CheckConfig {
        skip_hosts: vec!["Imgur.com".into()],
        ..config()
    };
    assert!(matches!(
        one("https://I.IMGUR.com/a.png", &mock, &cfg),
        Outcome::Skipped { .. }
    ));
    // `notimgur.com` is another host.
    let mock = Mock::default().head("https://notimgur.com/a", vec![status(200)]);
    assert_eq!(
        one("https://notimgur.com/a", &mock, &cfg),
        Outcome::Ok { status: 200 }
    );
}

#[test]
fn verify_content_rejects_html_with_status_200() {
    let mock = Mock::default()
        .get(
            "http://a.test/page",
            vec![body(200, "text/html; charset=utf-8", b"nope")],
        )
        .get(
            "http://a.test/sniffed",
            vec![body(
                200,
                "application/octet-stream",
                b"\n <!DOCTYPE HTML><p>gone",
            )],
        )
        .get(
            "http://a.test/png",
            vec![body(200, "image/png", b"\x89PNG\r\n\x1a\n")],
        )
        .get(
            "http://a.test/obj",
            vec![body(200, "text/plain", b"v 0 0 0\n")],
        );
    let cfg = CheckConfig {
        verify_content: true,
        ..config()
    };
    let got: HashMap<_, _> = run(
        &[
            "http://a.test/page",
            "http://a.test/sniffed",
            "http://a.test/png",
            "http://a.test/obj",
        ],
        &mock,
        &cfg,
    )
    .into_iter()
    .collect();
    assert_eq!(
        got["http://a.test/page"],
        Outcome::Dead {
            status: 200,
            detail: Some("content type is text/html".into())
        }
    );
    assert_eq!(
        got["http://a.test/sniffed"],
        Outcome::Dead {
            status: 200,
            detail: Some("body is an HTML page".into())
        }
    );
    assert_eq!(got["http://a.test/png"], Outcome::Ok { status: 200 });
    assert_eq!(got["http://a.test/obj"], Outcome::Ok { status: 200 });
    // Verification uses GET only.
    assert!(
        mock.calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, m)| *m != FetchMethod::Head)
    );
}

#[test]
fn html_is_fine_without_verify_content() {
    let mock = Mock::default().head("http://a.test/page", vec![body(200, "text/html", b"")]);
    assert_eq!(
        one("http://a.test/page", &mock, &config()),
        Outcome::Ok { status: 200 }
    );
}

#[test]
fn per_host_limit_is_respected() {
    struct Counting {
        now: AtomicUsize,
        peak: AtomicUsize,
    }
    impl Fetch for Counting {
        fn fetch(&self, _: &FetchRequest<'_>) -> Reply {
            let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
            self.now.fetch_sub(1, Ordering::SeqCst);
            status(200)
        }
    }
    let client = Counting {
        now: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    };
    let urls: Vec<_> = (0..12)
        .map(|i| asset(&format!("http://a.test/{i}")))
        .collect();
    let cfg = CheckConfig {
        concurrency: 8,
        per_host: 2,
        ..config()
    };
    let report = check_urls(urls, &client, &cfg);
    assert_eq!(report.results.len(), 12);
    assert!(client.peak.load(Ordering::SeqCst) <= 2);
}
