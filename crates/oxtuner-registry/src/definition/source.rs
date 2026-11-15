//! One download source per firmware family.

use crate::{
    definition::{
        family::{Family, FirmwareId},
        types::VersionEntry,
    },
    error::{ErrorKind, RegistryError},
    fetch::{HttpGet, MAX_VERSIONS_BYTES},
};

/// Where one family's definition files come from.
///
/// A new family is a new implementation here, never a branch in the shared
/// code. Only the required methods decide identity and the definition URL;
/// listing and notes stay optional because not every family publishes them.
pub trait FirmwareSource: Send + Sync {
    /// The family this source serves.
    fn family(&self) -> Family;

    /// Claims `signature`, or nothing when it is another family's.
    fn parse(&self, signature: &str) -> Option<FirmwareId>;

    /// The definition URL for an id this source parsed.
    fn ini_url(&self, id: &FirmwareId) -> Option<String>;

    /// The version listing URL, when the family publishes one text file.
    fn versions_url(&self) -> Option<String> {
        None
    }

    /// A hint of where the listing came from, for the cache provenance.
    ///
    /// The default is the versions URL; families with their own index
    /// override it with the index root.
    fn index_hint(&self) -> String {
        self.versions_url().unwrap_or_default()
    }

    /// Whether this family publishes a browsable version list.
    ///
    /// Sources without a list still resolve direct signature addresses.
    fn has_listing(&self) -> bool {
        self.versions_url().is_some()
    }

    /// Lists the family for the dialog, newest first.
    ///
    /// The default reads the [`versions_url`](Self::versions_url) text and
    /// parses one version per line. Families with their own index (an
    /// autoindex tree, a release feed) override this and walk it bounded.
    ///
    /// # Errors
    ///
    /// Returns the fetch failure when the index cannot be read.
    fn fetch_listing(&self, fetch: &dyn HttpGet) -> Result<Vec<VersionEntry>, RegistryError> {
        let url = self.versions_url().ok_or_else(|| {
            RegistryError::new(
                ErrorKind::Unsupported,
                "this family publishes no version list",
            )
        })?;

        let bytes = fetch.fetch(&url, MAX_VERSIONS_BYTES)?;
        let text = String::from_utf8_lossy(&bytes);

        Ok(crate::definition::types::parse_versions(&text))
    }
}
