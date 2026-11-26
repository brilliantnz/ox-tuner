//! The `OxTuner` color themes.
//!
//! The app does not use gpui-kit's built-in Default theme. It ships its own
//! hardcoded pair — `OxTuner` Light and `OxTuner` Dark — compiled in from
//! `themes/oxtuner.json`. There are no user theme files: [`register`] installs
//! the pair synchronously inside `Application::run` so the first frame already
//! uses it. The System / Light / Dark preference in settings keeps working —
//! it resolves to the `OxTuner` pair instead of the kit defaults.
//!
//! An older build seeded `<config>/themes/oxtuner.json` and watched it for
//! edits. This build never reads that file or the old `theme-bundled.json`
//! snapshot, so left-over copies are inert.

use gpui_kit::component::{Theme, ThemeRegistry};
use gpui_kit::{App, SharedString};

use crate::settings::preferences::{AppSettings, apply_theme};

/// The light theme's name in the registry.
pub const LIGHT_NAME: &str = "OxTuner Light";
/// The dark theme's name in the registry.
pub const DARK_NAME: &str = "OxTuner Dark";
/// The hardcoded theme set, compiled into the binary.
const BUNDLED: &str = include_str!("../themes/oxtuner.json");

/// Installs the hardcoded `OxTuner` pair as the live light and dark themes.
///
/// The bundled set loads synchronously so the first frame already uses it.
pub fn register(cx: &mut App) {
    install(cx);
}

/// Loads the `OxTuner` pair into the active theme and applies the preference.
///
/// The registry keeps every loaded theme; `apply_config` selects which two
/// are the live light and dark ones. Afterwards the stored System / Light /
/// Dark preference decides the mode, exactly as before — only the colors
/// behind the modes changed.
fn install(cx: &mut App) {
    if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(BUNDLED) {
        // The set is compiled in, so a parse failure is a build bug. The guard
        // tests parse it too; fall back to the kit themes instead of making a
        // wrong palette a launch crash.
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
    let (Some(light), Some(dark)) = (light, dark) else {
        tracing::error!("OxTuner themes missing from the registry");
        return;
    };

    Theme::update(cx, |theme| {
        theme.apply_config(&light);
        theme.apply_config(&dark);
    });

    apply_theme(cx.global::<AppSettings>().theme(), cx);
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "the theme tests may unwrap; a failed read is a failed test"
    )]

    use gpui_kit::component::{Theme, ThemeMode, ThemeSet, try_parse_color};
    use serde_json::Value;

    use super::*;
    use crate::settings::preferences::Settings;

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

    /// A token the crate knows but the bundle omits silently takes the crate
    /// default, which is how a theme drifts after an upgrade.
    #[test]
    fn every_theme_token_is_defined_in_the_bundled_set() {
        use gpui_kit::component::theme::ThemeConfigColors;

        // The crate's own token set, not this theme's keys: a token added by an
        // upgrade is missing here even though the parsed theme has no gap.
        let known = serde_json::to_value(ThemeConfigColors::default())
            .expect("the crate's tokens must serialize");
        let known = known
            .as_object()
            .expect("the crate's tokens must be an object");
        assert!(
            known.len() >= 140,
            "the crate must serialize its full token set, found {}",
            known.len()
        );

        let raw: serde_json::Value =
            serde_json::from_str(BUNDLED).expect("bundled theme set must parse");
        for theme in raw["themes"].as_array().expect("themes must be an array") {
            let name = theme["name"].as_str().expect("theme name");
            let colors = theme["colors"]
                .as_object()
                .expect("theme colors must be an object");

            let missing: Vec<&String> = known
                .keys()
                .filter(|key| !colors.contains_key(*key))
                .collect();

            assert!(
                missing.is_empty(),
                "{name} is missing theme tokens the crate knows: {missing:?}"
            );
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
    fn button_states_keep_their_label_readable() {
        let set: Value = serde_json::from_str(BUNDLED).expect("bundled theme set must parse");
        let families = [
            ("button", "button.foreground"),
            ("button.primary", "button.primary.foreground"),
            ("button.secondary", "button.secondary.foreground"),
            ("button.danger", "button.danger.foreground"),
            ("button.warning", "button.warning.foreground"),
            ("button.success", "button.success.foreground"),
            ("button.info", "button.info.foreground"),
        ];

        for theme in set["themes"].as_array().expect("themes must be an array") {
            let name = theme["name"].as_str().expect("theme name");
            let colors = theme["colors"]
                .as_object()
                .expect("theme colors must be an object");

            // A translucent state colour is measured over the surface the
            // button sits on, not as its raw channels: `#00000012` is a faint
            // wash over the window, not black.
            let surface = colors
                .get("background")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name} needs background"));

            for (prefix, foreground_key) in families {
                let foreground = colors
                    .get(foreground_key)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name} needs {foreground_key}"));

                for state in ["background", "hover.background", "active.background"] {
                    let key = format!("{prefix}.{state}");
                    let background = colors
                        .get(&key)
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| panic!("{name} needs {key}"));
                    let ratio = contrast_ratio(background, foreground, surface);

                    // Owner decision: bundled primary labels stay gray even
                    // though that lands below WCAG AA. The floor pins the
                    // shipped ratios instead of blessing any future drop.
                    assert!(
                        ratio >= 2.5,
                        "{name} has {ratio:.2}:1 for {key} on {foreground_key}"
                    );
                }
            }
        }
    }

    #[test]
    fn button_states_are_distinct() {
        let set: Value = serde_json::from_str(BUNDLED).expect("bundled theme set must parse");
        let families = [
            "button",
            "button.primary",
            "button.secondary",
            "button.danger",
            "button.warning",
            "button.success",
            "button.info",
        ];

        for theme in set["themes"].as_array().expect("themes must be an array") {
            let name = theme["name"].as_str().expect("theme name");
            let colors = theme["colors"]
                .as_object()
                .expect("theme colors must be an object");

            for prefix in families {
                let base = colors
                    .get(&format!("{prefix}.background"))
                    .and_then(Value::as_str)
                    .unwrap();
                let hover = colors
                    .get(&format!("{prefix}.hover.background"))
                    .and_then(Value::as_str)
                    .unwrap();
                let active = colors
                    .get(&format!("{prefix}.active.background"))
                    .and_then(Value::as_str)
                    .unwrap();

                assert_ne!(base, hover, "{name} has no hover step for {prefix}");
                assert_ne!(base, active, "{name} has no active step for {prefix}");
                assert_ne!(hover, active, "{name} reuses hover as active for {prefix}");
            }
        }
    }

    /// Contrast between two colors as they end up painted over `surface`.
    fn contrast_ratio(first: &str, second: &str, surface: &str) -> f64 {
        let first = relative_luminance(over(first, surface));
        let second = relative_luminance(over(second, surface));
        let (lighter, darker) = if first > second {
            (first, second)
        } else {
            (second, first)
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// A hex color's channels in 0..=1, alpha defaulting to opaque.
    fn channels(color: &str) -> [f64; 4] {
        let hex = color.strip_prefix('#').expect("a hex color");
        let channel = |offset: usize| {
            f64::from(u8::from_str_radix(&hex[offset..offset + 2], 16).expect("two hex digits"))
                / 255.0
        };
        let alpha = if hex.len() == 8 { channel(6) } else { 1.0 };

        [channel(0), channel(2), channel(4), alpha]
    }

    /// Paints `color` over an opaque `surface`, the way the window does.
    fn over(color: &str, surface: &str) -> [f64; 3] {
        let [r, g, b, alpha] = channels(color);
        let [sr, sg, sb, _] = channels(surface);

        [
            r * alpha + sr * (1. - alpha),
            g * alpha + sg * (1. - alpha),
            b * alpha + sb * (1. - alpha),
        ]
    }

    fn relative_luminance(color: [f64; 3]) -> f64 {
        let channels = color.map(|value| {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        });

        0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2]
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

    /// The full path through the registry: the pair is installed as the live
    /// light and dark themes, and switching modes lands on the bundled colors
    /// from the bundled file rather than the kit defaults.

    #[gpui_kit::test]
    fn install_selects_the_oxtuner_pair_and_applies_its_colors(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::init);

        cx.update(|cx| {
            cx.set_global(AppSettings::with(Settings::default()));
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

        for (mode, background, foreground, button, primary) in [
            (ThemeMode::Dark, "#121314", "#bfbfbf", "#202122", "#297AA0"),
            (ThemeMode::Light, "#FFFFFF", "#202020", "#F2F3F4", "#0069CC"),
        ] {
            cx.update(|cx| {
                Theme::change(mode, None, cx);
                let theme = Theme::global(cx);
                let expected_background =
                    try_parse_color(background).expect("bundled background must parse");
                let expected_foreground =
                    try_parse_color(foreground).expect("bundled foreground must parse");
                let expected_button = try_parse_color(button).expect("bundled button must parse");
                let expected_primary =
                    try_parse_color(primary).expect("bundled primary must parse");
                assert_eq!(theme.background, expected_background);
                assert_eq!(theme.foreground, expected_foreground);
                assert_eq!(theme.button, expected_button);
                assert_eq!(theme.button_primary, expected_primary);
            });
        }
    }
}
