//! How bytes enter the registry: they do not enter here.
//!
//! This crate defines the [`HttpGet`] trait and the policy every fetch obeys.
//! The application implements the trait on its own worker thread with whatever
//! client it chose, so this crate never depends on a network stack, a TLS
//! backend, or an async runtime.

use crate::error::{ErrorKind, RegistryError};

/// The largest `versions.txt` listing this crate will read.
///
/// A version list is a few dozen short lines; the cap only stops a stray or
/// hostile reply from being read whole.
pub const MAX_VERSIONS_BYTES: usize = 64 * 1024;

/// The largest definition file this crate will read.
///
/// A real definition is a few hundred kilobytes at most.
pub const MAX_INI_BYTES: usize = 2 * 1024 * 1024;

/// The largest release-notes document this crate will read.
pub const MAX_NOTES_BYTES: usize = 256 * 1024;

/// How many redirects one fetch may follow.
pub const MAX_REDIRECTS: u8 = 3;

/// Fetches URL bodies on the application's worker thread.
///
/// The implementor owns timeouts, redirect handling and the response size cap:
/// it must return [`ErrorKind::TooLarge`] instead of a body over `max_bytes`,
/// and only ever request URLs that [`check_url`] accepts.
pub trait HttpGet: Send + Sync {
    /// Reads the whole body of `url`, or the reason it could not be read.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::InvalidUrl`] for a URL outside the fetch policy,
    /// [`ErrorKind::NotFound`] for a missing file, [`ErrorKind::TooLarge`] for a
    /// body over the cap, and [`ErrorKind::Network`] for anything else that goes
    /// wrong on the wire.
    fn get(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, RegistryError>;
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

    let host = rest.split('/').next().unwrap_or_default();

    if host.is_empty() {
        return Err(RegistryError::new(
            ErrorKind::InvalidUrl,
            "the URL names no host",
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
        ] {
            let error = check_url(url).expect_err("must be refused");

            assert_eq!(error.kind(), ErrorKind::InvalidUrl, "for {url}");
        }
    }
}
