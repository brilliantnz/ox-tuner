//! A blocking HTTP client built on `reqwest`.
//!
//! This is the only network code in the crate, and it blocks the calling
//! thread on purpose: the caller runs it on a worker thread (GPUI's background
//! executor or a dedicated `std::thread`), never on the core or I/O thread and
//! never on the UI thread. The tokio runtime `reqwest` keeps inside stays
//! within that worker call, the same way async-only adapters own their runtime
//! on a separate thread.

use std::time::Duration;

use reqwest::{blocking::Client, redirect::Policy};

use crate::{
    error::{ErrorKind, RegistryError},
    fetch::{HttpGet, MAX_REDIRECTS, check_url},
};

/// How long one download may take, when the caller does not say otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Fetches URLs with `reqwest` blocking calls.
#[derive(Debug, Clone)]
pub struct BlockingClient {
    client: Client,
}

impl BlockingClient {
    /// Builds a client that fails any download past `timeout`.
    ///
    /// The client follows at most [`MAX_REDIRECTS`] redirects and only to
    /// `https://` URLs. Anything else stops the fetch.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::Network`] when the client cannot be built.
    pub fn new(timeout: Duration) -> Result<Self, RegistryError> {
        let policy = Policy::custom(|attempt| {
            let under_limit = attempt.previous().len() < usize::from(MAX_REDIRECTS);

            if under_limit && check_url(attempt.url().as_str()).is_ok() {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let client = Client::builder()
            .timeout(timeout)
            .redirect(policy)
            .build()
            .map_err(|error| RegistryError::new(ErrorKind::Network, error.to_string()))?;

        Ok(Self { client })
    }
}

impl HttpGet for BlockingClient {
    fn fetch(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, RegistryError> {
        let started = checked_start(url, "starting a fetch")?;

        let result = self.fetch_once(url, max_bytes);
        let duration_ms = elapsed_ms(&started);
        log_outcome(url, "fetch", &result, duration_ms);

        result
    }

    fn download(
        &self,
        url: &str,
        max_bytes: usize,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Vec<u8>, RegistryError> {
        let started = checked_start(url, "starting a download")?;

        let result = self.download_chunks(url, max_bytes, progress);
        let duration_ms = elapsed_ms(&started);
        log_outcome(url, "download", &result, duration_ms);

        result
    }
}

/// Refuses a URL outside the policy, with the refusal on the record.
fn checked_start(url: &str, message: &str) -> Result<std::time::Instant, RegistryError> {
    if let Err(error) = check_url(url) {
        tracing::warn!(
            op = "registry-fetch",
            url,
            code = error.kind().as_str(),
            reason = error.to_string(),
            "a download was refused"
        );

        return Err(error);
    }

    tracing::debug!(op = "registry-fetch", url, "{message}");

    Ok(std::time::Instant::now())
}

/// The outcome of one fetch or download, on the record the Diagnostics tab
/// filters and a future notification policy reads.
fn log_outcome(url: &str, verb: &str, result: &Result<Vec<u8>, RegistryError>, duration_ms: u64) {
    match result {
        Ok(body) => {
            tracing::info!(
                op = "registry-fetch",
                url,
                bytes = body.len(),
                duration_ms,
                "finished a {}",
                verb
            );
        }
        Err(error) => {
            tracing::warn!(
                op = "registry-fetch",
                url,
                code = error.kind().as_str(),
                reason = error.to_string(),
                duration_ms,
                "a {} failed",
                verb
            );
        }
    }
}

/// How long since `started`, in whole milliseconds.
fn elapsed_ms(started: &std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl BlockingClient {
    /// Runs one fetch after the policy check.
    fn fetch_once(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, RegistryError> {
        let mut no_progress = |_: u64, _: Option<u64>| {};
        self.download_chunks(url, max_bytes, &mut no_progress)
    }

    /// Downloads `url`, reporting `(downloaded, total)` after every chunk.
    ///
    /// `total` is the server's content length when it sent one. The callback
    /// runs on the calling worker thread; the caller forwards it to the UI.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`HttpGet::fetch`].
    fn download_chunks(
        &self,
        url: &str,
        max_bytes: usize,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Vec<u8>, RegistryError> {
        let mut response = self.client.get(url).send().map_err(|error| {
            if error.is_timeout() {
                RegistryError::new(ErrorKind::Network, "the download timed out")
            } else if error.is_redirect() {
                RegistryError::new(ErrorKind::InvalidUrl, "a redirect left the fetch policy")
            } else if error
                .status()
                .is_some_and(|status| status == reqwest::StatusCode::NOT_FOUND)
            {
                RegistryError::new(ErrorKind::NotFound, "the server has no such file")
            } else {
                RegistryError::new(ErrorKind::Network, error.to_string())
            }
        })?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RegistryError::new(
                ErrorKind::NotFound,
                "the server has no such file",
            ));
        }

        let status = response.status();
        if !status.is_success() {
            return Err(RegistryError::new(
                ErrorKind::Network,
                format!("the server answered {status}"),
            ));
        }

        let cap = u64::try_from(max_bytes).unwrap_or(u64::MAX);
        let total = response.content_length();

        if total.is_some_and(|len| len > cap) {
            return Err(RegistryError::new(
                ErrorKind::TooLarge,
                "the reply is larger than the cap for its kind",
            ));
        }

        let mut body = Vec::new();
        if let Some(len) = total.and_then(|len| usize::try_from(len).ok()) {
            body.reserve(len.min(max_bytes));
        }

        let mut chunk = vec![0u8; 32 * 1024];

        loop {
            use std::io::Read as _;

            let remaining = cap.saturating_sub(u64::try_from(body.len()).unwrap_or(u64::MAX));
            let read = response
                .by_ref()
                .take(remaining.saturating_add(1))
                .read(&mut chunk)
                .map_err(|error| RegistryError::new(ErrorKind::Network, error.to_string()))?;

            if read == 0 {
                break;
            }

            body.extend_from_slice(&chunk[..read]);
            progress(u64::try_from(body.len()).unwrap_or(u64::MAX), total);

            if u64::try_from(body.len()).unwrap_or(u64::MAX) > cap {
                return Err(RegistryError::new(
                    ErrorKind::TooLarge,
                    "the reply is larger than the cap for its kind",
                ));
            }
        }

        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_builds_without_touching_the_network() {
        let client = BlockingClient::new(Duration::from_secs(5)).expect("builds");

        assert!(
            client
                .fetch("http://speeduino.com/fw/versions", 64)
                .is_err()
        );
        assert!(client.fetch("not a url", 64).is_err());
    }

    #[test]
    fn policy_violations_report_their_kind() {
        let client = BlockingClient::new(Duration::from_secs(5)).expect("builds");

        let error = client
            .fetch("http://speeduino.com/fw/versions", 64)
            .expect_err("plain http is refused before any I/O");
        assert_eq!(error.kind(), ErrorKind::InvalidUrl);

        let error = client
            .fetch("https://", 64)
            .expect_err("a bare host is refused");
        assert_eq!(error.kind(), ErrorKind::InvalidUrl);
    }
}
