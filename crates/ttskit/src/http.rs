//! The blocking HTTP client shared by `save check-urls` and
//! `save fetch-assets`.

use std::time::Duration;

/// Longest a single download may take, from request to the last body byte.
/// The blocking `reqwest` client has no idle (read) timeout, only this
/// overall one, so it is generous: slow but steady downloads of large files
/// finish, and a stalled one is retried after this time at the latest.
pub const DOWNLOAD_TOTAL_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// A blocking `reqwest` client that never follows redirects (the engines do).
pub struct HttpClient {
    pub(crate) client: reqwest::blocking::Client,
}

impl HttpClient {
    /// A client for short requests: `timeout` covers each whole request.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        Self::build(reqwest::blocking::Client::builder().timeout(timeout))
    }

    /// A client for downloads of large files: `connect_timeout` limits
    /// establishing the connection; the whole request may take up to
    /// [`DOWNLOAD_TOTAL_TIMEOUT`].
    pub fn for_downloads(connect_timeout: Duration) -> Result<Self, String> {
        Self::build(
            reqwest::blocking::Client::builder()
                .connect_timeout(connect_timeout)
                .timeout(DOWNLOAD_TOTAL_TIMEOUT),
        )
    }

    fn build(builder: reqwest::blocking::ClientBuilder) -> Result<Self, String> {
        let client = builder
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("ttskit/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("cannot create the HTTP client: {}", describe(&e)))?;
        Ok(HttpClient { client })
    }
}

/// The error with its causes, since `reqwest` hides them behind a generic text.
pub fn describe(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

/// Like [`describe`], for any error.
pub fn describe_io(e: &std::io::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}
