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

/// The definition file for `version`, when it is a file-name token.
///
/// The version becomes a path segment, so anything outside the token
/// vocabulary is refused rather than interpolated into a URL.
pub fn ini_url_for_version(version: &str) -> Option<String> {
    let token = !version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));

    token.then(|| format!("https://speeduino.com/fw/{version}.ini"))
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

        (id.family() == Family::Speeduino).then_some(id)
    }

    fn ini_url(&self, id: &FirmwareId) -> Option<String> {
        if id.family() != Family::Speeduino || family_of(id.raw()) != Family::Speeduino {
            return None;
        }

        ini_url_for_version(id.version())
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
        use crate::definition::family::parse_signature;

        let source = SpeeduinoSource;
        let id =
            parse_signature("rusEFI master.2024.02.02.alphax-4chan.1293678056").expect("parses");

        assert_eq!(source.ini_url(&id), None);
    }

    #[test]
    fn a_version_outside_the_token_vocabulary_builds_no_url() {
        assert_eq!(ini_url_for_version("../../etc/passwd"), None);
        assert_eq!(ini_url_for_version("202501.7 extra"), None);
        assert_eq!(ini_url_for_version(""), None);
        assert_eq!(
            ini_url_for_version("202501.7").as_deref(),
            Some("https://speeduino.com/fw/202501.7.ini")
        );
    }
}
