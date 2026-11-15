//! The definition sources this build knows.

use std::fmt;

use super::{
    family::Family,
    family::FirmwareId,
    rusefi::RusefiSource,
    source::FirmwareSource,
    speeduino::SpeeduinoSource,
    types::{VersionEntry, parse_versions},
};
use crate::error::{ErrorKind, RegistryError};

/// The definition sources this build knows.
pub struct Registry {
    sources: Vec<Box<dyn FirmwareSource>>,
}

impl Registry {
    /// An empty registry, for callers that register their own sources.
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// The shipped sources: Speeduino and rusEFI.
    pub fn with_defaults() -> Self {
        Self {
            sources: vec![Box::new(SpeeduinoSource), Box::new(RusefiSource)],
        }
    }

    /// Adds one more source. Sources added first are asked first.
    pub fn add(&mut self, source: impl FirmwareSource + 'static) {
        self.sources.push(Box::new(source));
    }

    /// Splits `signature` with the first source that claims it.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::InvalidSignature`] when no source serves the text.
    pub fn parse_signature(&self, signature: &str) -> Result<FirmwareId, RegistryError> {
        for source in &self.sources {
            if let Some(id) = source.parse(signature) {
                return Ok(id);
            }
        }

        Err(RegistryError::new(
            ErrorKind::InvalidSignature,
            "no source serves this signature",
        ))
    }

    /// The definition URL for `signature`, or nothing when no source serves it.
    pub fn ini_url_for_signature(&self, signature: &str) -> Option<String> {
        let id = self.parse_signature(signature).ok()?;

        self.sources
            .iter()
            .find(|source| source.family() == id.family)?
            .ini_url(&id)
    }

    /// The version listing for `family`, or nothing when it publishes none.
    pub fn versions_url(&self, family: Family) -> Option<String> {
        self.sources
            .iter()
            .find(|source| source.family() == family)?
            .versions_url()
    }

    /// Reads a version listing into entries.
    pub fn read_versions(&self, family: Family, text: &str) -> Option<Vec<VersionEntry>> {
        self.sources
            .iter()
            .any(|source| source.family() == family && source.versions_url().is_some())
            .then(|| parse_versions(text))
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let families: Vec<Family> = self.sources.iter().map(|source| source.family()).collect();

        formatter
            .debug_struct("Registry")
            .field("sources", &families)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_family_resolves_to_its_own_url() {
        let registry = Registry::with_defaults();

        assert_eq!(
            registry
                .ini_url_for_signature("speeduino 202501.7")
                .expect("a url"),
            "https://speeduino.com/fw/202501.7.ini"
        );
        assert_eq!(
            registry
                .ini_url_for_signature("rusEFI master.2024.02.02.alphax-4chan.1293678056")
                .expect("a url"),
            "https://rusefi.com/online/ini/rusefi/master/2024/02/02/alphax-4chan/1293678056.ini"
        );
    }

    #[test]
    fn unknown_and_fome_signatures_resolve_to_nothing() {
        let registry = Registry::with_defaults();

        assert_eq!(registry.ini_url_for_signature("something else"), None);
        assert_eq!(
            registry.ini_url_for_signature("rusEFI (FOME) .2026.06.03.proteus_h7.1"),
            None
        );
    }

    #[test]
    fn only_speeduino_publishes_a_listing() {
        let registry = Registry::with_defaults();

        assert!(registry.versions_url(Family::Speeduino).is_some());
        assert_eq!(registry.versions_url(Family::Rusefi), None);

        let entries = registry
            .read_versions(Family::Speeduino, "202501.7\nmaster\n")
            .expect("entries");
        assert_eq!(entries.len(), 2);
        assert_eq!(registry.read_versions(Family::Rusefi, "x\n"), None);
    }
}
