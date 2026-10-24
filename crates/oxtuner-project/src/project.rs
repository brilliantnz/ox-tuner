//! Opening a TunerStudio project **in place** and saving its tune.
//!
//! A project folder is recognised by `projectCfg/`. The definition is
//! `projectCfg/mainController.ini` (or the only `.ini` there), resolved with
//! `#include` files relative to `projectCfg/`; the tune is `CurrentTune.msq`.
//! Nothing is copied or restructured (ADR-0007), and the only writes are:
//!
//! - `CurrentTune.msq` — and only the constants whose values changed;
//! - `.ox-tuner/provenance` — which INI was used, by hash and signature.
//!
//! Consistency is the S1 gate ([`crate::check`]): a mismatch opens the tune
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
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use oxtuner_ini::{Definition, IniError, SettingsContext};
use oxtuner_tune::{Layout, Tune, TuneDiff, TuneError, TuneImages};

use crate::check::LoadReport;
use crate::load::{self, FileValueError};
use crate::msq::{EntryKind, EntryPage, Msq, MsqError};
use crate::sidecar::Provenance;
use crate::{atomic, flags};

/// The tune file inside a project.
pub const TUNE_FILE: &str = "CurrentTune.msq";
/// The definition directory inside a project.
pub const CONFIG_DIR: &str = "projectCfg";
/// The definition a TunerStudio project normally uses.
pub const MAIN_DEFINITION: &str = "mainController.ini";

/// What a path looks like before opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    /// A TunerStudio project folder.
    Folder(PathBuf),
    /// A bare `.msq` file opened with a separate INI.
    TuneFile(PathBuf),
}

/// Why a project could not be opened or saved.
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
            Self::Io { source, .. } => Some(source),
            Self::Msq(error) => Some(error),
            Self::Ini(error) => Some(error),
            Self::Unrepresentable { source, .. } => Some(source),
            Self::Tune(error) => Some(error),
            Self::Sidecar { source, .. } => Some(source),
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
    settings: SettingsContext,
    tune_path: PathBuf,
    msq: Msq,
    images: TuneImages,
    report: LoadReport,
    in_project: bool,
}

impl Project {
    /// Classifies a path without opening anything.
    pub fn detect(path: &Path) -> Result<ProjectKind, ProjectError> {
        if path.is_dir() {
            if path.join(CONFIG_DIR).is_dir() {
                return Ok(ProjectKind::Folder(path.to_owned()));
            }
            return Err(ProjectError::NotAProject {
                path: path.to_owned(),
            });
        }
        let is_msq = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("msq"));
        if is_msq {
            return Ok(ProjectKind::TuneFile(path.to_owned()));
        }
        Err(ProjectError::NotAProject {
            path: path.to_owned(),
        })
    }

    /// Opens a TunerStudio project folder in place.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let root = root.as_ref();
        if !root.join(CONFIG_DIR).is_dir() {
            return Err(ProjectError::NotAProject {
                path: root.to_owned(),
            });
        }

        let config = root.join(CONFIG_DIR);
        let definition_path = find_definition(&config)?;
        let tune_path = root.join(TUNE_FILE);

        let definition_bytes = read(&definition_path)?;
        let tune_bytes = read(&tune_path).map_err(|error| match error {
            ProjectError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                ProjectError::MissingTune {
                    path: tune_path.clone(),
                }
            }
            other => other,
        })?;

        let msq = Msq::parse(&tune_bytes)?;

        // The snapshot records the flags the tune was written with. Parsing the
        // definition under different flags would interpret the file through a
        // different shape than the one that produced it.
        let settings = snapshot_settings(&msq);
        let definition = parse_definition(&definition_bytes, &settings, &config)?;

        let loaded = load::load(&msq, &definition);
        let images = TuneImages::from_working(loaded.tune);

        Ok(Self {
            root: root.to_owned(),
            definition_path,
            definition_digest: crate::hash::hex(&crate::hash::sha256(&definition_bytes)),
            definition,
            settings,
            tune_path,
            msq,
            images,
            report: loaded.report,
            in_project: true,
        })
    }

    /// Opens a bare `.msq` with a user-picked INI.
    pub fn open_standalone(
        tune_path: impl AsRef<Path>,
        definition_path: impl AsRef<Path>,
    ) -> Result<Self, ProjectError> {
        let tune_path = tune_path.as_ref();
        let definition_path = definition_path.as_ref();

        let definition_bytes = read(definition_path)?;
        let tune_bytes = read(tune_path)?;
        let msq = Msq::parse(&tune_bytes)?;

        let settings = snapshot_settings(&msq);
        let base = definition_path.parent().unwrap_or(Path::new("."));
        let definition = parse_definition(&definition_bytes, &settings, base)?;

        let loaded = load::load(&msq, &definition);
        let images = TuneImages::from_working(loaded.tune);
        let root = tune_path.parent().unwrap_or(Path::new(".")).to_owned();

        Ok(Self {
            root,
            definition_path: definition_path.to_owned(),
            definition_digest: crate::hash::hex(&crate::hash::sha256(&definition_bytes)),
            definition,
            settings,
            tune_path: tune_path.to_owned(),
            msq,
            images,
            report: loaded.report,
            in_project: false,
        })
    }

    /// The project folder, or the tune file's folder in standalone mode.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The definition that was loaded.
    pub fn definition(&self) -> &Definition {
        &self.definition
    }

    /// Path of the definition file that was loaded.
    pub fn definition_path(&self) -> &Path {
        &self.definition_path
    }

    /// Path of the tune file.
    pub fn tune_path(&self) -> &Path {
        &self.tune_path
    }

    /// Whether this is a project folder (as opposed to standalone).
    pub fn is_in_project(&self) -> bool {
        self.in_project
    }

    /// The settings context the definition was parsed with.
    pub fn settings(&self) -> &SettingsContext {
        &self.settings
    }

    /// The layout every value is interpreted through.
    pub fn layout(&self) -> &Arc<Layout> {
        self.images.layout()
    }

    /// The four tune images.
    pub fn images(&self) -> &TuneImages {
        &self.images
    }

    /// The images, mutably — edits go through here.
    pub fn images_mut(&mut self) -> &mut TuneImages {
        &mut self.images
    }

    /// The working tune.
    pub fn tune(&self) -> &Tune {
        self.images.working()
    }

    /// The working tune, mutably (an alias for [`Self::images_mut`] use).
    pub fn tune_mut(&mut self) -> &mut Tune {
        self.images.working_mut()
    }

    /// The load gate's findings.
    pub fn report(&self) -> &LoadReport {
        &self.report
    }

    /// Whether the tune must not be saved (S1).
    pub fn is_read_only(&self) -> bool {
        self.report.is_read_only()
    }

    /// Writes changed values back to the tune file.
    ///
    /// Only variables whose values differ from the baseline are touched, so an
    /// unedited project keeps its file byte-for-byte; unknown elements,
    /// comments and entries this build does not model are never rewritten.
    pub fn save(&mut self) -> Result<SaveOutcome, ProjectError> {
        if self.is_read_only() {
            return Err(ProjectError::ReadOnly {
                reasons: self
                    .report
                    .read_only_reasons()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            });
        }

        let diff = TuneDiff::between(self.images.baseline(), self.images.working())?;
        let mut written: Vec<String> = Vec::new();
        let mut cells = 0;
        for name in &diff.changed_variables {
            let Some(spec) = self.layout().variable(name) else {
                continue;
            };
            if spec.no_msq_save {
                continue;
            }
            let Some(values) = self.tune().value(name) else {
                continue;
            };
            let Some(baseline) = self.images.baseline().value(name) else {
                continue;
            };
            // Counted before the write: `values` borrows the images and the
            // document is mutated next.
            let changed = baseline.changed_cells(values);
            let text = load::format_values(spec, &self.definition, values).map_err(|source| {
                ProjectError::Unrepresentable {
                    name: name.clone(),
                    source,
                }
            })?;

            let (page, kind) = match spec.ini_page() {
                Some(page) => (EntryPage::Numbered(page.to_msq()), EntryKind::Constant),
                None => (EntryPage::Pc, EntryKind::PcVariable),
            };
            self.msq.put_entry_text(page, kind, name, &text)?;

            // Only the cells that actually reached the file are reported; a
            // `noMsqSave` variable is skipped above and must not be counted.
            cells += changed;
            written.push(name.clone());
        }

        self.msq
            .set_settings(&self.settings, &self.definition.tested_flags)?;

        let bytes = self.msq.save()?;
        atomic::write(&self.tune_path, &bytes).map_err(|source| ProjectError::Io {
            path: self.tune_path.clone(),
            source,
        })?;

        if self.in_project {
            let relative = self
                .definition_path
                .strip_prefix(&self.root)
                .unwrap_or(&self.definition_path);
            let relative = relative.to_string_lossy().replace('\\', "/");
            let provenance = Provenance {
                definition: relative,
                definition_sha256: self.definition_digest.clone(),
                signature: self.definition.signature.clone(),
            };
            provenance
                .write(&self.root)
                .map_err(|source| ProjectError::Sidecar {
                    path: self.root.join(crate::sidecar::SIDECAR_DIR),
                    source,
                })?;
        }

        self.images.commit_baseline();

        Ok(SaveOutcome {
            path: self.tune_path.clone(),
            cells,
            variables: written,
        })
    }

    /// The sidecar provenance recorded for this project, when any.
    pub fn provenance(&self) -> Option<Provenance> {
        if !self.in_project {
            return None;
        }
        Provenance::read(&self.root).ok().flatten()
    }
}

fn read(path: &Path) -> Result<Vec<u8>, ProjectError> {
    fs::read(path).map_err(|source| ProjectError::Io {
        path: path.to_owned(),
        source,
    })
}

fn find_definition(config: &Path) -> Result<PathBuf, ProjectError> {
    let main = config.join(MAIN_DEFINITION);
    if main.is_file() {
        return Ok(main);
    }

    let mut candidates: Vec<PathBuf> = fs::read_dir(config)
        .map_err(|source| ProjectError::Io {
            path: config.to_owned(),
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("ini"))
        })
        .collect();
    candidates.sort();

    match candidates.len() {
        0 => Err(ProjectError::MissingDefinition {
            directory: config.to_owned(),
        }),
        1 => Ok(candidates.remove(0)),
        _ => Err(ProjectError::AmbiguousDefinition {
            directory: config.to_owned(),
            candidates: candidates
                .iter()
                .filter_map(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect(),
        }),
    }
}

fn parse_definition(
    bytes: &[u8],
    settings: &SettingsContext,
    base: &Path,
) -> Result<Definition, ProjectError> {
    let text = oxtuner_ini::source::decode(bytes).text;
    let resolver = |name: &str| read_include(base, name);
    oxtuner_ini::parse_str_with_includes(&text, settings, &resolver).map_err(ProjectError::Ini)
}

/// Reads an `#include` file from `base`, refusing to leave it.
fn read_include(base: &Path, name: &str) -> Option<String> {
    let path = Path::new(name);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }

    // A symlink inside the definition directory can point anywhere, and a
    // backslash is not a separator on Unix, so lexical checks alone do not
    // keep the read inside `base`. Resolve both sides and compare.
    //
    // A bare filename has an empty parent, which is the working directory.
    // `canonicalize` rejects the empty path, so spell it out.
    let base = if base.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base
    };
    let base = fs::canonicalize(base).ok()?;
    let path = fs::canonicalize(base.join(path)).ok()?;
    if !path.starts_with(&base) {
        return None;
    }

    let bytes = fs::read(&path).ok()?;
    Some(oxtuner_ini::source::decode(&bytes).text)
}

/// The settings context a tune's `<settings>` snapshot describes.
///
/// Starts from the INI's own declarations (inside [`SettingsContext::new`]) and
/// applies the snapshot as overrides, so a flag the file names wins while an
/// unmentioned flag keeps the definition's default.
fn snapshot_settings(msq: &Msq) -> SettingsContext {
    SettingsContext::new().with_overrides(settings_overrides(&msq.settings()))
}

/// Maps a `<settings>` snapshot to preprocessor overrides.
///
/// The heuristic is the one the file format carries: `_ON`/`_OFF` suffixes
/// select a boolean flag, a bare name is an active choice. Setting-group
/// entries (`boostUnits` → `BOOSTPSI`) also become active names; that only
/// matters if the INI tests such a name in `#if`.
fn settings_overrides(entries: &[(String, String)]) -> Vec<(String, bool)> {
    let mut overrides: Vec<(String, bool)> = Vec::new();
    for (name, value) in entries {
        for text in [name, value] {
            if let Some(stem) = flags::strip_suffix_ci(text, "_OFF") {
                overrides.push((stem.to_owned(), false));
            } else if let Some(stem) = flags::strip_suffix_ci(text, "_ON") {
                overrides.push((stem.to_owned(), true));
            } else if !text.is_empty() {
                overrides.push((text.clone(), true));
            }
        }
    }
    overrides
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_without_projectcfg_is_not_a_project() {
        let path = Path::new("/definitely/not/a/project");
        assert!(matches!(
            Project::detect(path),
            Err(ProjectError::NotAProject { .. })
        ));
    }

    #[test]
    fn a_tune_file_is_detected_case_insensitively() {
        assert_eq!(
            Project::detect(Path::new("/tmp/CurrentTune.MSQ")).unwrap(),
            ProjectKind::TuneFile(PathBuf::from("/tmp/CurrentTune.MSQ"))
        );
    }

    #[test]
    fn settings_suffixes_map_to_booleans() {
        let overrides = settings_overrides(&[
            (
                "enablehardware_test_OFF".to_owned(),
                "enablehardware_test_OFF".to_owned(),
            ),
            ("CELSIUS".to_owned(), "CELSIUS".to_owned()),
        ]);
        assert!(overrides.contains(&("enablehardware_test".to_owned(), false)));
        assert!(overrides.contains(&("CELSIUS".to_owned(), true)));
    }

    #[test]
    fn an_include_cannot_escape_its_directory() {
        let base = Path::new("/tmp/project/projectCfg");
        assert_eq!(read_include(base, "../secret.ini"), None);
        assert_eq!(read_include(base, "/etc/passwd"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_include_cannot_escape_its_directory() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "oxtuner-include-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = root.join("projectCfg");
        fs::create_dir_all(&config).unwrap();
        fs::write(root.join("secret.ini"), "signature = \"secret\"").unwrap();
        fs::write(config.join("real.ini"), "signature = \"real\"").unwrap();
        symlink(root.join("secret.ini"), config.join("link.ini")).unwrap();

        assert!(read_include(&config, "real.ini").is_some());
        assert_eq!(
            read_include(&config, "link.ini"),
            None,
            "a symlink that leaves the definition directory must not be read"
        );

        fs::remove_dir_all(&root).unwrap();
    }
}
