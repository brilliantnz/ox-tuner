//! TunerStudio projects and `.msq` tune files.
//!
//! The v1 persistence layer (ADR-0007): open a TunerStudio project **in
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
//! - **In place.** Nothing is copied or restructured; writes are confined to
//!   `CurrentTune.msq` and the `.ox-tuner/` sidecar (ADR-0007).
//! - **No silent guesses.** A signature/page/size mismatch opens the tune
//!   read-only with reasons (ADR-0011); values that cannot be converted are
//!   reported and left at their default, with the original text untouched.
//! - **Raw-preserving.** The XML tree keeps untouched nodes byte-for-byte, so
//!   unknown elements, comments and newer TunerStudio data survive a save.
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
pub mod encoding;
mod flags;
pub mod hash;
pub mod load;
pub mod msq;
pub mod project;
pub mod sidecar;
pub mod xml;

pub use check::{IssueKind, LoadIssue, LoadReport, check};
pub use encoding::EncodeError;
pub use load::{FileValueError, LoadedTune, format_values, load};
pub use msq::{Entry, EntryKind, EntryPage, Msq, MsqError, PageHeader, VersionInfo};
pub use project::{
    CONFIG_DIR, MAIN_DEFINITION, Project, ProjectError, ProjectKind, SaveOutcome, TUNE_FILE,
};
pub use sidecar::{PROVENANCE_FILE, Provenance, SIDECAR_DIR};
pub use xml::{Attr, Document, Element, Node, XmlError, XmlErrorKind};
