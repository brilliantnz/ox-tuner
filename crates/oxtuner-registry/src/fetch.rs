//! How bytes enter the registry: they do not enter here.
//!
//! This crate defines the [`HttpGet`] trait and the policy every fetch obeys.
//! The application implements the trait on its own worker thread with whatever
//! client it chose, so this crate never depends on a network stack, a TLS
//! backend, or an async runtime.

use crate::error::{ErrorKind, RegistryError};

/// The largest version listing this crate will read.
///
/// A listing is a few dozen short lines; the cap only stops a stray or
/// hostile reply from being read whole.
pub const MAX_VERSIONS_BYTES: usize = 64 * 1024;

/// The largest definition file this crate will read.
///
/// A real definition is a few hundred kilobytes at most.
pub const MAX_INI_BYTES: usize = 2 * 1024 * 1024;

/// How long a cached listing counts as fresh.
pub const VERSIONS_TTL_SECS: u64 = 24 * 60 * 60;

/// The largest autoindex page this crate will read.
///
/// Directory listings are a few kilobytes; the cap only stops a stray or
/// hostile reply from being read whole.
pub const MAX_LISTING_BYTES: usize = 256 * 1024;

/// How many redirects one fetch may follow.
pub const MAX_REDIRECTS: u8 = 3;

/// Fetches URL bodies on the application's worker thread.
///
/// Two verbs, two jobs. `fetch` lists, gets and checks: version listings,
/// release notes, any small read. `download` moves file bytes with progress.
/// Both obey the same policy and caps; only the reporting differs.
pub trait HttpGet: Send + Sync {
    /// Reads the whole body of `url`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::InvalidUrl`] for a URL outside the fetch policy,
    /// [`ErrorKind::NotFound`] for a missing file, [`ErrorKind::TooLarge`] for
    /// a body over the cap, and [`ErrorKind::Network`] for anything else that
    /// goes wrong on the wire.
    fn fetch(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, RegistryError>;

    /// Reads the whole body of `url`, reporting `(downloaded, total)` after
    /// every chunk.
    ///
    /// `total` is the server's content length when it sent one. The callback
    /// runs on the calling worker thread; the caller forwards it to the UI.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`HttpGet::fetch`].
    fn download(
        &self,
        url: &str,
        max_bytes: usize,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Vec<u8>, RegistryError>;
}

/// Checks that `url` is one this crate will fetch.
///
/// Only `https://` URLs with a host are accepted. Plain `http://` is refused
/// even when a firmware tool once used it, because a definition decides what
/// the tuner trusts.
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidUrl`] for anything that is not an `https://`
/// URL with a host.
pub fn check_url(url: &str) -> Result<(), RegistryError> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| RegistryError::new(ErrorKind::InvalidUrl, "only https URLs are fetched"))?;

    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();

    if authority.is_empty() {
        return Err(RegistryError::new(
            ErrorKind::InvalidUrl,
            "the URL names no host",
        ));
    }

    // A userinfo section hides the real host from a reader and a filter, and a
    // space cannot appear in a valid URL at all.
    if authority.contains('@') || url.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(RegistryError::new(
            ErrorKind::InvalidUrl,
            "the URL carries credentials or whitespace",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_https_url_with_a_host_passes() {
        assert!(check_url("https://speeduino.com/fw/versions").is_ok());
        assert!(check_url("https://rusefi.com/online/ini/rusefi/master/x.ini").is_ok());
    }

    #[test]
    fn plain_http_and_bare_hosts_are_refused() {
        for url in [
            "http://speeduino.com/fw/versions",
            "https://",
            "https:///no-host",
            "ftp://example.com/x.ini",
            "not a url",
            "https://user:pw@speeduino.com/fw/versions",
            "https://speeduino.com/fw/a b.ini",
        ] {
            let error = check_url(url).expect_err("must be refused");

            assert_eq!(error.kind(), ErrorKind::InvalidUrl, "for {url}");
        }
    }
}
