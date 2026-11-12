//! Creating a new `TunerStudio` project folder.
//!
//! Creation scaffolds the reference layout (ADR-0007): `CurrentTune.msq`
//! generated from the definition's defaults, `projectCfg/mainController.ini`
//! copied from the user-supplied file, a minimal `project.properties`, empty
//! `dashboard/` and `TuneView/` folders, and a fresh
//! `.oxtuner/project.json`. The definition is parsed before anything is
//! written, and a failure past that point removes what creation wrote, so a
//! failed create leaves no half-built folder behind.

use std::fs;
use std::path::{Path, PathBuf};

use oxtuner_ini::SettingsContext;

use super::{CONFIG_DIR, MAIN_DEFINITION, PROPERTIES_FILE, Project, ProjectError, TUNE_FILE};
use crate::msq::Msq;

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
    /// The user-supplied definition bytes copied to `projectCfg/`.
    pub ini_bytes: Vec<u8>,
    /// Signature the ECU reported, when the project was detected from one.
    ///
    /// `None` means the user picked the INI by hand; the definition is used
    /// as-is without a signature check.
    pub expected_signature: Option<String>,
}

impl Project {
    /// Creates a project folder and opens it.
    ///
    /// The folder is `parent/name`. It must not exist or must be empty; the
    /// name must be a usable folder name on every desktop OS. When the ECU
    /// reported a signature, the definition must declare the same one.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the name or target is unusable, the
    /// definition cannot be parsed, a reported signature mismatches, or a
    /// file cannot be written. A failure removes what creation wrote.
    pub fn create(request: &CreateRequest) -> Result<Self, ProjectError> {
        let name = validate_name(&request.name)?;
        let root = request.parent.join(&name);
        ensure_target_empty(&root)?;

        let root_existed = root.exists();
        if !root_existed {
            fs::create_dir_all(&root).map_err(|source| ProjectError::Io {
                path: root.clone(),
                source,
            })?;
        }

        let mut written: Vec<PathBuf> = Vec::new();
        let project = Self::scaffold(&root, &name, request, &mut written).inspect_err(|_| {
            rollback(&root, root_existed, &written);
        })?;

        Ok(project)
    }

    /// Writes the scaffold, then opens it through the normal open path so a
    /// created project is exactly an opened one. The state file is written
    /// last, once the project provably opens: no failure past that point is
    /// possible, so rollback never meets a half-built folder.
    fn scaffold(
        root: &Path,
        name: &str,
        request: &CreateRequest,
        written: &mut Vec<PathBuf>,
    ) -> Result<Self, ProjectError> {
        let config = root.join(CONFIG_DIR);
        create_tracked(written, config.clone())?;

        let definition_path = config.join(MAIN_DEFINITION);
        write_tracked(written, definition_path, &request.ini_bytes)?;

        let text = oxtuner_ini::source::decode(&request.ini_bytes).text;
        let settings = SettingsContext::new();
        let resolver = |name: &str| super::read_include(&config, name);
        let definition = oxtuner_ini::parse_str_with_includes(&text, &settings, &resolver)
            .map_err(ProjectError::Ini)?;

        if let Some(expected) = &request.expected_signature
            && definition.signature.trim() != expected.trim()
        {
            return Err(ProjectError::SignatureMismatch {
                expected: expected.clone(),
                found: definition.signature,
            });
        }

        let document = Msq::scaffold(&definition, &settings).map_err(|source| {
            ProjectError::Unrepresentable {
                name: source.name(),
                source,
            }
        })?;
        let bytes = document.save()?;
        write_tracked(written, root.join(TUNE_FILE), &bytes)?;

        let properties = format!(
            "# Created by OxTuner.\nprojectName={name}\nsignature={}\n",
            definition.signature
        );
        write_tracked(written, config.join(PROPERTIES_FILE), properties.as_bytes())?;

        for dir in SCAFFOLD_DIRS {
            create_tracked(written, root.join(dir))?;
        }

        let project = Self::open(root)?;

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

/// The folder name, trimmed and checked for every desktop OS.
///
/// Projects move between machines, so the Windows rules apply everywhere:
///
/// - empty after trimming, `.` or `..`
/// - a path separator, NUL or control character
/// - a Windows-reserved character `< > : " | ? *`, or a reserved device
///   name (`CON`, `NUL`, `COM1`…) with or without an extension
/// - a trailing space or dot, which Windows strips
fn validate_name(name: &str) -> Result<String, ProjectError> {
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

/// Refuses a target that already holds files.
///
/// The check and the creation are not atomic, but creation is a deliberate
/// user action and the worst case is a plain I/O error, never a merged
/// folder: an existing non-empty folder is always refused.
fn ensure_target_empty(root: &Path) -> Result<(), ProjectError> {
    match fs::read_dir(root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(ProjectError::TargetNotEmpty {
                    path: root.to_owned(),
                });
            }
            Ok(())
        }
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

/// Removes what a failed creation wrote, best effort.
///
/// A root the creation itself made disappears whole; a pre-existing empty
/// folder keeps existing, with only the written paths removed.
fn rollback(root: &Path, root_existed: bool, written: &[PathBuf]) {
    if !root_existed {
        let _ = fs::remove_dir_all(root);
        return;
    }
    for path in written.iter().rev() {
        if path.is_dir() {
            let _ = fs::remove_dir(path);
        } else {
            let _ = fs::remove_file(path);
        }
    }
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
        assert_eq!(validate_name("  Roadster  ").unwrap(), "Roadster");
        assert_eq!(validate_name("Sonic_150 (E85)").unwrap(), "Sonic_150 (E85)");
    }

    #[test]
    fn unusable_names_name_their_reason() {
        for name in [
            "", "   ", ".", "..", "a/b", "a\\b", "a:b", "a*b", "trail.", "NUL", "com1", "nul.txt",
            "LPT9.csv",
        ] {
            assert!(
                matches!(validate_name(name), Err(ProjectError::InvalidName { .. })),
                "{name:?} must be refused"
            );
        }
    }
}
