//! A blocking HTTP client built on `reqwest`.
//!
//! This is the only network code in the crate, and it blocks the calling
//! thread on purpose: the caller runs it on a worker thread (GPUI's background
//! executor or a dedicated `std::thread`), never on the core or I/O thread and
//! never on the UI thread. The tokio runtime `reqwest` keeps inside stays
//! within that worker call, the same way async-only adapters own their runtime
//! on a separate thread.

use std::{io::Read, time::Duration};

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
    fn get(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, RegistryError> {
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

        let started = std::time::Instant::now();
        tracing::debug!(op = "registry-fetch", url, max_bytes, "starting a download");

        let result = self.fetch_once(url, max_bytes);
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        match &result {
            Ok(body) => {
                tracing::info!(
                    op = "registry-fetch",
                    url,
                    bytes = body.len(),
                    duration_ms,
                    "finished a download"
                );
            }
            Err(error) => {
                tracing::warn!(
                    op = "registry-fetch",
                    url,
                    code = error.kind().as_str(),
                    reason = error.to_string(),
                    duration_ms,
                    "a download failed"
                );
            }
        }

        result
    }
}

impl BlockingClient {
    /// Runs one download after the policy check.
    fn fetch_once(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, RegistryError> {
        let response = self.client.get(url).send().map_err(|error| {
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

        if response.content_length().is_some_and(|len| len > cap) {
            return Err(RegistryError::new(
                ErrorKind::TooLarge,
                "the reply is larger than the cap for its kind",
            ));
        }

        let mut body = Vec::new();
        response
            .take(cap.saturating_add(1))
            .read_to_end(&mut body)
            .map_err(|error| RegistryError::new(ErrorKind::Network, error.to_string()))?;

        if body.len() > max_bytes {
            return Err(RegistryError::new(
                ErrorKind::TooLarge,
                "the reply is larger than the cap for its kind",
            ));
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

        assert!(client.get("http://speeduino.com/fw/versions", 64).is_err());
        assert!(client.get("not a url", 64).is_err());
    }

    #[test]
    fn policy_violations_report_their_kind() {
        let client = BlockingClient::new(Duration::from_secs(5)).expect("builds");

        let error = client
            .get("http://speeduino.com/fw/versions", 64)
            .expect_err("plain http is refused before any I/O");
        assert_eq!(error.kind(), ErrorKind::InvalidUrl);

        let error = client
            .get("https://", 64)
            .expect_err("a bare host is refused");
        assert_eq!(error.kind(), ErrorKind::InvalidUrl);
    }
}
