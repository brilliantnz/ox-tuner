//! The Settings window.
//!
//! A utility window (one focused task, short action path) rather than a dialog.
//! settings grow beyond a single decision. The page itself is gpui-kit's
//! [`Settings`] component — its sidebar, search and grouped pages are the
//! standard macOS-like settings layout.
//!
//! The window's title bar is our own [`crate::layout::titlebar::UtilityBar`], the
//! same geometry and drag behaviour as the main window. The gpui-kit `TitleBar`
//! component would draw its own gradient strip and lead padding, and this app
//! already owns its window chrome.
//!
//! Only settings that work today appear here. The theme is read from and
//! written to `settings.json`, so the choice survives a restart, further
//! sections arrive with the features they configure.

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::Button;
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Global, IntoElement, Keystroke,
    ParentElement as _, Pixels, Render, SharedString, Styled as _, WeakEntity, Window,
    WindowBounds, WindowKind, WindowOptions, div, px, size,
};

pub mod preferences;

use self::preferences::{AppSettings, ThemePreference};
use crate::{
    app::shortcuts::{SHORTCUTS, Shortcut},
    constants::APP_NAME,
    i18n::t,
};

/// Width of the settings sidebar.
///
/// Fixed, and shared with [`UtilityBar`]: the window's drag strip and the
/// component's own sidebar have to line up exactly for the sidebar to read as
/// one column running to the top edge of the window.
pub(crate) const SIDEBAR_WIDTH: Pixels = px(190.);

/// The open Settings window, so invoking `Settings` again focuses the existing
/// window instead of stacking copies.
///
/// Liveness comes from the view, not the window handle: the window's root holds
/// the view, so a successful upgrade means the window is still open, while
/// asking the handle can fail during the window's own creation. That failure is
/// what made every `Settings` command open another window.
#[derive(Clone)]
struct OpenWindowState {
    handle: AnyWindowHandle,
    view: WeakEntity<SettingsWindow>,
}

#[derive(Default)]
struct OpenWindow(Option<OpenWindowState>);

impl Global for OpenWindow {}

/// Opens the Settings window, or brings the existing one to the front.
pub(crate) fn open(cx: &mut App) {
    if let Some(open) = cx
        .try_global::<OpenWindow>()
        .and_then(|open| open.0.clone())
        && open.view.upgrade().is_some()
    {
        if let Err(error) = cx.update_window(open.handle, |_, window, _| window.activate_window()) {
            // The view is alive, so the window exists; report without opening a
            // duplicate, which would leave two live copies of the same page.
            tracing::debug!(%error, "could not activate the settings window");
        }
        return;
    }

    let bounds = Bounds::centered(None, size(px(720.), px(640.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // titlebar: Some(titlebar_options(t!("app.settings.window_title"))),
        // app_owns_titlebar_drag: true,
        // Linux only: without this the compositor adds a second, server-side
        // caption above our own. Windows and macOS ignore it.
        // window_decorations: Some(WindowDecorations::Client),
        kind: WindowKind::Floating,
        is_resizable: true,
        window_min_size: Some(size(px(620.), px(460.))),
        app_id: Some(APP_NAME.into()),
        ..Default::default()
    };

    let (handle, view) = gpui_kit::open_window(options, cx, |_, cx| cx.new(SettingsWindow::new))
        .expect("failed to open the settings window");
    cx.set_global(OpenWindow(Some(OpenWindowState {
        handle,
        view: view.downgrade(),
    })));
}

/// The settings page.
///
/// Stateless on purpose: every field reads its value from the live
/// [`AppSettings`] global, so the controls cannot drift from what the windows
/// actually use (including an OS appearance change while the page is open).
pub(crate) struct SettingsWindow;

impl SettingsWindow {
    fn new(_: &mut Context<'_, Self>) -> Self {
        Self
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Two-region bar: the sidebar column keeps its own drag strip so it
            // runs to the window's top edge, exactly like the main window.
            // .child(UtilityBar::new(
            //     t!("app.settings.window_title"),
            //     SIDEBAR_WIDTH,
            // ))
            .child(
                div().flex_1().min_h_0().child(
                    Settings::new("preferences")
                        // The bar's strip and the component's sidebar share this
                        // width, and the range is pinned so a drag cannot
                        // misalign them.
                        .sidebar_width(SIDEBAR_WIDTH)
                        .sidebar_size_range(SIDEBAR_WIDTH..px(250.))
                        .with_group_variant(GroupBoxVariant::Outline)
                        .pages(pages()),
                ),
            )
    }
}

fn pages() -> Vec<SettingPage> {
    vec![
        appearance_page(),
        editor_page(),
        project_page(),
        shortcuts_page(),
        safety_page(),
        logging_page(),
        about_page(),
    ]
}

fn appearance_page() -> SettingPage {
    SettingPage::new(t!("app.settings.page.appearance"))
        .icon(IconName::Settings2)
        .group(
            SettingGroup::new()
                .title(t!("app.settings.page.appearance"))
                .item(
                    SettingItem::new(
                        t!("app.settings.item.mode"),
                        SettingField::dropdown(
                            theme_options(),
                            |cx| cx.global::<AppSettings>().settings().ui.theme.key().into(),
                            |value, cx| {
                                AppSettings::set_theme(
                                    cx,
                                    ThemePreference::from_key(value.as_ref()),
                                );
                            },
                        ),
                    )
                    .description(t!("app.settings.theme_description").to_string())
                    .keywords(["theme", "appearance", "dark", "light"]),
                )
                .item(
                    SettingItem::new(
                        t!("app.settings.item.language"),
                        SettingField::dropdown(
                            language_options(),
                            |cx| {
                                cx.global::<AppSettings>()
                                    .settings()
                                    .ui
                                    .language
                                    .clone()
                                    .into()
                            },
                            |value, cx| AppSettings::set_language(cx, value.as_ref()),
                        ),
                    )
                    .keywords(["language", "locale", "translation"]),
                ),
        )
}

/// Dropdown options as `(value, label)`.
///
/// gpui-kit's dropdown stores the first element of the pair and shows the
/// second. Building them the other way round made every pick read back as an
/// unknown key, so the theme silently stayed on its default.
fn theme_options() -> Vec<(SharedString, SharedString)> {
    [
        ThemePreference::System,
        ThemePreference::Light,
        ThemePreference::Dark,
    ]
    .into_iter()
    .map(|preference| (preference.key().into(), preference.label().into()))
    .collect()
}

/// Languages the build ships, as `(code, label)`.
///
/// A language's label is written in that language and stays the same across
/// locales, so the label comes from the locale file's `app.language.<code>`
/// key rather than the active locale.
fn language_options() -> Vec<(SharedString, SharedString)> {
    vec![(preferences::DEFAULT_LANGUAGE.into(), t!("app.language.en"))]
}

/// The table editor's display preferences.
fn editor_page() -> SettingPage {
    SettingPage::new(t!("app.settings.page.editor"))
        .icon(IconName::Columns3Cog)
        .group(
            SettingGroup::new()
                .title(t!("app.settings.group.table"))
                .item(
                    SettingItem::new(
                        t!("app.settings.item.header_wash_for_ranges"),
                        SettingField::switch(
                            |cx| {
                                cx.global::<AppSettings>()
                                    .settings()
                                    .ui
                                    .table_header_wash_for_ranges
                            },
                            |value, cx| {
                                AppSettings::set_table_header_wash_for_ranges(cx, value);
                            },
                        ),
                    )
                    .description(t!("app.settings.header_wash_for_ranges_description").to_string())
                    .keywords([
                        "table",
                        "header",
                        "bin",
                        "range",
                        "selection",
                        "highlight",
                    ]),
                ),
        )
}

fn project_page() -> SettingPage {
    SettingPage::new(t!("app.settings.page.project"))
        .icon(IconName::Folder)
        .group(
            SettingGroup::new()
                .title(t!("app.settings.group.startup"))
                .item(
                    SettingItem::new(
                        t!("app.settings.item.auto_reopen"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().settings().project.auto_reopen,
                            |value, cx| AppSettings::set_auto_reopen(cx, value),
                        ),
                    )
                    .description(t!("app.settings.auto_reopen_description").to_string())
                    .keywords(["project", "reopen", "startup", "recent"]),
                ),
        )
}

/// Keyboard shortcuts, from the static table that also binds them.
///
/// Reading [`SHORTCUTS`] rather than the live keymap means the page cannot show
/// a binding the app does not use: the table *is* what `app::shortcuts` binds,
/// and a unit test checks that no two entries share a key.
fn shortcuts_page() -> SettingPage {
    let items: Vec<SettingItem> = SHORTCUTS.iter().map(shortcut_item).collect();

    SettingPage::new(t!("app.settings.page.shortcuts"))
        .icon(IconName::SquareTerminal)
        .group(
            SettingGroup::new()
                .title(t!("app.settings.page.shortcuts"))
                .items(items),
        )
}

/// One read-only shortcut row: the command's name and its keys.
fn shortcut_item(shortcut: &'static Shortcut) -> SettingItem {
    SettingItem::new(
        shortcut.title(),
        SettingField::render(move |_, _, _| match Keystroke::parse(shortcut.keys()) {
            Ok(keystroke) => h_flex().child(Kbd::new(keystroke)).into_any_element(),
            // Unbound: say so rather than render an empty row slot.
            Err(_) => div().child("—").into_any_element(),
        }),
    )
}

fn safety_page() -> SettingPage {
    SettingPage::new(t!("app.settings.page.safety"))
        .icon(IconName::CircleAlert)
        .group(
            SettingGroup::new()
                .title(t!("app.settings.group.guardrails"))
                .item(
                    SettingItem::render(|options, _, cx| {
                        let accepted = cx
                            .global::<AppSettings>()
                            .settings()
                            .safety
                            .disclaimer_accepted;
                        let muted = cx.theme().muted_foreground;

                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(t!("app.settings.item.safety_disclaimer"))
                                    .child(div().text_sm().text_color(muted).child(if accepted {
                                        t!("app.settings.disclaimer_accepted")
                                    } else {
                                        t!("app.settings.disclaimer_not_accepted")
                                    })),
                            )
                            .child(
                                Button::new("review-disclaimer")
                                    .label(t!("app.settings.review"))
                                    .with_size(options.size())
                                    .on_click(|_, window, cx| crate::disclaimer::show(window, cx)),
                            )
                            .into_any_element()
                    })
                    .keywords(["disclaimer", "safety", "warning"]),
                ),
        )
}

fn logging_page() -> SettingPage {
    SettingPage::new(t!("app.settings.page.logging"))
        .icon(IconName::SquareText)
        .group(
            SettingGroup::new()
                .title(t!("app.settings.group.diagnostics"))
                .item(
                    SettingItem::new(
                        t!("app.settings.item.verbose_logging"),
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().settings().logging.verbose,
                            |value, cx| AppSettings::set_log_verbose(cx, value),
                        ),
                    )
                    .description(t!("app.settings.verbose_logging_description").to_string())
                    .keywords([
                        "log",
                        "logging",
                        "debug",
                        "verbose",
                        "diagnostics",
                        "tracing",
                    ]),
                )
                .item(
                    SettingItem::new(
                        t!("app.settings.item.log_file"),
                        SettingField::render(|_, _, cx| {
                            let text = match crate::platform::logging::handle(cx) {
                                Some(handle) => handle.path().map_or_else(
                                    || t!("app.settings.log_file_console_only").to_string(),
                                    |path| path.display().to_string(),
                                ),
                                None => t!("app.settings.log_file_unavailable").to_string(),
                            };

                            div().text_sm().child(text)
                        }),
                    )
                    .keywords(["log", "file", "path"]),
                ),
        )
}

fn about_page() -> SettingPage {
    SettingPage::new(t!("app.settings.page.about"))
        .icon(IconName::Info)
        .group(
            SettingGroup::new()
                .title(t!("app.settings.group.application"))
                .items(vec![
                    SettingItem::new(
                        t!("app.settings.item.version"),
                        SettingField::render(|_, _, _| {
                            div().child(concat!("v", env!("CARGO_PKG_VERSION")))
                        }),
                    ),
                    SettingItem::new(
                        t!("app.settings.item.license"),
                        SettingField::render(|_, _, _| div().child("GPL-3.0-or-later")),
                    ),
                    SettingItem::new(
                        t!("app.settings.item.source_code"),
                        SettingField::render(|options, _, _| {
                            Button::new("open-homepage")
                                .label(t!("app.settings.github_label"))
                                .with_size(options.size())
                                .on_click(|_, _, cx| {
                                    cx.open_url("https://github.com/brilliantnz/ox-tuner");
                                })
                        }),
                    ),
                ]),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dropdown stores the first element of each pair and displays the
    /// second. Passing `(label, key)` made every selection read back as an
    /// unknown key, so choosing Dark applied the default again and nothing
    /// visible changed.
    #[test]
    fn theme_options_carry_the_key_as_value_and_the_label_as_text() {
        let options = theme_options();
        assert_eq!(options.len(), 3);

        for (value, label) in options {
            let preference = ThemePreference::from_key(value.as_ref());
            assert_eq!(
                preference.key(),
                value.as_ref(),
                "the stored value must be a key that round-trips"
            );
            assert_eq!(preference.label(), label.as_ref());
        }
    }

    /// Two entries must not share a value, or one of them can never be selected.
    #[test]
    fn theme_option_values_are_unique() {
        let mut values: Vec<_> = theme_options()
            .into_iter()
            .map(|(value, _)| value.to_string())
            .collect();
        values.sort();
        values.dedup();

        assert_eq!(values.len(), 3);
    }
}
