//! The OxTuner color themes.
//!
//! The app does not use gpui-kit's built-in Default theme. It ships its own
//! pair — OxTuner Light and OxTuner Dark — as a theme set in `<config>/themes/oxtuner.json`, next to
//! `settings.json` and `state.json`. The file is user-editable and hot
//! reloaded, so a user can tune any token and see the change without a
//! restart.
//!
//! Start-up calls [`seed`] before the UI exists (plain file IO, no GPUI
//! context needed) and [`register`] once inside `Application::run`: the
//! bundled set is installed synchronously so the first frame already uses it,
//! and the directory is watched so later edits reload. The System / Light /
//! Dark preference in settings keeps working — it now resolves to the OxTuner
//! pair instead of the kit defaults.
//!
//! The seeded file is user-owned, so it is not replaced on upgrade. It carries
//! a schema version, and a version older than the bundle is migrated by adding
//! the keys it is missing. Existing values and unknown keys are kept. An
//! unreadable file is moved aside, the same way the settings and state stores
//! handle one.

use std::path::{Path, PathBuf};

use gpui_kit::component::{Theme, ThemeRegistry};
use gpui_kit::{App, SharedString};
use serde_json::Value;

use crate::platform::paths::AppPaths;
use crate::settings::preferences::{AppSettings, apply_theme};
use crate::state::store;

/// The light theme's name in the registry and in the theme file.
pub const LIGHT_NAME: &str = "OxTuner Light";
/// The dark theme's name in the registry and in the theme file.
pub const DARK_NAME: &str = "OxTuner Dark";
/// The file in the themes directory that carries the bundled set.
const BUNDLED_FILE_NAME: &str = "oxtuner.json";
/// The theme set shipped with the app, also written to disk on first launch.
const BUNDLED: &str = include_str!("../themes/oxtuner.json");
/// Bump when the bundled set gains a token or changes a theme name.
///
/// The seeded file records the version it came from. A lower version is
/// migrated by adding the keys it is missing; a higher one is left alone,
/// because this build cannot know what a newer file means.
const THEME_SCHEMA_VERSION: u32 = 1;
/// The key that carries [`THEME_SCHEMA_VERSION`] in the theme set.
const SCHEMA_VERSION_KEY: &str = "schema_version";

/// The seeded file: `<config>/themes/oxtuner.json`.
fn bundled_path(paths: &AppPaths) -> PathBuf {
    paths.themes_dir().join(BUNDLED_FILE_NAME)
}

/// Writes the bundled theme set into the config directory, then keeps it in
/// step across upgrades.
///
/// A missing file is written from the bundle. An existing file is user-owned:
/// a current version is left untouched, an older one gains the keys the bundle
/// defines and keeps every value the user set, and a newer one is left alone.
/// An unreadable file is moved aside so the user can recover it. A failure is
/// logged, not fatal — the in-memory bundled set is still installed by
/// [`register`], so the app keeps its colors.
pub fn seed(paths: &AppPaths) {
    let path = bundled_path(paths);

    let user = match store::read_json::<Value>(&path) {
        Ok(Some(value)) => value,
        Ok(None) => {
            write_bundled(&path);
            return;
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read theme file");
            match store::quarantine(&path) {
                Ok(backup) => {
                    tracing::warn!(path = %backup.display(), "moved unreadable theme aside");
                }
                Err(move_error) => {
                    tracing::warn!(
                        %move_error,
                        path = %path.display(),
                        "could not move unreadable theme aside"
                    );
                    return;
                }
            }
            write_bundled(&path);
            return;
        }
    };

    let user_version = schema_version(&user);
    if user_version > THEME_SCHEMA_VERSION {
        tracing::warn!(
            path = %path.display(),
            user_version,
            built_version = THEME_SCHEMA_VERSION,
            "theme file is newer than this build; leaving it alone"
        );
        return;
    }
    if user_version == THEME_SCHEMA_VERSION {
        return;
    }

    let mut migrated = user;
    merge_missing(&mut migrated, &bundled_value());
    migrated[SCHEMA_VERSION_KEY] = Value::from(THEME_SCHEMA_VERSION);

    if let Err(error) = store::write_json(&path, &migrated) {
        tracing::warn!(%error, path = %path.display(), "could not migrate theme file");
    }
}

/// Parses the embedded set. The file is compiled in, so a parse failure is a
/// build-time bug and not something to recover from at runtime.
fn bundled_value() -> Value {
    serde_json::from_str(BUNDLED).expect("the bundled theme set is valid JSON")
}

fn write_bundled(path: &Path) {
    if let Err(error) = store::write_json(path, &bundled_value()) {
        tracing::warn!(%error, path = %path.display(), "could not write bundled theme");
    }
}

fn schema_version(value: &Value) -> u32 {
    value
        .get(SCHEMA_VERSION_KEY)
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or(0)
}

/// Adds every key the bundle defines and the user's copy does not.
///
/// Existing values win, and keys the bundle does not know stay. The `themes`
/// array is matched by `name`, so a user theme keeps its values and position,
/// and a bundled theme that is absent is appended.
fn merge_missing(user: &mut Value, bundled: &Value) {
    match (user, bundled) {
        (Value::Object(user), Value::Object(bundled)) => {
            for (key, bundled_value) in bundled {
                match user.get_mut(key) {
                    Some(user_value) => merge_missing(user_value, bundled_value),
                    None => {
                        user.insert(key.clone(), bundled_value.clone());
                    }
                }
            }
        }
        (Value::Array(user), Value::Array(bundled)) => {
            for bundled_item in bundled {
                let name = bundled_item.get("name").and_then(Value::as_str);
                let position = name.and_then(|name| {
                    user.iter()
                        .position(|item| item.get("name").and_then(Value::as_str) == Some(name))
                });

                match position {
                    Some(index) => merge_missing(&mut user[index], bundled_item),
                    None => user.push(bundled_item.clone()),
                }
            }
        }
        // A scalar or an unnamed array the user set: keep it.
        _ => {}
    }
}

/// Installs the OxTuner themes and watches the directory for edits.
///
/// The bundled set loads synchronously first so the first frame already uses
/// it; the watcher then reloads the directory (including the seeded file and
/// anything else the user drops next to it) and reinstalls on every change.
pub fn register(paths: &AppPaths, cx: &mut App) {
    install(cx);

    let themes_dir = paths.themes_dir();
    if let Err(error) = ThemeRegistry::watch_dir(themes_dir.clone(), cx, install) {
        tracing::warn!(%error, path = %themes_dir.display(), "could not watch themes directory");
    }
}

/// Loads the OxTuner pair into the active theme and applies the preference.
///
/// The registry keeps every loaded theme; `apply_config` selects which two
/// are the live light and dark ones. Afterwards the stored System / Light /
/// Dark preference decides the mode, exactly as before — only the colors
/// behind the modes changed.
fn install(cx: &mut App) {
    if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(BUNDLED) {
        tracing::error!(%error, "bundled theme set failed to parse");
        return;
    }

    let registry = ThemeRegistry::global(cx);
    let light = registry
        .themes()
        .get(&SharedString::from(LIGHT_NAME))
        .cloned();
    let dark = registry
        .themes()
        .get(&SharedString::from(DARK_NAME))
        .cloned();
    match (light, dark) {
        (Some(light), Some(dark)) => {
            Theme::update(cx, |theme| {
                theme.apply_config(&light);
                theme.apply_config(&dark);
            });
        }
        _ => {
            tracing::error!("OxTuner themes missing from the registry");
            return;
        }
    }

    apply_theme(cx.global::<AppSettings>().theme(), cx);
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::{Theme, ThemeMode, ThemeSet, try_parse_color};

    use super::*;

    fn bundled_set() -> ThemeSet {
        serde_json::from_str(BUNDLED).expect("bundled theme set must parse")
    }

    #[test]
    fn the_bundled_set_carries_one_light_and_one_dark_default() {
        let set = bundled_set();

        assert_eq!(set.themes.len(), 2);
        for theme in &set.themes {
            assert!(
                theme.is_default,
                "{} must be marked as the default",
                theme.name
            );
        }

        let light = set.themes.iter().find(|theme| theme.name == LIGHT_NAME);
        let dark = set.themes.iter().find(|theme| theme.name == DARK_NAME);
        let (light, dark) = (light.expect("light theme"), dark.expect("dark theme"));
        assert!(!light.mode.is_dark());
        assert!(dark.mode.is_dark());
    }

    #[test]
    fn every_bundled_color_value_parses() {
        let set = bundled_set();

        for theme in &set.themes {
            let colors = serde_json::to_value(&theme.colors).expect("theme colors must serialize");
            let object = colors.as_object().expect("colors must be an object");
            assert!(!object.is_empty(), "{} must define colors", theme.name);
            for (key, value) in object {
                let Some(color) = value.as_str() else {
                    continue;
                };
                assert!(
                    try_parse_color(color).is_ok(),
                    "{} has an unparsable color for {key}: {color}",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn every_bundled_color_key_is_a_theme_token() {
        let set = bundled_set();
        let known =
            serde_json::to_value(&set.themes[0].colors).expect("theme colors must serialize");
        let known = known.as_object().expect("theme colors must be an object");

        let raw: serde_json::Value =
            serde_json::from_str(BUNDLED).expect("bundled theme set must parse");
        let themes = raw["themes"].as_array().expect("themes must be an array");

        for theme in themes {
            let name = theme["name"].as_str().expect("theme name");
            let colors = theme["colors"]
                .as_object()
                .expect("theme colors must be an object");

            let unknown: Vec<_> = colors
                .keys()
                .filter(|key| !known.contains_key(*key))
                .collect();

            assert!(
                unknown.is_empty(),
                "{name} has color keys gpui-kit does not know: {unknown:?}"
            );
        }
    }

    #[test]
    fn the_bundled_highlight_sections_define_an_editor_style() {
        let set = bundled_set();

        for theme in &set.themes {
            let highlight = theme
                .highlight
                .as_ref()
                .unwrap_or_else(|| panic!("{} must define highlight colors", theme.name));
            assert!(
                highlight.editor_background.is_some(),
                "{} needs an editor background",
                theme.name
            );
            assert!(
                highlight.editor_foreground.is_some(),
                "{} needs an editor foreground",
                theme.name
            );
        }
    }

    fn temp_paths(tag: &str) -> AppPaths {
        let directory = std::env::temp_dir().join(format!(
            "oxtuner-theme-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        AppPaths {
            config_dir: directory.clone(),
            logs_dir: directory.join("logs"),
        }
    }

    #[test]
    fn seed_writes_the_bundled_file_and_keeps_a_current_user_edit() {
        let paths = temp_paths("seed");
        let directory = paths.config_dir.clone();

        seed(&paths);
        let path = bundled_path(&paths);
        let seeded: Value = store::read_json(&path)
            .expect("read seeded theme")
            .expect("seeded file");
        assert_eq!(seeded, bundled_value());

        // A file at the current version is user-owned and must not be rewritten.
        let mut edited = seeded;
        edited["themes"][0]["colors"]["background"] = Value::from("#010203");
        store::write_json(&path, &edited).expect("write user edit");
        seed(&paths);

        let kept: Value = store::read_json(&path)
            .expect("read user theme")
            .expect("user file");
        assert_eq!(kept, edited, "a current theme file must not be clobbered");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_bundled_set_declares_its_version() {
        assert_eq!(schema_version(&bundled_value()), THEME_SCHEMA_VERSION);
    }

    #[test]
    fn an_old_theme_file_gains_new_tokens_and_keeps_edits() {
        let paths = temp_paths("migrate");
        let directory = paths.config_dir.clone();
        let path = bundled_path(&paths);

        let bundled = bundled_value();
        let mut old = bundled.clone();
        old.as_object_mut()
            .expect("theme set object")
            .remove(SCHEMA_VERSION_KEY);
        old["themes"][0]["colors"]
            .as_object_mut()
            .expect("colors object")
            .remove("secondary.background");
        old["themes"][0]["colors"]["background"] = Value::from("#010203");

        store::write_json(&path, &old).expect("write old theme");
        seed(&paths);

        let migrated: Value = store::read_json(&path)
            .expect("read migrated theme")
            .expect("migrated file");
        assert_eq!(schema_version(&migrated), THEME_SCHEMA_VERSION);
        assert_eq!(
            migrated["themes"][0]["colors"]["secondary.background"],
            bundled["themes"][0]["colors"]["secondary.background"]
        );
        assert_eq!(migrated["themes"][0]["colors"]["background"], "#010203");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_newer_theme_file_is_left_alone() {
        let paths = temp_paths("newer");
        let directory = paths.config_dir.clone();
        let path = bundled_path(&paths);

        let newer = serde_json::json!({
            "name": "future",
            "schema_version": THEME_SCHEMA_VERSION + 1,
            "themes": []
        });
        store::write_json(&path, &newer).expect("write newer theme");
        seed(&paths);

        let kept: Value = store::read_json(&path)
            .expect("read newer theme")
            .expect("newer file");
        assert_eq!(kept, newer);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_unreadable_theme_file_is_quarantined_and_reseeded() {
        let paths = temp_paths("broken");
        let directory = paths.config_dir.clone();
        let path = bundled_path(&paths);

        std::fs::create_dir_all(path.parent().expect("themes directory"))
            .expect("create themes directory");
        std::fs::write(&path, b"{ not json").expect("write broken theme");
        seed(&paths);

        let seeded: Value = store::read_json(&path)
            .expect("read seeded theme")
            .expect("reseeded file");
        assert_eq!(seeded, bundled_value());

        let quarantined = std::fs::read_dir(directory.join("themes"))
            .expect("read themes directory")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains("invalid-"));
        assert!(quarantined, "the unreadable file should be moved aside");

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The full path through the registry: the pair is installed as the live
    /// light and dark themes, and switching modes lands on the bundled colors
    /// from the bundled file rather than the kit defaults.

    #[gpui_kit::test]
    fn install_selects_the_oxtuner_pair_and_applies_its_colors(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::init);
        let paths = temp_paths("install");
        let directory = paths.config_dir.clone();

        cx.update(|cx| {
            cx.set_global(AppSettings::load(&paths));
            super::install(cx);

            let theme = Theme::global(cx);
            assert_eq!(theme.light_theme.name.to_string(), LIGHT_NAME);
            assert_eq!(theme.dark_theme.name.to_string(), DARK_NAME);
            assert_eq!(
                theme.dark_theme.colors.background.as_deref(),
                Some("#121314")
            );
            assert_eq!(
                theme.light_theme.colors.background.as_deref(),
                Some("#FFFFFF")
            );
        });

        for (mode, background, foreground, button) in [
            (ThemeMode::Dark, "#121314", "#bfbfbf", "#297AA0"),
            (ThemeMode::Light, "#FFFFFF", "#202020", "#0069CC"),
        ] {
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                let theme = Theme::global(cx);
                let expected_background =
                    try_parse_color(background).expect("bundled background must parse");
                let expected_foreground =
                    try_parse_color(foreground).expect("bundled foreground must parse");
                let expected_button = try_parse_color(button).expect("bundled button must parse");
                assert_eq!(theme.background, expected_background);
                assert_eq!(theme.foreground, expected_foreground);
                assert_eq!(theme.button, expected_button);
            });
        }

        let _ = std::fs::remove_dir_all(&directory);
    }
}
