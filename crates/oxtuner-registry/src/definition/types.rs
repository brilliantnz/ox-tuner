//! Version listings shared by the definition sources.

use serde::{Deserialize, Serialize};

use crate::definition::family::FirmwareId;

/// One definition the registry can serve: the parsed id and its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDefinition {
    /// The parsed signature.
    pub id: FirmwareId,
    /// Where the bytes live.
    pub ini_url: String,
}

/// One published version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    /// The version text as the server lists it.
    pub version: String,
    /// Whether this family treats the entry as a release rather than a channel.
    ///
    /// The rule is per family. Speeduino's `master` image and `EEPROM_clear`
    /// are channels; rusEFI publishes its releases from the `master` branch,
    /// so its builds count as releases.
    pub stable: bool,
}

/// A version listing and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedVersions {
    /// Whether the entries came from the cache.
    pub cached: bool,
    /// Whether the answer is older than the caller asked for.
    ///
    /// Set when a listing was served from the cache even though its age was
    /// past the policy ([`MaxAge::Any`]) or a refresh failed
    /// ([`MaxAge::Zero`]). Something to browse beats nothing while offline,
    /// and the dialog says so.
    pub stale: bool,
    /// The parsed entries.
    pub entries: Vec<VersionEntry>,
}

/// Whether `version` names a stable release.
///
/// The rolling channels (`master`) and the maintenance image
/// (`EEPROM_clear`) are useful but never defaults.
pub fn is_stable_version(version: &str) -> bool {
    let normalized = version.trim().to_ascii_lowercase();

    !normalized.is_empty() && normalized != "master" && normalized != "eeprom_clear"
}

/// Reads a plain-text version listing, one version per line.
///
/// Blank lines are skipped. An unstable channel is kept but marked, so the UI
/// can show it behind an advanced switch instead of hiding what exists.
pub fn parse_versions(text: &str) -> Vec<VersionEntry> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| VersionEntry {
            stable: is_stable_version(line),
            version: line.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_realistic_listing_parses_with_channels_marked() {
        let entries = parse_versions("202501.7\n202402.2\nmaster\nEEPROM_clear\n\n");

        assert_eq!(entries.len(), 4);
        assert!(entries[0].stable);
        assert!(entries[1].stable);
        assert!(!entries[2].stable);
        assert!(!entries[3].stable);
    }

    #[test]
    fn casing_and_blanks_do_not_create_entries() {
        assert_eq!(parse_versions("").len(), 0);
        assert_eq!(parse_versions("  \n\n").len(), 0);
        assert!(!is_stable_version("Master"));
        assert!(!is_stable_version("eeprom_clear"));
    }
}
