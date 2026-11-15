//! `TunerStudio` projects and `.msq` tune files.
//!
//! The v1 persistence layer (ADR-0007): open a `TunerStudio` project **in
//! place**, read `CurrentTune.msq` into the tune model, and write only the
//! values that changed. The native project format is deferred (ADR-0018).
//!
//! ```text
//! project folder ── projectCfg/mainController.ini ──▶ Definition ─┐
//!        │                                                          ├─▶ check ─▶ read-only (S1)
//!        └──────────── CurrentTune.msq ──▶ Msq ──▶ load ──▶ TuneImages
//!                                              │
//!                                              └─ save: changed values only,
//!                                                 unknown elements preserved
//! ```
//!
//! # Guarantees
//!
//! - **In place.** Nothing is copied or restructured; inside the project,
//!   writes are confined to `CurrentTune.msq` and the `.oxtuner/` sidecar
//!   (ADR-0007). The optional definition cache is written only under the
//!   caller-supplied cache directory (ADR-0026).
//! - **Cached, never owned.** A caller may pass a cache directory
//!   ([`Project::open_with_cache`]); the parsed definition is then memoized by
//!   content, settings and parser version. The cache is a derived artifact:
//!   deleting it costs one parse and changes nothing about the project, and a
//!   read-only project is still cacheable because the cache lives outside it.
//! - **No silent guesses.** A signature/page/size mismatch opens the tune
//!   read-only with reasons (ADR-0011); values that cannot be converted are
//!   reported and left at their default, with the original text untouched.
//! - **Raw-preserving.** The XML tree keeps untouched nodes byte-for-byte, so
//!   unknown elements, comments and newer `TunerStudio` data survive a save.
//! - **No panics on hostile input.** The reader has a depth limit, structured
//!   errors and no entity expansion.
//!
//! # Example
//!
//! ```no_run
//! use oxtuner_project::Project;
//!
//! let mut project = Project::open("/tunes/Sonic_150")?;
//! println!("{}", project.report().summary());
//!
//! if !project.is_read_only() {
//!     project
//!         .tune_mut()
//!         .set_user("reqFuel", 12.3)
//!         .expect("the definition has reqFuel");
//!     let saved = project.save()?;
//!     println!("wrote {} cells to {}", saved.cells, saved.path.display());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_code)]

mod atomic;
pub mod check;
mod definition_cache;
mod flags;
pub mod hash;
pub mod load;
pub mod msq;
pub mod project;
pub mod sidecar;
pub mod xml;

pub use check::{IssueKind, LoadIssue, LoadReport, check};
pub use definition_cache::{CACHE_FORMAT_VERSION, DefinitionSource};
pub use load::{FileValueError, LoadedTune, format_values, load};
pub use msq::{Entry, EntryKind, EntryPage, Msq, MsqError, PageHeader, VersionInfo};
pub use oxtuner_encoding::Latin1Error as EncodeError;
pub use project::{
    CONFIG_DIR, CreateRequest, MAIN_DEFINITION, Project, ProjectError, ProjectKind, SaveOutcome,
    TUNE_FILE,
};
pub use sidecar::{
    ConnectionState, DEFAULT_BOTTOM_PANEL_HEIGHT, DEFAULT_PRIMARY_SIDEBAR_WIDTH,
    DEFAULT_SECONDARY_SIDEBAR_WIDTH, DefinitionRef, ProjectState, SIDECAR_DIR, STATE_FILE,
    STATE_SCHEMA_VERSION, UiState,
};
pub use xml::{Attr, Document, Element, Node, XmlError, XmlErrorKind};
