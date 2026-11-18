//! Creating a new `TunerStudio` project folder.
//!
//! Creation scaffolds the reference layout (ADR-0007): `CurrentTune.msq`
//! generated from the definition's defaults, `projectCfg/mainController.ini`
//! copied from the user-supplied file, a minimal `project.properties`, empty
//! `dashboard/` and `TuneView/` folders, and a fresh
//! `.oxtuner/project.json`. The definition is parsed and the files are built
//! before anything is written, and a failure past that point removes the
//! project folder creation made. A parent folder created for it to sit in
//! stays: deleting an ancestor the user may be using is worse than a leftover
//! empty folder.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use oxtuner_ini::SettingsContext;

use super::{CONFIG_DIR, MAIN_DEFINITION, PROPERTIES_FILE, Project, ProjectError, TUNE_FILE};
use crate::msq::Msq;
use crate::sidecar::ProjectLayout;

/// The longest folder name creation accepts.
///
/// A project folder sits inside `Documents/OxTunerProjects`, and the files
/// inside it add more path. 128 bytes stays far inside every desktop limit
/// while leaving room for a descriptive name.
const MAX_NAME_LEN: usize = 128;

/// Empty folders a new project carries for compatible tooling.
///
/// Neither reference project carries a logs folder at the top level — the
/// writer recreates what it needs on open — so only the view folders are
/// scaffolded.
const SCAFFOLD_DIRS: &[&str] = &["dashboard", "TuneView"];

/// What `Project::create` builds.
#[derive(Debug, Clone)]
pub struct CreateRequest {
    /// The folder the project is created inside.
    pub parent: PathBuf,
    /// Display name, also used as the folder name.
    pub name: String,
    /// The user-supplied definition file, copied into `projectCfg/`.
    ///
    /// Creation reads it itself: reading a file is project I/O, and the core
    /// thread — not the UI — owns that. A path that is not a `.ini` file, or
    /// cannot be read, is refused before anything is written.
    pub ini_path: PathBuf,
    /// Signature the ECU reported, when the project was detected from one.
    ///
    /// `None` means the user picked the INI by hand; the definition is used
    /// as-is without a signature check.
    pub expected_signature: Option<String>,
    /// `#if` flags chosen from the definition's setting groups.
    ///
    /// Each entry is an option name the user picked (for example
    /// `pressure_bar`), and the display choices the wizard offers
    /// (`LAMBDA`, `CELSIUS`). The definition is parsed with them active, so the
    /// tune snapshot records the same choices the user sees in the dialog. Only
    /// names the definition declares in a setting group or tests with `#if`
    /// are written to `ecuSettings`; an unknown name still activates nothing.
    /// An empty list keeps the definition's own defaults.
    pub settings: Vec<String>,
    /// Serial port and baud the project starts on, when the wizard picked one.
    ///
    /// `None` means no connection choice: the definition's own rate answers
    /// the first connect.
    pub connection: Option<ConnectionSeed>,
    /// The panel layout the project starts with.
    ///
    /// Creation is not the place to invent a layout: the caller hands over the
    /// one the user already arranged, so a new project keeps the workspace as
    /// it is.
    pub layout: ProjectLayout,
}

/// A serial device a new project should remember.
#[derive(Debug, Clone)]
pub struct ConnectionSeed {
    /// Device path.
    pub port: String,
    /// Bits per second.
    pub baud: u32,
}

impl CreateRequest {
    /// The folder the project would occupy.
    ///
    /// The name is trimmed the way creation trims it, so a log line and the
    /// folder that creation writes name the same path.
    pub fn root(&self) -> PathBuf {
        self.parent.join(self.name.trim())
    }
}

/// The folder and the project name inside a typed destination path.
///
/// This is the inverse of [`CreateRequest::root`], and lives beside it so the
/// two cannot disagree. Both separators count on every platform, so a Windows
/// style path works while typing on macOS or Linux too. A trailing separator
/// means the user named a folder only, and the project name then stays whatever
/// it was. A single component is a name inside the folder the caller already
/// has.
pub fn split_destination(typed: &Path) -> (Option<PathBuf>, Option<String>) {
    let text = typed.as_os_str().to_string_lossy();

    // A trailing separator means the user named a folder, not a project.
    let folder = text.trim_end_matches(['/', '\\']);
    if folder.len() != text.len() {
        let mut folder = folder.to_string();

        // A root keeps its separator. Without it "C:" is drive-relative on
        // Windows, so the project would land in the drive's current folder.
        if folder.ends_with(':') {
            folder.push(text.chars().last().unwrap_or('/'));
        }
        // A lone separator is the filesystem root itself, not an empty folder.
        if folder.is_empty() && text.len() == 1 {
            folder.push(text.chars().next().unwrap_or('/'));
        }

        let folder = (!folder.is_empty()).then(|| PathBuf::from(folder));

        return (folder, None);
    }

    let Some(cut) = text.rfind(['/', '\\']) else {
        return (None, Some(text.to_string()));
    };

    let mut folder = text[..cut].to_string();

    // A root keeps its separator: "/name" lives in "/", "C:\name" lives in "C:\".
    if folder.is_empty() || folder.ends_with(':') {
        let separator = text[cut..].chars().next().unwrap_or('/');
        folder.push(separator);
    }

    let file_name = text[cut + 1..].to_string();
    let file_name = (!file_name.is_empty()).then_some(file_name);

    (Some(PathBuf::from(folder)), file_name)
}

/// Whether `path` names a definition file.
///
/// The native file picker cannot filter by extension, so this is the one rule
/// for what may become a project definition. Creation applies it before reading
/// anything; the UI applies the same function so a wrong pick explains itself
/// immediately instead of at Create.
pub fn accepts_definition(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ini"))
}

impl Project {
    /// Creates a project folder and opens it.
    ///
    /// The folder is `parent/name`, and it must not exist. The name must be a
    /// usable folder name on every desktop OS. When the ECU reported a
    /// signature, the definition must declare the same one.
    ///
    /// The definition is parsed twice — once to scaffold the tune, once by
    /// the open that proves the folder is readable — and the second parse does
    /// not use the definition cache. Creation is a one-shot user action, so the
    /// few milliseconds are not worth a cache parameter here; later opens of
    /// the same project are cached normally.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the name or target is unusable, the
    /// definition cannot be parsed, a reported signature mismatches, or a
    /// file cannot be written. A failure removes what creation wrote.
    pub fn create(request: &CreateRequest) -> Result<Self, ProjectError> {
        let root = request.root();
        let _span = tracing::info_span!(
            "project_create",
            root = %root.display(),
            name = request.name.trim()
        )
        .entered();

        let result = Self::create_inner(request);

        match &result {
            Ok(project) => tracing::info!(
                root = %project.root().display(),
                signature = %project.definition().signature,
                pages = project.definition().pages.len(),
                constants = project.definition().constants.len(),
                "project created"
            ),
            Err(error) => tracing::warn!(
                error_kind = error.kind(),
                %error,
                "project creation failed"
            ),
        }

        result
    }

    /// The body of [`Self::create`], split out so one place logs its outcome.
    fn create_inner(request: &CreateRequest) -> Result<Self, ProjectError> {
        let name = validate_project_name(&request.name)?;
        let root = request.root();
        ensure_target_free(&root)?;
        let ini_bytes = read_definition(&request.ini_path)?;
        tracing::debug!(
            path = %request.ini_path.display(),
            bytes = ini_bytes.len(),
            "definition read for the new project"
        );

        // Everything is validated and built in memory before the folder
        // exists, so a refused definition touches no disk at all.
        let built = build_scaffold(&name, request, &ini_bytes)?;

        // The target was checked above; `create_dir` makes that check atomic
        // by refusing a folder that appeared in between. Reusing it would make
        // the rollback below delete a folder creation did not make.
        if let Some(parent) = root.parent() {
            fs::create_dir_all(parent).map_err(|source| ProjectError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        fs::create_dir(&root).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                ProjectError::TargetExists { path: root.clone() }
            } else {
                ProjectError::Io {
                    path: root.clone(),
                    source,
                }
            }
        })?;

        let mut written: Vec<PathBuf> = Vec::new();
        let project = Self::write_scaffold(&root, request, &ini_bytes, &built, &mut written)
            .inspect_err(|_| rollback(&root, &written))?;

        Ok(project)
    }

    /// Writes the scaffold, then opens it through the normal open path so a
    /// created project is exactly an opened one.
    ///
    /// Only I/O happens here: every value was built by [`build_scaffold`], so a
    /// failure means the disk refused a write and rollback removes what was
    /// written. The state file is written last, once the project provably
    /// opens.
    fn write_scaffold(
        root: &Path,
        request: &CreateRequest,
        ini_bytes: &[u8],
        built: &BuiltScaffold,
        written: &mut Vec<PathBuf>,
    ) -> Result<Self, ProjectError> {
        let config = root.join(CONFIG_DIR);
        let definition_path = config.join(MAIN_DEFINITION);

        create_tracked(written, config.clone())?;
        write_tracked(written, definition_path, ini_bytes)?;
        write_tracked(written, root.join(TUNE_FILE), &built.tune_bytes)?;

        let properties_path = config.join(PROPERTIES_FILE);
        write_tracked(
            written,
            properties_path.clone(),
            built.properties.as_bytes(),
        )?;

        // TunerStudio rotates a `.bkup` beside its properties file and reads it
        // when the main file is damaged. Writing the same bytes now means a
        // project this build created never looks half-written there.
        let mut backup = properties_path.into_os_string();
        backup.push(".bkup");
        write_tracked(written, PathBuf::from(backup), built.properties.as_bytes())?;

        for dir in SCAFFOLD_DIRS {
            create_tracked(written, root.join(dir))?;
        }

        tracing::debug!(
            settings = request.settings.len(),
            "project properties and backup written"
        );

        let mut project = Self::open(root)?;
        if let Some(connection) = &request.connection {
            project.state.connection.last_port = Some(connection.port.clone());
            project.state.connection.baud_override = Some(connection.baud);
        }
        project.state.layout = request.layout.clone();

        // A created project must be usable: if the scaffold opened read-only,
        // the definition and the tune disagree about something creation wrote,
        // and handing it back would only move that failure to the first edit.
        if project.is_read_only() {
            return Err(ProjectError::ScaffoldRejected {
                reasons: project
                    .report()
                    .blockers
                    .iter()
                    .map(|blocker| blocker.message.clone())
                    .collect(),
            });
        }

        project
            .state
            .write(root)
            .map_err(|source| ProjectError::Sidecar {
                path: root.join(crate::sidecar::SIDECAR_DIR),
                source,
            })?;

        Ok(project)
    }
}

/// The bytes a create writes once its inputs prove valid.
///
/// Built before the target folder exists, so a definition that cannot become a
/// project never leaves anything behind.
struct BuiltScaffold {
    tune_bytes: Vec<u8>,
    properties: String,
}

/// Validates the request and builds every file's bytes in memory.
///
/// Nothing here touches the target folder. The definition is parsed, its
/// include and signature gates run, the tune is scaffolded and serialized, and
/// the properties text is assembled; only [`Project::write_scaffold`] writes.
fn build_scaffold(
    name: &str,
    request: &CreateRequest,
    ini_bytes: &[u8],
) -> Result<BuiltScaffold, ProjectError> {
    let text = oxtuner_ini::source::decode(ini_bytes).text;
    // A chosen setting group marks its option active, exactly like the
    // `<settings>` entry a saved tune carries. The parser only sees the names
    // as `#if` conditions, so it is given the request as written.
    let overrides: Vec<(String, bool)> = request
        .settings
        .iter()
        .filter(|option| !option.eq_ignore_ascii_case("DEFAULT"))
        .cloned()
        .map(|option| (option, true))
        .collect();
    let settings = SettingsContext::new().with_overrides(overrides);
    // The parser resolves `#include` through a `Fn` resolver, so the names
    // it asks for are recorded through interior mutability. The base is the
    // project's own config folder, which does not exist yet: an include can
    // never resolve there, and recording the name is what refuses it.
    let includes = RefCell::new(Vec::new());
    let config = request.root().join(CONFIG_DIR);
    let resolver = |name: &str| {
        includes.borrow_mut().push(name.to_owned());
        super::read_include(&config, name)
    };
    let definition = oxtuner_ini::parse_str_with_includes(&text, &settings, &resolver)
        .map_err(ProjectError::Ini)?;
    tracing::debug!(
        signature = %definition.signature,
        pages = definition.pages.len(),
        constants = definition.constants.len(),
        settings = request.settings.len(),
        "definition parsed for the new project"
    );

    // Creation copies one file. A definition that pulls in more would open
    // with those pages missing, so it is refused instead of half-built.
    let includes = includes.into_inner();
    if !includes.is_empty() {
        return Err(ProjectError::DefinitionNeedsIncludes { names: includes });
    }

    if let Some(expected) = &request.expected_signature
        && definition.signature.trim() != expected.trim()
    {
        return Err(ProjectError::SignatureMismatch {
            expected: expected.clone(),
            found: definition.signature,
        });
    }

    let document = Msq::scaffold(&definition, &settings).map_err(ProjectError::Msq)?;
    let tune_bytes = document.save()?;
    tracing::debug!(
        bytes = tune_bytes.len(),
        "tune scaffolded from the definition defaults"
    );

    // The keys are the ones TunerStudio reads, so a project created here
    // opens there without a settings mismatch. `ecuSettings` is the flag
    // list the definition's setting groups produced; TunerStudio stores it
    // pipe-separated with a trailing separator.
    //
    // The signature is trimmed: a definition can carry a trailing NUL,
    // which a Java-properties file must not hold (a reader would see it as
    // part of the value and report the project as corrupt).
    let signature = definition.signature.trim();
    let mut ecu_settings = String::new();
    for flag in chosen_flags(&definition, &request.settings) {
        ecu_settings.push_str(&flag);
        ecu_settings.push('|');
    }

    let mut properties = String::from("# Created by Ox Tuner.\n");
    properties.push_str("projectName=");
    properties.push_str(&oxtuner_encoding::escape_java_property(name));
    properties.push('\n');
    properties.push_str("projectDescription=\n");
    properties.push_str("ecuConfigFile=");
    properties.push_str(MAIN_DEFINITION);
    properties.push('\n');
    if !signature.is_empty() {
        properties.push_str("firmwareDescription=");
        properties.push_str(&oxtuner_encoding::escape_java_property(signature));
        properties.push('\n');
    }
    properties.push_str("ecuSettings=");
    properties.push_str(&ecu_settings);
    properties.push('\n');

    Ok(BuiltScaffold {
        tune_bytes,
        properties,
    })
}

/// Reads the definition creation will copy.
///
/// Refused before anything is written, with the path in the error so the UI can
/// say which pick failed.
fn read_definition(path: &Path) -> Result<Vec<u8>, ProjectError> {
    if !accepts_definition(path) {
        return Err(ProjectError::DefinitionNotIni {
            path: path.to_owned(),
        });
    }

    fs::read(path).map_err(|source| ProjectError::Io {
        path: path.to_owned(),
        source,
    })
}

/// The option names that stand for a flag, in the order the caller listed them.
///
/// The option named `DEFAULT` is not a flag: the dialect ignores it and falls
/// into the `#else` branch, so a project must not record it. A name the
/// definition's setting groups never declare is dropped too, because it cannot
/// compile a branch and would only add a setting the definition does not know.
/// Duplicates collapse to their first occurrence.
fn chosen_flags(definition: &oxtuner_ini::Definition, settings: &[String]) -> Vec<String> {
    let declared = |option: &str| {
        definition
            .setting_groups
            .values()
            .flat_map(|group| group.options.iter())
            .any(|candidate| candidate.name.eq_ignore_ascii_case(option))
            // The display choices the wizard offers are only ever tested with
            // `#if`, never declared as a setting group, so the flags the file
            // tests count too.
            || definition
                .tested_flags
                .iter()
                .any(|tested| tested.eq_ignore_ascii_case(option))
    };

    let mut flags: Vec<String> = Vec::new();
    for option in settings {
        if option.eq_ignore_ascii_case("DEFAULT") || !declared(option) {
            continue;
        }
        if flags.iter().any(|flag| flag.eq_ignore_ascii_case(option)) {
            continue;
        }

        flags.push(option.clone());
    }

    flags
}

/// The folder name, trimmed and checked for every desktop OS.
///
/// Projects move between machines, so the Windows rules apply everywhere:
///
/// - empty after trimming, `.` or `..`, or longer than [`MAX_NAME_LEN`]
/// - a path separator, NUL or control character
/// - a Windows-reserved character `< > : " | ? *`, or a reserved device
///   name (`CON`, `NUL`, `COM1`…) with or without an extension
/// - a trailing space or dot, which Windows strips
///
/// The UI applies this before enabling its next step, and creation applies it
/// again before touching the disk: one rule, so a name the wizard accepted can
/// never fail later for a reason the wizard did not show.
///
/// # Errors
///
/// Returns [`ProjectError::InvalidName`] naming the reason the name cannot be
/// used.
pub fn validate_project_name(name: &str) -> Result<String, ProjectError> {
    let invalid = |reason: &'static str| ProjectError::InvalidName {
        name: name.to_owned(),
        reason,
    };

    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(invalid("the name is empty"));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(invalid("the name is not a usable folder name"));
    }
    if trimmed.len() > MAX_NAME_LEN {
        return Err(invalid("the name is longer than 128 bytes"));
    }
    if trimmed.chars().any(|character| {
        character == '/' || character == '\\' || character == '\0' || character.is_control()
    }) {
        return Err(invalid("the name holds a separator or control character"));
    }
    if trimmed
        .chars()
        .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return Err(invalid("the name holds a reserved character"));
    }
    if is_windows_device_name(trimmed) {
        return Err(invalid("the name is reserved by Windows"));
    }
    if trimmed.ends_with(' ') || trimmed.ends_with('.') {
        return Err(invalid("the name ends with a space or dot"));
    }

    Ok(trimmed.to_owned())
}

/// Whether `name` is a Windows-reserved device name, with or without an
/// extension (`NUL`, `nul.txt` and `COM1` are all refused by Windows).
fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Refuses a target that already exists, as a folder or as a file.
///
/// Creation only ever writes into a folder it made itself. A folder the user
/// already has may hold a hidden file (macOS writes `.DS_Store` into a folder
/// the moment it is opened in Finder), and a folder that looks empty in a
/// dialog is then not empty on disk, so "empty" is not a safe test. A file with
/// the project's name is refused with the same message instead of failing later
/// as a directory error.
///
/// The check and the creation are not atomic, but creation is a deliberate user
/// action and the worst case is a plain I/O error, never a merged folder: the
/// entry is re-checked by `create_dir_all` refusing an existing file.
///
/// # Errors
///
/// Returns [`ProjectError::TargetExists`] when anything is already there, and
/// [`ProjectError::Io`] when the path cannot be inspected.
pub fn ensure_target_free(root: &Path) -> Result<(), ProjectError> {
    match fs::symlink_metadata(root) {
        Ok(_) => Err(ProjectError::TargetExists {
            path: root.to_owned(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ProjectError::Io {
            path: root.to_owned(),
            source,
        }),
    }
}

/// Writes a file and records it for rollback.
fn write_tracked(
    written: &mut Vec<PathBuf>,
    path: PathBuf,
    bytes: &[u8],
) -> Result<(), ProjectError> {
    fs::write(&path, bytes).map_err(|source| ProjectError::Io {
        path: path.clone(),
        source,
    })?;
    written.push(path);
    Ok(())
}

/// Creates a directory and records it for rollback.
fn create_tracked(written: &mut Vec<PathBuf>, path: PathBuf) -> Result<(), ProjectError> {
    fs::create_dir_all(&path).map_err(|source| ProjectError::Io {
        path: path.clone(),
        source,
    })?;
    written.push(path);
    Ok(())
}

/// Removes the folder a failed creation made, best effort.
///
/// The folder is always one creation created itself, so it disappears whole;
/// `written` only feeds the debug line that lists what was there.
fn rollback(root: &Path, written: &[PathBuf]) {
    tracing::debug!(
        root = %root.display(),
        written = ?written,
        "removing the folder a failed creation wrote"
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use super::*;

    #[test]
    fn usable_names_trim_and_pass() {
        assert_eq!(validate_project_name("  Roadster  ").unwrap(), "Roadster");
        assert_eq!(
            validate_project_name("Sonic_150 (E85)").unwrap(),
            "Sonic_150 (E85)"
        );
    }

    #[test]
    fn unusable_names_name_their_reason() {
        for name in [
            "", "   ", ".", "..", "a/b", "a\\b", "a:b", "a*b", "trail.", "NUL", "com1", "nul.txt",
            "LPT9.csv",
        ] {
            assert!(
                matches!(
                    validate_project_name(name),
                    Err(ProjectError::InvalidName { .. })
                ),
                "{name:?} must be refused"
            );
        }
        let long = "x".repeat(MAX_NAME_LEN + 1);
        assert!(
            matches!(
                validate_project_name(&long),
                Err(ProjectError::InvalidName { .. })
            ),
            "a name over the cap must be refused"
        );
    }

    fn destination(path: &str) -> (Option<String>, Option<String>) {
        let (folder, file_name) = split_destination(Path::new(path));

        (folder.map(|folder| folder.display().to_string()), file_name)
    }

    #[test]
    fn a_destination_path_splits_into_folder_and_name() {
        assert_eq!(
            destination("/Users/me/OxTunerProjects/MyProject"),
            (
                Some("/Users/me/OxTunerProjects".to_owned()),
                Some("MyProject".to_owned())
            )
        );
    }

    #[test]
    fn a_trailing_separator_names_a_folder_only() {
        assert_eq!(
            destination("/Users/me/OxTunerProjects/"),
            (Some("/Users/me/OxTunerProjects".to_owned()), None)
        );
    }

    #[test]
    fn a_bare_component_is_a_name_inside_the_current_folder() {
        assert_eq!(
            destination("MyProject"),
            (None, Some("MyProject".to_owned()))
        );
    }

    #[test]
    fn a_windows_path_splits_on_backslashes() {
        assert_eq!(
            destination("C:\\Users\\me\\OxTunerProjects\\MyProject"),
            (
                Some("C:\\Users\\me\\OxTunerProjects".to_owned()),
                Some("MyProject".to_owned())
            )
        );
    }

    #[test]
    fn a_drive_root_keeps_its_separator() {
        assert_eq!(
            destination("C:\\MyProject"),
            (Some("C:\\".to_owned()), Some("MyProject".to_owned()))
        );
    }

    #[test]
    fn a_drive_root_on_its_own_stays_a_root() {
        assert_eq!(
            destination("C:\\"),
            (Some("C:\\".to_owned()), None),
            "without the separator C: is drive-relative"
        );
        assert_eq!(destination("C:/"), (Some("C:/".to_owned()), None));
    }

    #[test]
    fn a_lone_separator_is_the_root_itself() {
        assert_eq!(destination("/"), (Some("/".to_owned()), None));
    }

    #[test]
    fn a_unc_share_is_one_folder() {
        assert_eq!(
            destination("\\\\server\\share\\MyProject"),
            (
                Some("\\\\server\\share".to_owned()),
                Some("MyProject".to_owned())
            )
        );
    }

    #[test]
    fn a_posix_root_keeps_its_separator() {
        assert_eq!(
            destination("/MyProject"),
            (Some("/".to_owned()), Some("MyProject".to_owned()))
        );
    }

    #[test]
    fn a_windows_path_accepts_forward_slashes() {
        assert_eq!(
            destination("C:/Users/me/MyProject"),
            (Some("C:/Users/me".to_owned()), Some("MyProject".to_owned()))
        );
    }

    #[test]
    fn a_windows_trailing_separator_names_a_folder_only() {
        assert_eq!(
            destination("C:\\Users\\me\\OxTunerProjects\\"),
            (Some("C:\\Users\\me\\OxTunerProjects".to_owned()), None)
        );
    }

    #[test]
    fn a_refused_name_carries_a_stable_error_kind() {
        let error = validate_project_name("a/b").expect_err("the separator is refused");
        assert_eq!(error.kind(), "invalid-name");
    }

    #[test]
    fn only_ini_files_are_definitions() {
        assert!(accepts_definition(Path::new("speeduino.ini")));
        assert!(accepts_definition(Path::new("SPEEDUINO.INI")));
        assert!(!accepts_definition(Path::new("speeduino.txt")));
        assert!(!accepts_definition(Path::new("speeduino")));
    }
}
