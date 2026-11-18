//! The `.oxtuner/` sidecar: state that is ours, never `TunerStudio`'s.
//!
//! ADR-0007 allows exactly one place inside a project for ox-tuner's own
//! files: the `.oxtuner/` directory. That is `project.json`: which INI a tune
//! was opened with (relative path, SHA-256 and firmware signature), which
//! connection the project last used, and the project's panel layout.
//!
//! The file is pretty-printed JSON so it can be inspected and diffed next to
//! `settings.json`. A missing file reads as "no state yet". A malformed file
//! is an error the caller falls back from, so a corrupt sidecar never blocks
//! opening the tune; the next write heals it. A file from a newer build is
//! refused instead and left untouched, so a downgrade cannot destroy it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Directory name inside a project.
pub const SIDECAR_DIR: &str = ".oxtuner";
/// Project state file inside the sidecar directory.
pub const STATE_FILE: &str = "project.json";

/// Bump when the on-disk shape changes in a way that needs migration.
///
/// The app is unreleased, so a stale file is deleted and recreated instead of
/// migrated. The version exists so a newer file is refused rather than
/// half-read.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// Per-project state: definition reference, last-used connection and panel
/// layout.
///
/// Passive data, so every field is public. Every section has a default, so a
/// file that predates a section still reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectState {
    /// On-disk schema version; refused when it does not match the build.
    pub schema_version: u32,
    /// Which definition the tune is interpreted through.
    pub definition: DefinitionRef,
    /// The connection this project last used.
    pub connection: ConnectionState,
    /// The project's panel layout.
    pub layout: ProjectLayout,
}

impl Default for ProjectState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            definition: DefinitionRef::default(),
            connection: ConnectionState::default(),
            layout: ProjectLayout::default(),
        }
    }
}

/// Which definition a tune was opened with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DefinitionRef {
    /// Definition path relative to the project root, with `/` separators.
    pub path: String,
    /// SHA-256 of the definition file's bytes, as lowercase hex.
    pub sha256: String,
    /// Firmware signature the definition declares.
    pub signature: String,
}

/// The connection this project last used.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionState {
    /// Last-used serial port, when one was chosen while this project was open.
    pub last_port: Option<String>,
    /// Baud override paired with that port, when one was set.
    pub baud_override: Option<u32>,
}

/// The project's panel layout.
///
/// Passive data: panel sizes and visibility the app echoes while the project is
/// open. The core only ferries it between the state file and the app and never
/// interprets it.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each panel visibility is an independent layout fact"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectLayout {
    /// Navigation sidebar width, in pixels.
    pub primary_sidebar_width: f32,
    /// Inspector sidebar width, in pixels.
    pub secondary_sidebar_width: f32,
    /// Bottom panel height, in pixels.
    pub bottom_panel_height: f32,
    /// Whether the navigation sidebar is open.
    pub primary_sidebar_visible: bool,
    /// Whether the inspector sidebar is open.
    pub secondary_sidebar_visible: bool,
    /// Whether the bottom panel is open.
    pub bottom_panel_visible: bool,
    /// Whether the table grid draws its Y axis downward.
    pub table_low_at_top: bool,
}

impl ProjectLayout {
    /// Width of the navigation sidebar, in pixels, for a fresh project.
    pub const DEFAULT_PRIMARY_SIDEBAR_WIDTH: f32 = 240.;
    /// Width of the inspector sidebar, in pixels, for a fresh project.
    pub const DEFAULT_SECONDARY_SIDEBAR_WIDTH: f32 = 280.;
    /// Height of the bottom panel, in pixels, for a fresh project.
    pub const DEFAULT_BOTTOM_PANEL_HEIGHT: f32 = 180.;
}

impl Default for ProjectLayout {
    fn default() -> Self {
        Self {
            primary_sidebar_width: Self::DEFAULT_PRIMARY_SIDEBAR_WIDTH,
            secondary_sidebar_width: Self::DEFAULT_SECONDARY_SIDEBAR_WIDTH,
            bottom_panel_height: Self::DEFAULT_BOTTOM_PANEL_HEIGHT,
            primary_sidebar_visible: true,
            secondary_sidebar_visible: true,
            bottom_panel_visible: true,
            table_low_at_top: false,
        }
    }
}

impl ProjectState {
    /// Records which definition the tune is interpreted through.
    pub fn set_definition(
        &mut self,
        path: impl Into<String>,
        sha256: impl Into<String>,
        signature: impl Into<String>,
    ) {
        self.definition = DefinitionRef {
            path: path.into(),
            sha256: sha256.into(),
            signature: signature.into(),
        };
    }

    /// Whether `bytes` are the definition this state was written for.
    pub fn definition_matches(&self, bytes: &[u8]) -> bool {
        self.definition.sha256 == crate::hash::hex(&crate::hash::sha256(bytes))
    }

    /// Reads the state file, or `None` when the sidecar has never been written.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the sidecar exists but cannot be read, an
    /// invalid-data error when it is malformed, and an unsupported error when
    /// it follows a newer schema. Malformed means "start fresh": the caller
    /// falls back to the default and the next write heals the file. Newer
    /// means "hands off": the caller must not overwrite it.
    pub fn read(project_root: &Path) -> io::Result<Option<Self>> {
        let path = project_root.join(SIDECAR_DIR).join(STATE_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };

        let mut state: Self = serde_json::from_str(&text).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the project state is malformed: {error}"),
            )
        })?;

        if state.schema_version > STATE_SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "project state schema {} needs a newer build; leaving it untouched",
                    state.schema_version
                ),
            ));
        }
        // An older file is read as it stands and stamped with the current
        // version, so the next write cannot keep a stale number alive.
        state.schema_version = STATE_SCHEMA_VERSION;

        Ok(Some(state))
    }

    /// Writes the state file, creating `.oxtuner/` when needed.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the sidecar directory or file cannot be
    /// written.
    pub fn write(&self, project_root: &Path) -> io::Result<PathBuf> {
        let directory = project_root.join(SIDECAR_DIR);
        fs::create_dir_all(&directory)?;

        let text = serde_json::to_string_pretty(self).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the project state cannot be serialized: {error}"),
            )
        })?;

        let path = directory.join(STATE_FILE);
        crate::atomic::write(&path, text.as_bytes())?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "oxtuner-sidecar-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn populated() -> ProjectState {
        let mut state = ProjectState::default();
        state.set_definition(
            "projectCfg/mainController.ini",
            "abc123",
            "speeduino 202501",
        );
        state.connection.last_port = Some("/dev/ttyACM0".to_owned());
        state.connection.baud_override = Some(115_200);
        state.layout.primary_sidebar_visible = false;
        state
    }

    #[test]
    fn state_round_trips_with_connection_and_layout() {
        let root = temp_project("roundtrip");
        let state = populated();

        state.write(&root).unwrap();
        let read = ProjectState::read(&root).unwrap().expect("written");

        assert_eq!(read, state);
        assert!(!read.definition_matches(b"anything"));

        fs::remove_dir_all(&root).unwrap();
    }

    /// The on-disk names are a contract: a reader sees a connection and a
    /// layout whose fields say which panel they belong to.
    #[test]
    fn the_file_names_the_connection_and_the_layout_panels() {
        let root = temp_project("keys");
        populated().write(&root).unwrap();
        let text = fs::read_to_string(root.join(SIDECAR_DIR).join(STATE_FILE)).unwrap();

        for key in [
            "\"connection\"",
            "\"last_port\"",
            "\"primary_sidebar_width\"",
            "\"secondary_sidebar_width\"",
            "\"bottom_panel_height\"",
            "\"primary_sidebar_visible\"",
            "\"secondary_sidebar_visible\"",
            "\"bottom_panel_visible\"",
            "\"table_low_at_top\"",
        ] {
            assert!(text.contains(key), "{key} is written: {text}");
        }
        for key in [
            "\"device\"",
            "\"primary_width\"",
            "\"secondary_width\"",
            "\"bottom_height\"",
            "\"primary_visible\"",
            "\"secondary_visible\"",
            "\"bottom_visible\"",
        ] {
            assert!(!text.contains(key), "{key} must not be written: {text}");
        }

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_definition_reference_verifies_the_bytes() {
        let root = temp_project("matches");
        let bytes = b"[MegaTune]\nsignature = \"speeduino 202501\"\n";
        let mut state = ProjectState::default();
        state.set_definition(
            "projectCfg/mainController.ini",
            crate::hash::hex(&crate::hash::sha256(bytes)),
            "speeduino 202501",
        );

        assert!(state.definition_matches(bytes));
        assert!(!state.definition_matches(b"changed"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_sidecar_is_not_an_error() {
        let root = temp_project("missing");
        assert_eq!(ProjectState::read(&root).unwrap(), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_malformed_sidecar_is_an_error_the_caller_falls_back_from() {
        let root = temp_project("malformed");
        fs::create_dir_all(root.join(SIDECAR_DIR)).unwrap();
        fs::write(root.join(SIDECAR_DIR).join(STATE_FILE), "{ not json").unwrap();

        let error = ProjectState::read(&root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_half_read() {
        let root = temp_project("schema");
        let mut state = populated();
        state.schema_version = STATE_SCHEMA_VERSION + 1;
        // A newer file is still well-formed JSON, so the write succeeds and
        // the version gate is what refuses it.
        state.write(&root).unwrap();

        let error = ProjectState::read(&root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_older_schema_reads_and_is_stamped_with_the_current_version() {
        let root = temp_project("older-schema");
        fs::create_dir_all(root.join(SIDECAR_DIR)).unwrap();
        fs::write(
            root.join(SIDECAR_DIR).join(STATE_FILE),
            r#"{"schema_version": 0, "connection": {"last_port": "/dev/ttyACM0"}}"#,
        )
        .unwrap();

        let read = ProjectState::read(&root).unwrap().expect("readable");
        assert_eq!(read.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(read.connection.last_port.as_deref(), Some("/dev/ttyACM0"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unknown_fields_are_tolerated_on_read() {
        let root = temp_project("forward-compat");
        fs::create_dir_all(root.join(SIDECAR_DIR)).unwrap();
        fs::write(
            root.join(SIDECAR_DIR).join(STATE_FILE),
            r#"{"schema_version": 1, "future_section": {"x": 1}}"#,
        )
        .unwrap();

        let read = ProjectState::read(&root).unwrap().expect("readable");
        assert_eq!(read, ProjectState::default());

        fs::remove_dir_all(&root).unwrap();
    }
}
