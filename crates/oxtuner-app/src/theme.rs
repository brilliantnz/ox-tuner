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

use std::path::PathBuf;

use gpui_kit::component::{Theme, ThemeRegistry};
use gpui_kit::{App, SharedString};

use crate::platform::paths::AppPaths;
use crate::settings::preferences::{AppSettings, apply_theme};

/// The light theme's name in the registry and in the theme file.
pub const LIGHT_NAME: &str = "OxTuner Light";
/// The dark theme's name in the registry and in the theme file.
pub const DARK_NAME: &str = "OxTuner Dark";
/// The file in the themes directory that carries the bundled set.
const BUNDLED_FILE_NAME: &str = "oxtuner.json";
/// The theme set shipped with the app, also written to disk on first launch.
const BUNDLED: &str = include_str!("../themes/oxtuner.json");

/// The seeded file: `<config>/themes/oxtuner.json`.
fn bundled_path(paths: &AppPaths) -> PathBuf {
    paths.themes_dir().join(BUNDLED_FILE_NAME)
}

/// Writes the bundled theme set into the config directory on first launch.
///
/// An existing file is never overwritten: once seeded, the file belongs to
/// the user. A failure is logged, not fatal — the in-memory bundled set is
/// still installed by [`register`], so the app keeps its colors.
pub fn seed(paths: &AppPaths) {
    let path = bundled_path(paths);
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(%error, path = %path.display(), "could not create themes directory");
        return;
    }
    if let Err(error) = std::fs::write(&path, BUNDLED) {
        tracing::warn!(%error, path = %path.display(), "could not write bundled theme");
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
    fn seed_writes_the_bundled_file_once_and_keeps_user_edits() {
        let paths = temp_paths("seed");
        let directory = paths.config_dir.clone();

        seed(&paths);
        let path = bundled_path(&paths);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read seeded theme"),
            BUNDLED
        );

        std::fs::write(&path, r#"{"name": "user edit"}"#).expect("write user edit");
        seed(&paths);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read user theme"),
            r#"{"name": "user edit"}"#,
            "reseeding must not clobber user edits"
        );

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
