//! Looking up the size of remote files with `HEAD`, without downloading them.
//!
//! [`probe_sizes`] answers "how big is this URL?" for a plan that is shown
//! before anything is downloaded. A host that does not send a usable
//! `Content-Length` (or refuses `HEAD`) gives [`Size::Unknown`] with the
//! reason; that is never an error, because the real download decides.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use crate::check::{
    Fetch, FetchError, FetchMethod, FetchRequest, FetchResponse, HostGate, is_redirect,
    is_retryable, resolve_location,
};
use crate::save::assets::host_of;

use super::FetchConfig;

/// The size of one remote file, as far as `HEAD` tells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Size {
    /// The `Content-Length` of the final answer.
    Known(u64),
    /// No usable size; the text says why.
    Unknown(String),
}

/// Probes every URL in `urls` with `HEAD` (following redirects, retrying like
/// downloads do) and returns the sizes by URL. Uses the concurrency, per-host,
/// retry, backoff and redirect settings of `config`.
pub fn probe_sizes(
    urls: &[String],
    client: &dyn Fetch,
    config: &FetchConfig,
) -> BTreeMap<String, Size> {
    let gate = HostGate::new(config.per_host.max(1));
    let queue: Mutex<VecDeque<&String>> = Mutex::new(urls.iter().collect());
    let results: Mutex<BTreeMap<String, Size>> = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..config.concurrency.max(1) {
            scope.spawn(|| {
                loop {
                    let Some(url) = queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .pop_front()
                    else {
                        break;
                    };
                    let size = probe_one(url, client, config, &gate);
                    results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(url.clone(), size);
                }
            });
        }
    });
    results.into_inner().unwrap_or_else(PoisonError::into_inner)
}

fn probe_one(url: &str, client: &dyn Fetch, config: &FetchConfig, gate: &HostGate) -> Size {
    let _permit = gate.acquire(&host_of(url));
    let mut current = url.to_owned();
    let mut hops = 0;
    loop {
        let response = match head_with_retries(&current, client, config) {
            Ok(r) => r,
            Err(FetchError::Timeout) => return Size::Unknown("timed out".into()),
            Err(FetchError::Network(m)) => return Size::Unknown(m),
        };
        if is_redirect(response.status) {
            let Some(next) = response
                .location
                .as_deref()
                .and_then(|l| resolve_location(&current, l))
            else {
                return Size::Unknown(format!(
                    "redirect ({}) without a usable Location",
                    response.status
                ));
            };
            hops += 1;
            if hops > config.max_redirects {
                return Size::Unknown("too many redirects".into());
            }
            current = next;
            continue;
        }
        if !(200..300).contains(&response.status) {
            return Size::Unknown(format!("HTTP {}", response.status));
        }
        return match response.content_length {
            Some(n) => Size::Known(n),
            None => Size::Unknown("no Content-Length".into()),
        };
    }
}

fn head_with_retries(
    url: &str,
    client: &dyn Fetch,
    config: &FetchConfig,
) -> Result<FetchResponse, FetchError> {
    let mut n = 0;
    loop {
        let result = client.fetch(&FetchRequest {
            url,
            method: FetchMethod::Head,
        });
        let retry = match &result {
            Ok(r) => is_retryable(r.status),
            Err(_) => true,
        };
        if !retry || n >= config.retries {
            return result;
        }
        n += 1;
        let wait = config.backoff * n;
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;

    type Reply = Result<FetchResponse, FetchError>;

    /// Scripted `HEAD` client: each URL has a queue of replies, the last one
    /// repeats; an unscripted URL fails the test.
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
    }

    impl Fetch for Mock {
        fn fetch(&self, request: &FetchRequest<'_>) -> Reply {
            assert_eq!(request.method, FetchMethod::Head);
            self.calls.lock().unwrap().push(request.url.to_owned());
            let mut table = self.replies.lock().unwrap();
            let queue = table
                .get_mut(request.url)
                .unwrap_or_else(|| panic!("unscripted {}", request.url));
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue[0].clone()
            }
        }
    }

    fn answer(status: u16, length: Option<u64>) -> Reply {
        Ok(FetchResponse {
            status,
            content_length: length,
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

    fn config() -> FetchConfig {
        FetchConfig {
            backoff: Duration::ZERO,
            ..FetchConfig::default()
        }
    }

    fn probe(urls: &[&str], mock: &Mock) -> BTreeMap<String, Size> {
        let urls: Vec<String> = urls.iter().map(|u| (*u).to_owned()).collect();
        probe_sizes(&urls, mock, &config())
    }

    #[test]
    fn known_and_unknown_sizes() {
        let mock = Mock::default()
            .on("http://a.test/1", vec![answer(200, Some(1234))])
            .on("http://a.test/2", vec![answer(200, None)])
            .on("http://a.test/3", vec![answer(404, Some(10))])
            .on("http://a.test/4", vec![answer(405, None)])
            .on(
                "http://a.test/5",
                vec![Err(FetchError::Network("refused".into()))],
            );
        let got = probe(
            &[
                "http://a.test/1",
                "http://a.test/2",
                "http://a.test/3",
                "http://a.test/4",
                "http://a.test/5",
            ],
            &mock,
        );
        assert_eq!(got["http://a.test/1"], Size::Known(1234));
        assert_eq!(
            got["http://a.test/2"],
            Size::Unknown("no Content-Length".into())
        );
        assert_eq!(got["http://a.test/3"], Size::Unknown("HTTP 404".into()));
        assert_eq!(got["http://a.test/4"], Size::Unknown("HTTP 405".into()));
        assert_eq!(got["http://a.test/5"], Size::Unknown("refused".into()));
    }

    #[test]
    fn follows_redirects_and_gives_up_on_loops() {
        let mock = Mock::default()
            .on("http://a.test/old", vec![redirect("/new")])
            .on("http://a.test/new", vec![answer(200, Some(7))])
            .on("http://a.test/loop", vec![redirect("/loop")]);
        let got = probe(&["http://a.test/old", "http://a.test/loop"], &mock);
        assert_eq!(got["http://a.test/old"], Size::Known(7));
        assert_eq!(
            got["http://a.test/loop"],
            Size::Unknown("too many redirects".into())
        );
    }

    #[test]
    fn retries_server_errors_then_gives_up() {
        let mock = Mock::default()
            .on(
                "http://a.test/flaky",
                vec![answer(503, None), answer(200, Some(5))],
            )
            .on("http://a.test/down", vec![answer(500, None)]);
        let got = probe(&["http://a.test/flaky", "http://a.test/down"], &mock);
        assert_eq!(got["http://a.test/flaky"], Size::Known(5));
        assert_eq!(got["http://a.test/down"], Size::Unknown("HTTP 500".into()));
        assert_eq!(mock.count("http://a.test/down"), 4);
    }
}
