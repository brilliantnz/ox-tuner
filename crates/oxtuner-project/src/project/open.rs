use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_tune::TuneImages;

use crate::definition_cache::{DefinitionCache, DefinitionSource, IncludeRecorder};
use crate::flags;
use crate::load::{self};
use crate::msq::Msq;

use super::{
    CONFIG_DIR, MAIN_DEFINITION, PC_VALUES_FILE, PROPERTIES_FILE, Project, ProjectError, TUNE_FILE,
};

impl Project {
    /// Opens a `TunerStudio` project folder in place.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the folder is not a project, its
    /// definition or tune is missing, or either cannot be read.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ProjectError> {
        Self::open_with_cache(root, None)
    }

    /// Opens a `TunerStudio` project folder, using `cache_dir` for the parsed
    /// definition when one is given.
    ///
    /// The cache is optional on purpose: without it the project opens exactly
    /// as it always did, and a cache that is missing, stale or corrupt is
    /// treated as a miss. Reading the definition bytes is not skipped — they
    /// are the fingerprint that decides whether the cached value still applies.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the folder is not a project, its
    /// definition or tune is missing, or either cannot be read.
    pub fn open_with_cache(
        root: impl AsRef<Path>,
        cache_dir: Option<&Path>,
    ) -> Result<Self, ProjectError> {
        let root = root.as_ref();
        let _span = tracing::info_span!("project_open", root = %root.display()).entered();
        let mut phase = Instant::now();

        if !root.join(CONFIG_DIR).is_dir() {
            return Err(ProjectError::NotAProject {
                path: root.to_owned(),
            });
        }

        let config = root.join(CONFIG_DIR);
        let definition_path = find_definition(&config)?;
        let tune_path = root.join(TUNE_FILE);

        let definition_bytes = {
            let _span = tracing::debug_span!("project_read_definition").entered();
            read(&definition_path)?
        };
        let definition_digest = crate::hash::sha256(&definition_bytes);
        let read_definition_ms = elapsed_ms(&mut phase);

        let tune_bytes = {
            let _span = tracing::debug_span!("project_read_tune").entered();
            read(&tune_path).map_err(|error| match error {
                ProjectError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                    ProjectError::MissingTune {
                        path: tune_path.clone(),
                    }
                }
                other => other,
            })?
        };
        let read_tune_ms = elapsed_ms(&mut phase);

        let msq = {
            let _span = tracing::debug_span!("project_msq_parse").entered();
            Msq::parse(&tune_bytes)?
        };
        let msq_ms = elapsed_ms(&mut phase);

        // The snapshot records the flags the tune was written with. Parsing the
        // definition under different flags would interpret the file through a
        // different shape than the one that produced it. The project's own
        // properties sit between the INI defaults and that snapshot.
        let files = read_project_files(&config);
        let settings = snapshot_settings(&msq, &files.settings);
        let (definition, definition_source) = {
            let _span = tracing::debug_span!("project_definition_load").entered();
            load_definition(
                &definition_bytes,
                definition_digest,
                &settings,
                &config,
                cache_dir,
            )
        }?;
        let definition_ms = elapsed_ms(&mut phase);

        let mut loaded = {
            let _span = tracing::debug_span!("project_tune_load").entered();
            load::load(&msq, &definition)
        };
        if let Some(values) = &files.pc_values {
            load::apply_pc_overrides(
                &mut loaded.tune,
                &loaded.layout,
                &definition,
                values,
                &mut loaded.report,
            );
        }
        let images = TuneImages::from_working(loaded.tune);
        let tune_ms = elapsed_ms(&mut phase);

        tracing::debug!(
            read_definition_ms,
            read_tune_ms,
            msq_ms,
            definition_ms,
            tune_ms,
            cache_hit = definition_source == DefinitionSource::Cache,
            "project open phases"
        );

        Ok(Self {
            root: root.to_owned(),
            definition_path,
            definition_digest: crate::hash::hex(&definition_digest),
            definition,
            definition_source,
            settings,
            tune_path,
            msq,
            images,
            report: loaded.report,
            in_folder: true,
        })
    }

    /// Opens a bare `.msq` with a user-picked INI.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the tune or definition cannot be read.
    pub fn open_standalone(
        tune_path: impl AsRef<Path>,
        definition_path: impl AsRef<Path>,
    ) -> Result<Self, ProjectError> {
        Self::open_standalone_with_cache(tune_path, definition_path, None)
    }

    /// Opens a bare `.msq`, caching the parsed definition when `cache_dir` is
    /// given.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the tune or definition cannot be read.
    pub fn open_standalone_with_cache(
        tune_path: impl AsRef<Path>,
        definition_path: impl AsRef<Path>,
        cache_dir: Option<&Path>,
    ) -> Result<Self, ProjectError> {
        let tune_path = tune_path.as_ref();
        let definition_path = definition_path.as_ref();
        let _span = tracing::info_span!("project_open", root = %tune_path.display()).entered();
        let mut phase = Instant::now();

        let definition_bytes = {
            let _span = tracing::debug_span!("project_read_definition").entered();
            read(definition_path)?
        };
        let definition_digest = crate::hash::sha256(&definition_bytes);
        let read_definition_ms = elapsed_ms(&mut phase);

        let tune_bytes = {
            let _span = tracing::debug_span!("project_read_tune").entered();
            read(tune_path)?
        };
        let read_tune_ms = elapsed_ms(&mut phase);

        let msq = {
            let _span = tracing::debug_span!("project_msq_parse").entered();
            Msq::parse(&tune_bytes)?
        };
        let msq_ms = elapsed_ms(&mut phase);

        let settings = snapshot_settings(&msq, &[]);
        let base = definition_path.parent().unwrap_or(Path::new("."));
        let (definition, definition_source) = {
            let _span = tracing::debug_span!("project_definition_load").entered();
            load_definition(
                &definition_bytes,
                definition_digest,
                &settings,
                base,
                cache_dir,
            )
        }?;
        let definition_ms = elapsed_ms(&mut phase);

        let loaded = {
            let _span = tracing::debug_span!("project_tune_load").entered();
            load::load(&msq, &definition)
        };
        let images = TuneImages::from_working(loaded.tune);
        let tune_ms = elapsed_ms(&mut phase);
        let root = tune_path.parent().unwrap_or(Path::new(".")).to_owned();

        tracing::debug!(
            read_definition_ms,
            read_tune_ms,
            msq_ms,
            definition_ms,
            tune_ms,
            cache_hit = definition_source == DefinitionSource::Cache,
            "project open phases"
        );

        Ok(Self {
            root,
            definition_path: definition_path.to_owned(),
            definition_digest: crate::hash::hex(&definition_digest),
            definition,
            definition_source,
            settings,
            tune_path: tune_path.to_owned(),
            msq,
            images,
            report: loaded.report,
            in_folder: false,
        })
    }
}

fn read(path: &Path) -> Result<Vec<u8>, ProjectError> {
    fs::read(path).map_err(|source| ProjectError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Milliseconds since the last mark, and moves the mark to now.
fn elapsed_ms(since: &mut Instant) -> f64 {
    let now = Instant::now();
    let elapsed = now.duration_since(*since).as_secs_f64() * 1000.0;
    *since = now;
    elapsed
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

/// Loads the definition from the cache or parses it, and stores the result.
///
/// A cache miss is not an error: parsing is the source of truth, and any write
/// failure leaves the freshly parsed definition in place.
fn load_definition(
    bytes: &[u8],
    definition_digest: [u8; 32],
    settings: &SettingsContext,
    base: &Path,
    cache_dir: Option<&Path>,
) -> Result<(Definition, DefinitionSource), ProjectError> {
    let cache = cache_dir
        .and_then(|directory| DefinitionCache::new(directory, base, definition_digest, settings));

    if let Some(cache) = &cache
        && let Some(definition) = cache.load()
    {
        tracing::debug!(
            base = %base.display(),
            "definition loaded from the cache"
        );
        return Ok((definition, DefinitionSource::Cache));
    }

    let text = oxtuner_ini::source::decode(bytes).text;
    let recorder = IncludeRecorder::new(base);
    let resolver = |name: &str| recorder.resolve(name);
    let mut definition = oxtuner_ini::parse_str_with_includes(&text, settings, &resolver)
        .map_err(ProjectError::Ini)?;
    let includes = recorder.finish();

    if let Some(cache) = &cache {
        cache.store(&mut definition, &includes);
    }

    Ok((definition, DefinitionSource::Parsed))
}

/// Reads an `#include` file from `base`, refusing to leave it.
pub(crate) fn read_include(base: &Path, name: &str) -> Option<String> {
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
    let base = match fs::canonicalize(base) {
        Ok(base) => base,
        Err(error) => {
            tracing::debug!(
                base = %base.display(),
                %error,
                "the include base directory could not be resolved"
            );
            return None;
        }
    };
    let path = match fs::canonicalize(base.join(path)) {
        Ok(path) => path,
        Err(error) => {
            tracing::debug!(include = name, %error, "an include path could not be resolved");
            return None;
        }
    };
    if !path.starts_with(&base) {
        return None;
    }

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::debug!(
                path = %path.display(),
                %error,
                "an include file could not be read"
            );
            return None;
        }
    };
    Some(oxtuner_ini::source::decode(&bytes).text)
}

/// The settings context a tune's `<settings>` snapshot describes.
///
/// Starts from the INI's own declarations (inside [`SettingsContext::new`]) and
/// applies overrides in the order the project defines them: the project's
/// `project.properties` first, then the tune's `<settings>` snapshot, which
/// wins on a duplicate flag. A flag the file names wins; an unmentioned flag
/// keeps the definition's default.
fn snapshot_settings(msq: &Msq, project: &[(String, bool)]) -> SettingsContext {
    let mut overrides = project.to_vec();
    overrides.extend(settings_overrides(&msq.settings()));

    SettingsContext::new().with_overrides(overrides)
}

/// The optional files a project keeps beside its definition.
#[derive(Debug, Default)]
struct ProjectFiles {
    /// Flags from `project.properties` (`ecuSettings=...`).
    settings: Vec<(String, bool)>,
    /// Per-project PC variable values, when the file is present and readable.
    pc_values: Option<Msq>,
}

/// Reads `project.properties` and `pcVariableValues.msq` when they exist.
///
/// Both are optional: a project without them opens exactly as before. A
/// malformed PC values file is logged and skipped rather than blocking the
/// tune, because it is not calibration data.
fn read_project_files(config: &Path) -> ProjectFiles {
    let mut files = ProjectFiles::default();

    if let Ok(bytes) = fs::read(config.join(PROPERTIES_FILE)) {
        files.settings = property_overrides(&String::from_utf8_lossy(&bytes));
    }

    let path = config.join(PC_VALUES_FILE);
    if let Ok(bytes) = fs::read(&path) {
        match Msq::parse(&bytes) {
            Ok(msq) => files.pc_values = Some(msq),
            Err(error) => tracing::warn!(
                path = %path.display(),
                %error,
                "the project PC variable values could not be parsed and were skipped"
            ),
        }
    }

    files
}

/// Maps a `project.properties` text to setting overrides.
///
/// Only `ecuSettings` matters: its value is a `|`-separated list of active
/// names, with the same `_ON` / `_OFF` suffix convention the `.msq` snapshot
/// uses.
fn property_overrides(text: &str) -> Vec<(String, bool)> {
    let mut overrides = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("ecuSettings") {
            continue;
        }

        for item in value.split('|') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            overrides.push(flags::state(item));
        }
    }

    overrides
}

/// Maps a `<settings>` snapshot to preprocessor overrides.
///
/// The heuristic is the one the file format carries: `_ON`/`_OFF` suffixes
/// select a boolean flag, a bare name is an active choice. Setting-group
/// entries (`boostUnits` → `BOOSTPSI`) also become active names; that only
/// matters if the INI tests such a name in `#if`.
pub(super) fn settings_overrides(entries: &[(String, String)]) -> Vec<(String, bool)> {
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
