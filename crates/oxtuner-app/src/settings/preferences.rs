//! Application settings: the user's durable preferences.
//!
//! Settings are a JSON document, not a binary blob, so they can be inspected,
//! edited and diffed. The global [`AppSettings`] owns the parsed document and
//! its file path; UI reads it through `AppSettings::global(cx)` and writes
//! through [`AppSettings::update`], which persists immediately (settings change
//! on a human timescale, unlike window geometry).
//!
//! Only settings that affect behaviour today are surfaced in the Settings
//! window. The connection slice exists in the schema so a later feature can add
//! fields without changing the file's shape, but it has no UI until there is a
//! transport to configure.

use std::{
    fs,
    path::{Path, PathBuf},
};

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::platform::paths::AppPaths;
use crate::state::store;

/// Bump when the on-disk shape changes in a way that needs migration.
pub(crate) const SETTINGS_SCHEMA_VERSION: u32 = 1;

/// How many recent projects are remembered.
const RECENT_PROJECTS_MAX: usize = 20;

/// The JSON document at `<config>/settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) schema_version: u32,
    pub(crate) ui: UiSettings,
    pub(crate) project: ProjectSettings,
    pub(crate) connection: ConnectionSettings,
    pub(crate) safety: SafetySettings,
    pub(crate) recent: RecentSettings,
    pub(crate) logging: LoggingSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            ui: UiSettings::default(),
            project: ProjectSettings::default(),
            connection: ConnectionSettings::default(),
            safety: SafetySettings::default(),
            recent: RecentSettings::default(),
            logging: LoggingSettings::default(),
        }
    }
}

impl Settings {
    /// Migrates a parsed document to the current schema and deserializes it.
    ///
    /// A document from a newer build is rejected rather than partially read:
    /// silently dropping fields the current build does not know about would
    /// make the next save destructive.
    pub(crate) fn from_json_value(mut value: Value) -> Result<Self, String> {
        let version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;

        match version {
            SETTINGS_SCHEMA_VERSION => {}
            0 => {
                // Version 0 is a file written before the field existed, or the
                // first-run state. Nothing needs rewriting yet; applying the
                // defaults below is the migration.
            }
            version if version < SETTINGS_SCHEMA_VERSION => {
                return Err(format!(
                    "settings schema {version} is older than the supported range"
                ));
            }
            version => {
                return Err(format!(
                    "settings schema {version} is newer than this build supports \
                     ({}); update ox-tuner instead of overwriting it",
                    SETTINGS_SCHEMA_VERSION
                ));
            }
        }

        if let Some(object) = value.as_object_mut() {
            object.insert("schema_version".into(), SETTINGS_SCHEMA_VERSION.into());
        }

        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    /// The paths to show in the "Recent projects" UI, newest first.
    pub(crate) fn recent_projects(&self) -> &[PathBuf] {
        &self.recent.projects
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UiSettings {
    pub(crate) theme: ThemePreference,
    /// BCP-47 code of the UI language. Only English ships today; the key is
    /// persisted so a new locale is a file plus a dropdown option, not a
    /// schema change.
    pub(crate) language: String,
    /// Whether a range selection also tints the row and column bins of the
    /// table editor. Off by default: a range is read on the cells, and a
    /// single selected cell marks its two bins either way.
    pub(crate) table_header_wash_for_ranges: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            language: DEFAULT_LANGUAGE.to_string(),
            table_header_wash_for_ranges: false,
        }
    }
}

/// The UI language this build ships.
pub(crate) const DEFAULT_LANGUAGE: &str = "en";

/// The theme the user asked for. `System` follows the OS appearance; the other
/// two pin the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    /// Parse the value stored in the dropdown field.
    pub(crate) fn from_key(key: &str) -> Self {
        match key {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    /// Stable key used by the dropdown; never translated.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Human-readable label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The concrete mode when this preference is not `System`.
    pub(crate) fn mode(self) -> Option<ThemeMode> {
        match self {
            Self::System => None,
            Self::Light => Some(ThemeMode::Light),
            Self::Dark => Some(ThemeMode::Dark),
        }
    }
}

/// Applies a theme preference to every window.
pub fn apply_theme(preference: ThemePreference, cx: &mut App) {
    match preference.mode() {
        Some(mode) => Theme::change(mode, None, cx),
        None => Theme::sync_system_appearance(None, cx),
    }
}

/// Selects the UI language and repaints every window.
///
/// The locale is global state outside GPUI's change tracking, so switching it
/// alone never schedules a render — the refresh is what makes translated
/// surfaces pick the new strings up.
pub(crate) fn apply_language(code: &str, cx: &mut App) {
    gpui_kit::component::set_locale(code);
    cx.refresh_windows();
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ProjectSettings {
    /// Reopen the last project at start-up. Off by default: a tune is opened
    /// deliberately, never as a side effect of launching the app.
    pub(crate) auto_reopen: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ConnectionSettings {
    pub(crate) last_port: Option<String>,
    pub(crate) baud_override: Option<u32>,
    pub(crate) poll_rate_hz: u32,
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            last_port: None,
            baud_override: None,
            poll_rate_hz: 20,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SafetySettings {
    pub(crate) disclaimer_accepted: bool,
    /// Whether the global write switch starts on (S8).
    ///
    /// Off by default: an installation that has never been told the ECU is at
    /// the bench must not write to it. The app passes this into the core at
    /// start-up and persists every explicit change.
    pub(crate) writes_enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RecentSettings {
    pub(crate) projects: Vec<PathBuf>,
}

impl RecentSettings {
    /// Adds a project to the front, removing duplicates by canonical path and
    /// keeping the list bounded.
    pub(crate) fn remember(&mut self, path: &Path) {
        let path = canonical(path);
        self.projects.retain(|existing| canonical(existing) != path);
        self.projects.insert(0, path);
        self.projects.truncate(RECENT_PROJECTS_MAX);
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LoggingSettings {
    /// Whether debug records go to the console and the file.
    pub(crate) verbose: bool,
}

/// The parsed settings document plus the file it came from.
///
/// A GPUI global because the component library's settings fields read and write
/// through `&App`/`&mut App` closures, not a view entity.
pub struct AppSettings {
    path: PathBuf,
    settings: Settings,
}

impl Global for AppSettings {}

impl AppSettings {
    /// Loads `<config>/settings.json`, falling back to defaults when it is
    /// missing or unreadable. An unreadable file is moved aside first so the
    /// next save cannot destroy it.
    pub fn load(paths: &AppPaths) -> Self {
        let path = paths.settings_file();
        let settings = match store::read_json::<Value>(&path) {
            Ok(Some(value)) => match Settings::from_json_value(value) {
                Ok(settings) => settings,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "settings file is invalid");
                    if let Err(move_error) = store::quarantine(&path) {
                        tracing::warn!(error = %move_error, "could not move invalid settings aside");
                    }
                    Settings::default()
                }
            },
            Ok(None) => Settings::default(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "could not read settings");
                if let Err(move_error) = store::quarantine(&path) {
                    tracing::warn!(error = %move_error, "could not move invalid settings aside");
                }
                Settings::default()
            }
        };

        Self { path, settings }
    }

    /// The settings document, for the crate's views.
    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The theme the user chose. Public so the binary can apply it without
    /// reaching into the schema.
    pub fn theme(&self) -> ThemePreference {
        self.settings.ui.theme
    }

    /// The interface language the user chose.
    pub fn language(&self) -> &str {
        &self.settings.ui.language
    }

    /// Whether the user asked for debug records.
    pub fn log_verbose(&self) -> bool {
        self.settings.logging.verbose
    }

    /// The connection preferences the dialog starts from.
    pub(crate) fn connection(&self) -> &ConnectionSettings {
        &self.settings.connection
    }

    /// Mutates the document and writes it back immediately.
    ///
    /// A write failure is logged rather than surfaced: losing a preference is
    /// not a reason to interrupt the task the user is in the middle of.
    pub(crate) fn update(cx: &mut App, mutate: impl FnOnce(&mut Settings)) {
        let path = {
            let global = cx.global_mut::<Self>();
            mutate(&mut global.settings);
            global.settings.schema_version = SETTINGS_SCHEMA_VERSION;
            global.path.clone()
        };

        let settings = &cx.global::<Self>().settings;
        if let Err(error) = store::write_json(&path, settings) {
            tracing::error!(%error, path = %path.display(), "could not save settings");
        }
    }

    /// Persists the document without changing it (save on exit).
    pub(crate) fn save(cx: &App) {
        let global = cx.global::<Self>();
        if let Err(error) = store::write_json(&global.path, &global.settings) {
            tracing::error!(
                %error,
                path = %global.path.display(),
                "could not save settings"
            );
        }
    }

    /// Changes the theme preference, applies it and persists it.
    pub(crate) fn set_theme(cx: &mut App, preference: ThemePreference) {
        Self::update(cx, |settings| settings.ui.theme = preference);
        apply_theme(preference, cx);
    }

    /// Changes the UI language, applies it and persists it.
    ///
    /// `update` already saves, and the locale is global state outside GPUI's
    /// change tracking, so the windows are refreshed explicitly after the
    /// switch.
    pub(crate) fn set_language(cx: &mut App, code: &str) {
        Self::update(cx, |settings| settings.ui.language = code.to_string());
        apply_language(code, cx);
    }

    /// Whether a range selection also tints the table's row and column bins.
    pub(crate) fn table_header_wash_for_ranges(cx: &App) -> bool {
        cx.try_global::<Self>()
            .is_some_and(|settings| settings.settings.ui.table_header_wash_for_ranges)
    }

    /// Changes the header wash preference and persists it.
    pub(crate) fn set_table_header_wash_for_ranges(cx: &mut App, wash: bool) {
        Self::update(cx, |settings| {
            settings.ui.table_header_wash_for_ranges = wash;
        });
    }

    /// Changes the verbose logging preference, applies it to the live filter
    /// and persists it.
    pub(crate) fn set_log_verbose(cx: &mut App, verbose: bool) {
        crate::platform::logging::set_verbose(cx, verbose);
        Self::update(cx, |settings| settings.logging.verbose = verbose);
    }

    /// Records that the safety disclaimer was accepted.
    pub(crate) fn set_disclaimer_accepted(cx: &mut App) {
        Self::update(cx, |settings| {
            settings.safety.disclaimer_accepted = true;
        });
    }

    /// Whether writes start enabled for the next launch.
    pub(crate) fn writes_enabled(cx: &App) -> bool {
        cx.global::<Self>().settings.safety.writes_enabled
    }

    /// Persists the global write switch.
    pub(crate) fn set_writes_enabled(cx: &mut App, enabled: bool) {
        Self::update(cx, |settings| settings.safety.writes_enabled = enabled);
    }

    /// Adds a project to the recent list and saves.
    pub(crate) fn remember_project(cx: &mut App, path: &Path) {
        Self::update(cx, |settings| settings.recent.remember(path));
    }

    /// Whether the last project reopens at start-up.
    pub(crate) fn auto_reopen(cx: &App) -> bool {
        cx.global::<Self>().settings.project.auto_reopen
    }

    /// Changes the start-up project behavior and persists it.
    pub(crate) fn set_auto_reopen(cx: &mut App, reopen: bool) {
        Self::update(cx, |settings| settings.project.auto_reopen = reopen);
    }

    /// Replaces the in-memory document and path; used by tests.
    #[cfg(test)]
    fn with(settings: Settings) -> Self {
        Self {
            path: PathBuf::from("/tmp/oxtuner-settings.json"),
            settings,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::*;

    #[test]
    fn a_malformed_file_is_kept_aside_and_replaced_by_defaults() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!(
            "oxtuner-settings-test-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create temp dir");

        let paths = AppPaths {
            config_dir: directory.clone(),
            cache_dir: directory.join("cache"),
            logs_dir: directory.join("logs"),
        };
        fs::write(paths.settings_file(), b"{ this is not json").expect("write malformed settings");

        let loaded = AppSettings::load(&paths);
        assert_eq!(loaded.settings(), &Settings::default());
        assert!(
            fs::read_dir(&directory)
                .expect("read temp dir")
                .filter_map(Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains("invalid-")),
            "the unreadable file should be quarantined"
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn settings_round_trip_through_json() {
        let settings = Settings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            ui: UiSettings {
                theme: ThemePreference::Dark,
                language: DEFAULT_LANGUAGE.to_string(),
                table_header_wash_for_ranges: true,
            },
            connection: ConnectionSettings {
                last_port: Some("/dev/tty.usbmodem1101".into()),
                baud_override: Some(115_200),
                poll_rate_hz: 30,
            },
            safety: SafetySettings {
                disclaimer_accepted: true,
                writes_enabled: true,
            },
            project: ProjectSettings { auto_reopen: true },
            recent: RecentSettings {
                projects: vec![PathBuf::from("/projects/roadster")],
            },
            logging: LoggingSettings { verbose: true },
        };

        let encoded = serde_json::to_string(&settings).expect("serialize");
        let decoded: Settings = serde_json::from_str(&encoded).expect("deserialize");

        assert_eq!(decoded, settings);
    }

    #[test]
    fn a_file_without_a_schema_version_is_migrated() {
        let value = json!({
            "ui": { "theme": "dark" },
            "safety": { "disclaimer_accepted": true }
        });

        let settings = Settings::from_json_value(value).expect("migrate");

        assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);
        assert_eq!(settings.ui.theme, ThemePreference::Dark);
        assert!(settings.safety.disclaimer_accepted);
        // Fields the old document did not know about get defaults.
        assert!(!settings.logging.verbose);
    }

    #[test]
    fn a_newer_schema_is_rejected() {
        let value = json!({ "schema_version": SETTINGS_SCHEMA_VERSION + 1 });

        let error = Settings::from_json_value(value).expect_err("must reject");

        assert!(error.contains("newer than this build"));
    }

    #[test]
    fn recent_projects_are_deduplicated_and_bounded() {
        let mut recent = RecentSettings::default();
        for index in 0..(RECENT_PROJECTS_MAX + 5) {
            recent.remember(Path::new(&format!("/projects/{index}")));
        }
        assert_eq!(recent.projects.len(), RECENT_PROJECTS_MAX);
        assert_eq!(
            recent.projects.first(),
            Some(&PathBuf::from(format!(
                "/projects/{}",
                RECENT_PROJECTS_MAX + 4
            )))
        );

        recent.remember(Path::new("/projects/7"));
        assert_eq!(recent.projects.len(), RECENT_PROJECTS_MAX);
        assert_eq!(recent.projects.first(), Some(&PathBuf::from("/projects/7")));
        assert_eq!(
            recent
                .projects
                .iter()
                .filter(|path| *path == Path::new("/projects/7"))
                .count(),
            1
        );
    }

    #[test]
    fn theme_keys_round_trip() {
        for preference in [
            ThemePreference::System,
            ThemePreference::Light,
            ThemePreference::Dark,
        ] {
            assert_eq!(ThemePreference::from_key(preference.key()), preference);
        }
    }

    /// A fresh installation must not add tint to the table's headers; only a
    /// single selected cell marks its bins until the user asks for more.
    #[test]
    fn the_range_header_wash_is_off_by_default() {
        assert!(!Settings::default().ui.table_header_wash_for_ranges);

        let document = json!({ "ui": { "theme": "dark" } });
        let migrated = Settings::from_json_value(document).expect("migrate");

        assert!(!migrated.ui.table_header_wash_for_ranges);
    }

    #[test]
    fn the_global_keeps_the_document_it_was_given() {
        let settings = Settings::default();
        let global = AppSettings::with(settings.clone());

        assert_eq!(global.settings(), &settings);
    }
}
