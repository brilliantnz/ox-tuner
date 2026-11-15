//! Definition lookup: which source serves which signature.
//!
//! The registry holds one [`FirmwareSource`] per family and asks each in turn.
//! Adding a family means adding a source, never editing the dispatch.

pub mod family;
pub mod registry;
pub mod rusefi;
pub mod source;
pub mod speeduino;
pub mod types;

pub use family::{Family, FirmwareId, family_of, parse_signature};
pub use registry::{MaxAge, Registry};
pub use rusefi::RusefiSource;
pub use source::FirmwareSource;
pub use speeduino::SpeeduinoSource;
pub use types::{
    FetchedVersions, ResolvedDefinition, VersionEntry, is_stable_version, parse_versions,
};
