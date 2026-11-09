//! Opening a `TunerStudio` project **in place** and saving its tune.
//!
//! A project folder is recognised by `projectCfg/`. The definition is
//! `projectCfg/mainController.ini` (or the only `.ini` there), resolved with
//! `#include` files relative to `projectCfg/`; the tune is `CurrentTune.msq`.
//! Nothing is copied or restructured (ADR-0007), and the only writes inside the
//! project are:
//!
//! - `CurrentTune.msq` — and only the constants whose values changed;
//! - `.ox-tuner/provenance` — which INI was used, by hash and signature.
//!
//! A caller may additionally pass a cache directory; the parsed definition is
//! then memoized there, never inside the project (ADR-0026).
//!
//! Consistency is the S1 gate ([`crate::check()`]): a mismatch opens the tune
//! **read-only** and [`Project::save`] refuses with the reasons. There is no
//! bypass (ADR-0011).
//!
//! # Standalone
//!
//! A bare `.msq` plus a user-picked INI opens through
//! [`Project::open_standalone`]. In both modes the tune's `<settings>` snapshot
//! is the settings context: it records which INI feature flags (`CELSIUS`,
//! `LAMBDA`, …) the tune was written with, and the definition must be parsed
//! the same way to interpret its values. The INI's own `#set`/`#unset`
//! directives remain the defaults for flags the snapshot does not mention.

use std::fmt;
use std::io;
use std::path::PathBuf;

use oxtuner_ini::{Definition, IniError, SettingsContext};
use oxtuner_tune::{TuneError, TuneImages};

use crate::check::LoadReport;
use crate::definition_cache::DefinitionSource;
use crate::load::FileValueError;
use crate::msq::{Msq, MsqError};

/// The tune file inside a project.
pub const TUNE_FILE: &str = "CurrentTune.msq";
/// The definition directory inside a project.
pub const CONFIG_DIR: &str = "projectCfg";
/// The definition a `TunerStudio` project normally uses.
pub const MAIN_DEFINITION: &str = "mainController.ini";

/// What a path looks like before opening it.
#[expect(
    clippy::exhaustive_enums,
    reason = "a path is either a project folder or a standalone tune file"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    /// A `TunerStudio` project folder.
    Folder(PathBuf),
    /// A bare `.msq` file opened with a separate INI.
    TuneFile(PathBuf),
}

/// Why a project could not be opened or saved.
#[non_exhaustive]
#[derive(Debug)]
pub enum ProjectError {
    /// The path is neither a project folder nor a `.msq`.
    NotAProject {
        /// The path that was inspected.
        path: PathBuf,
    },
    /// `projectCfg/` exists but holds no `.ini`.
    MissingDefinition {
        /// The directory that was searched.
        directory: PathBuf,
    },
    /// `projectCfg/` holds several `.ini` files and none is `mainController.ini`.
    AmbiguousDefinition {
        /// The directory that was searched.
        directory: PathBuf,
        /// The candidates, sorted.
        candidates: Vec<String>,
    },
    /// The project has no `CurrentTune.msq`.
    MissingTune {
        /// The expected path.
        path: PathBuf,
    },
    /// A file could not be read or written.
    Io {
        /// The file involved.
        path: PathBuf,
        /// The operating-system error.
        source: io::Error,
    },
    /// The `.msq` is not readable.
    Msq(MsqError),
    /// The INI could not be parsed.
    Ini(IniError),
    /// The tune opened read-only; the reasons are the blockers.
    ReadOnly {
        /// Why saving is refused.
        reasons: Vec<String>,
    },
    /// A value cannot be represented in a `.msq`.
    Unrepresentable {
        /// The variable involved.
        name: String,
        /// Why.
        source: FileValueError,
    },
    /// The tune model rejected an operation.
    Tune(TuneError),
    /// The sidecar could not be written.
    Sidecar {
        /// The path involved.
        path: PathBuf,
        /// The operating-system error.
        source: io::Error,
    },
}

impl fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAProject { path } => write!(
                formatter,
                "{} is not a TunerStudio project (no {CONFIG_DIR}/) or a .msq",
                path.display()
            ),
            Self::MissingDefinition { directory } => write!(
                formatter,
                "{} contains no .ini definition",
                directory.display()
            ),
            Self::AmbiguousDefinition {
                directory,
                candidates,
            } => write!(
                formatter,
                "{} contains several definitions and no {MAIN_DEFINITION}: {}",
                directory.display(),
                candidates.join(", ")
            ),
            Self::MissingTune { path } => {
                write!(
                    formatter,
                    "the project has no {TUNE_FILE} at {}",
                    path.display()
                )
            }
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Msq(error) => write!(formatter, "{error}"),
            Self::Ini(error) => write!(formatter, "the definition could not be parsed: {error}"),
            Self::ReadOnly { reasons } => {
                write!(formatter, "the tune is read-only: {}", reasons.join("; "))
            }
            Self::Unrepresentable { name, source } => write!(formatter, "{name}: {source}"),
            Self::Tune(error) => write!(formatter, "{error}"),
            Self::Sidecar { path, source } => {
                write!(formatter, "cannot write {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ProjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Sidecar { source, .. } => Some(source),
            Self::Msq(error) => Some(error),
            Self::Ini(error) => Some(error),
            Self::Unrepresentable { source, .. } => Some(source),
            Self::Tune(error) => Some(error),
            Self::NotAProject { .. }
            | Self::MissingDefinition { .. }
            | Self::AmbiguousDefinition { .. }
            | Self::MissingTune { .. }
            | Self::ReadOnly { .. } => None,
        }
    }
}

impl From<MsqError> for ProjectError {
    fn from(error: MsqError) -> Self {
        Self::Msq(error)
    }
}

impl From<TuneError> for ProjectError {
    fn from(error: TuneError) -> Self {
        Self::Tune(error)
    }
}

/// What a save wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    /// The tune file that was rewritten.
    pub path: PathBuf,
    /// Number of values that changed and were written.
    pub cells: usize,
    /// Names that were written, in layout order.
    pub variables: Vec<String>,
}

/// An open project: definition, tune images and the file they came from.
#[derive(Debug)]
pub struct Project {
    root: PathBuf,
    definition_path: PathBuf,
    definition_digest: String,
    definition: Definition,
    definition_source: DefinitionSource,
    settings: SettingsContext,
    tune_path: PathBuf,
    msq: Msq,
    images: TuneImages,
    report: LoadReport,
    in_folder: bool,
}

mod access;
mod detect;
mod open;
mod save;

pub(crate) use open::read_include;

#[cfg(test)]
mod tests;
