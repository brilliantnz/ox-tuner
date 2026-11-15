//! The registry's error type.

use std::{fmt, io};

/// Why a registry operation failed.
///
/// The variants are stable machine keys. Diagnostics and notification policy
/// filter on [`RegistryError::kind`] instead of parsing the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The signature names no known family or version.
    InvalidSignature,
    /// The text is not a URL this crate will fetch.
    InvalidUrl,
    /// The server has no such file.
    NotFound,
    /// The family publishes no such thing.
    Unsupported,
    /// The reply is larger than the cap for its kind.
    TooLarge,
    /// The bytes could not be fetched.
    Network,
    /// The cache could not be read or written.
    Cache,
    /// The definition pulls in other files, so it cannot be installed as one.
    RefusedIncludes,
}

/// A registry failure with a machine-readable kind and a human message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryError {
    kind: ErrorKind,
    message: String,
}

impl ErrorKind {
    /// A stable kebab-case code for logs, Diagnostics filters and a future
    /// notification policy. Filtering never parses the message.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSignature => "invalid-signature",
            Self::InvalidUrl => "invalid-url",
            Self::NotFound => "not-found",
            Self::Unsupported => "unsupported",
            Self::TooLarge => "too-large",
            Self::Network => "network",
            Self::Cache => "cache",
            Self::RefusedIncludes => "refused-includes",
        }
    }
}
impl RegistryError {
    /// The machine key for this failure.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Builds an error of one kind with a message.
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RegistryError {}

impl From<io::Error> for RegistryError {
    /// Any filesystem failure surfaces as a cache failure.
    ///
    /// The cache is a derived artifact, so callers treat this as a miss and
    /// fall back to fetching or parsing again.
    fn from(error: io::Error) -> Self {
        Self::new(ErrorKind::Cache, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kind_selects_how_a_caller_reacts() {
        let error = RegistryError::new(ErrorKind::NotFound, "no such version");

        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(error.kind().as_str(), "not-found");
        assert_eq!(error.to_string(), "no such version");
    }

    #[test]
    fn an_io_failure_is_a_cache_failure() {
        let error = RegistryError::from(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));

        assert_eq!(error.kind(), ErrorKind::Cache);
    }
}
