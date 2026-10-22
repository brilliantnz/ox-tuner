//! The application menu.
//!
//! One definition, two presentations: macOS draws it in the global menu bar
//! (`cx.set_menus`), while Windows and Linux have no native menu bar in GPUI, so
//! the same items are also handed to [`AppMenuBar`], which `Shell` renders inside
//! the window.
//!
//! Keep this to commands that exist today. Menu entries appear before their
//! feature does otherwise, and a disabled or inert item is worse than its
//! absence (design guides: every action names its object, state, and result).

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::component::{ActiveTheme as _, GlobalState, WindowExt as _, menu::AppMenuBar};
use gpui_kit::{
    App, Entity, Menu, MenuItem, OsAction, ParentElement as _, Styled as _, Window, div,
};
use rust_i18n::t;

use super::actions::{
    About, OpenDefinition, OpenHomepage, OpenProject, Quit, Settings, ShowDisclaimer,
    ToggleBottomPanel, TogglePrimarySidebar, ToggleSecondarySidebar,
};
use crate::constants::APP_NAME;

/// Installs the menu bar for this process and returns the in-window bar.
///
/// Call it after the key bindings are registered: `set_menus` copies each
/// item's shortcut from the keymap at the moment it runs.
pub(crate) fn init(cx: &mut App) -> Entity<AppMenuBar> {
    let menu_bar = AppMenuBar::new(cx);
    install(cx, &menu_bar);
    menu_bar
}

fn install(cx: &mut App, menu_bar: &Entity<AppMenuBar>) {
    cx.set_menus(menus());
    // The in-window bar reads its own copy, so it stays correct on the
    // platforms where the OS keeps the menus to itself.
    GlobalState::global_mut(cx).set_app_menus(menus().into_iter().map(Menu::owned).collect());
    menu_bar.update(cx, |menu_bar, cx| menu_bar.reload(cx));
}

fn menus() -> Vec<Menu> {
    vec![
        Menu::new(APP_NAME).items(app_items()),
        Menu::new(t!("app.menu.file")).items(file_items()),
        // Standard editing commands. Nothing is editable yet, so these are
        // inert until a field has focus; the OS actions keep macOS text
        // controls working once they do.
        Menu::new(t!("app.menu.edit")).items([
            MenuItem::os_action(t!("app.menu.undo"), Undo, OsAction::Undo),
            MenuItem::os_action(t!("app.menu.redo"), Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action(t!("app.menu.cut"), Cut, OsAction::Cut),
            MenuItem::os_action(t!("app.menu.copy"), Copy, OsAction::Copy),
            MenuItem::os_action(t!("app.menu.paste"), Paste, OsAction::Paste),
        ]),
        Menu::new(t!("app.menu.selection")).items([MenuItem::os_action(
            t!("app.menu.select_all"),
            SelectAll,
            OsAction::SelectAll,
        )]),
        // The panel commands, so a panel closed by mistake can be found again
        // without knowing the shortcut. Each carries its key from the keymap.
        Menu::new(t!("app.menu.view")).items([
            MenuItem::action(t!("app.menu.toggle_primary_sidebar"), TogglePrimarySidebar),
            MenuItem::action(
                t!("app.menu.toggle_secondary_sidebar"),
                ToggleSecondarySidebar,
            ),
            MenuItem::action(t!("app.menu.toggle_bottom_panel"), ToggleBottomPanel),
        ]),
        Menu::new(t!("app.menu.help")).items([
            MenuItem::action(t!("app.menu.safety_disclaimer"), ShowDisclaimer),
            MenuItem::separator(),
            MenuItem::action(t!("app.menu.github"), OpenHomepage),
        ]),
    ]
}

/// The application menu.
///
/// macOS expects Settings and Quit here, on Windows/Linux Settings moves to
/// File so the app-name menu does not duplicate it.
fn app_items() -> Vec<MenuItem> {
    let mut items = vec![MenuItem::action(t!("app.menu.about_item"), About)];
    if cfg!(target_os = "macos") {
        items.push(MenuItem::separator());
        items.push(MenuItem::action(t!("app.menu.settings"), Settings));
    }
    items.push(MenuItem::separator());
    items.push(MenuItem::action(t!("app.menu.quit"), Quit));
    items
}

/// The File menu.
///
/// macOS already has Settings in the application menu, Windows and Linux have
/// no application menu of their own, so Settings belongs under File there.
fn file_items() -> Vec<MenuItem> {
    let mut items = vec![
        MenuItem::action(t!("app.menu.open_definition"), OpenDefinition),
        MenuItem::action(t!("app.menu.open_project_folder"), OpenProject),
    ];
    if !cfg!(target_os = "macos") {
        items.push(MenuItem::separator());
        items.push(MenuItem::action(t!("app.menu.settings"), Settings));
    }
    items
}

/// Shows the About dialog.
///
/// Runs from the app-level action handler rather than a view: a menu command
/// must work when nothing in the window holds focus, and only the app-level
/// listeners are reached then.
pub(crate) fn show_about(window: &mut Window, cx: &mut App) {
    // The builder is `'static` and may run again when the dialog layer is
    // rebuilt, so it captures values rather than borrowing the context.
    let muted = cx.theme().muted_foreground;
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title(APP_NAME).child(t!("app.about.line")).child(
            div()
                .text_sm()
                .text_color(muted)
                .child(t!("app.about.version", version = env!("CARGO_PKG_VERSION"))),
        )
    });
}
