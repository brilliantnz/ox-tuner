//! One download source per firmware family.

use crate::definition::family::{Family, FirmwareId};

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

    /// The version listing URL, when the family publishes one.
    fn versions_url(&self) -> Option<String> {
        None
    }

    /// The release-notes URL for an id, when the family publishes notes.
    fn release_notes_url(&self, _id: &FirmwareId) -> Option<String> {
        None
    }
}
