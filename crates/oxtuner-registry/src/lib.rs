//! Firmware and definition lookup with an OS cache.
//!
//! This crate answers two questions and nothing else: which family an ECU
//! signature names, and where that family's files live on disk and on the
//! network. It never opens a port and never flashes anything.
//!
//! The split is deliberate. URL shapes and signature spellings are pure
//! functions, so they are easy to test. Bytes move through [`fetch::HttpGet`],
//! implemented by the application (or by [`blocking::BlockingClient`]) on its
//! own worker thread, and land in a [`cache::CacheStore`] under the
//! caller-supplied root. The core keeps accepting plain file paths.
//!
//! ```no_run
//! use oxtuner_registry::{CacheStore, Registry};
//!
//! # fn main() -> Result<(), oxtuner_registry::RegistryError> {
//! let registry = Registry::with_defaults();
//! let url = registry.ini_url_for_signature("speeduino 202501.7");
//! assert!(url.is_some());
//!
//! let store = CacheStore::new("/tmp/oxtuner-registry-example");
//! let id = registry.parse_signature("speeduino 202501.7")?;
//! let _path = store.definition_ini(&id);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod blocking;
pub mod cache;
pub mod definition;
pub mod error;
pub mod fetch;
pub mod firmware;

pub use blocking::BlockingClient;

pub use cache::{CacheStore, Meta};
pub use definition::{
    Family, FetchedVersions, FirmwareId, FirmwareSource, MaxAge, Registry, ResolvedDefinition,
    VersionEntry,
};
pub use error::{ErrorKind, RegistryError};
pub use fetch::HttpGet;
pub use firmware::{Board, FirmwareImage, FirmwareImageSource, FlashPlan, FlashTool};
