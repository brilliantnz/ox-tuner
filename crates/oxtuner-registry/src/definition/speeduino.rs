//! The Speeduino definition source.
//!
//! Versions are listed at one plain-text URL and definitions sit next to it,
//! one file per version. Both shapes were read from the running server, not
//! guessed.

use crate::definition::{
    family::{Family, FirmwareId, family_of, parse_signature},
    source::FirmwareSource,
};

/// The version listing: one version per line.
pub const VERSIONS_URL: &str = "https://speeduino.com/fw/versions";

/// The definition file for `version`.
pub fn ini_url_for_version(version: &str) -> String {
    format!("https://speeduino.com/fw/{version}.ini")
}

/// Serves the Speeduino definitions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SpeeduinoSource;

impl FirmwareSource for SpeeduinoSource {
    fn family(&self) -> Family {
        Family::Speeduino
    }

    fn parse(&self, signature: &str) -> Option<FirmwareId> {
        let id = parse_signature(signature).ok()?;

        (id.family == Family::Speeduino).then_some(id)
    }

    fn ini_url(&self, id: &FirmwareId) -> Option<String> {
        if id.family != Family::Speeduino || family_of(&id.raw) != Family::Speeduino {
            return None;
        }

        Some(ini_url_for_version(&id.version))
    }

    fn versions_url(&self) -> Option<String> {
        Some(VERSIONS_URL.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_claims_only_its_own_signatures() {
        let source = SpeeduinoSource;

        assert!(source.parse("speeduino 202501.7").is_some());
        assert!(source.parse("rusEFI master.2024.02.02.x.1").is_none());
        assert!(source.parse("").is_none());
    }

    #[test]
    fn the_urls_match_the_running_server() {
        let source = SpeeduinoSource;
        let id = source.parse("speeduino 202501.7").expect("parses");

        assert_eq!(
            source.ini_url(&id).expect("a url"),
            "https://speeduino.com/fw/202501.7.ini"
        );
        assert_eq!(
            source.versions_url().expect("a url"),
            "https://speeduino.com/fw/versions"
        );
    }

    #[test]
    fn another_family_gets_no_url() {
        let source = SpeeduinoSource;
        let mut id = source.parse("speeduino 202501.7").expect("parses");
        id.family = Family::Rusefi;

        assert_eq!(source.ini_url(&id), None);
    }
}
