//! The application menu.
//!
//! One definition, two presentations: macOS draws it in the global menu bar
//! (`cx.set_menus`), while Windows and Linux have no native menu bar in GPUI, so
//! the same items are also handed to [`AppMenuBar`], which `AppLayout` renders inside
//! the window.
//!
//! Keep this to commands that exist today. Menu entries appear before their
//! feature does otherwise, and a disabled or inert item is worse than its
//! absence (design guides: every action names its object, state, and result).

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::component::{ActiveTheme as _, GlobalState, menu::AppMenuBar};
use gpui_kit::{
    App, Entity, Menu, MenuItem, OsAction, ParentElement as _, Styled as _, SystemMenuType, Window,
    div,
};
use rust_i18n::t;

use super::actions::{
    About, AdoptEcuTune, BurnToFlash, CloseProject, ConnectEcu, DisconnectEcu, EnableWrites, Hide,
    HideOthers, ImportTune, ManageProjects, OpenDefinition, OpenHomepage, OpenProject,
    OpenSnapshots, Quit, RevertTune, SaveSessionTrace, SaveTune, Settings, ShowAll, ShowCharts,
    ShowDisclaimer, ShowTune, ToggleBottomPanel, TogglePrimarySidebar, ToggleSecondarySidebar,
    WriteJournal, WriteToRam,
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
    menu_bar.update(cx, AppMenuBar::reload);
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
            MenuItem::action(t!("app.menu.show_tune"), ShowTune),
            MenuItem::action(t!("app.menu.show_charts"), ShowCharts),
            MenuItem::separator(),
            MenuItem::action(t!("app.menu.toggle_primary_sidebar"), TogglePrimarySidebar),
            MenuItem::action(
                t!("app.menu.toggle_secondary_sidebar"),
                ToggleSecondarySidebar,
            ),
            MenuItem::action(t!("app.menu.toggle_bottom_panel"), ToggleBottomPanel),
        ]),
        // The ECU link has its own menu: connecting, testing and disconnecting
        // are a different job from file handling.
        Menu::new(t!("app.menu.connection")).items(connection_items()),
        Menu::new(t!("app.menu.help")).items([
            MenuItem::action(t!("app.menu.safety_disclaimer"), ShowDisclaimer),
            MenuItem::separator(),
            MenuItem::action(t!("app.menu.github"), OpenHomepage),
        ]),
    ]
}

/// The application menu.
///
/// macOS expects the standard application commands here, and the ones that
/// name the product spell it from [`APP_NAME`] so the label cannot drift from
/// the window title. Windows and Linux have no application menu of their own,
/// so Settings moves to File there and the macOS-only items are left out.
fn app_items() -> Vec<MenuItem> {
    let mut items = vec![MenuItem::action(
        t!("app.menu.about_app", app = APP_NAME),
        About,
    )];

    if cfg!(target_os = "macos") {
        items.push(MenuItem::separator());
        items.push(MenuItem::action(t!("app.menu.settings"), Settings));
        items.push(MenuItem::separator());
        // The OS fills this submenu with the services other applications
        // register, so it has no items of ours to list.
        items.push(MenuItem::os_submenu(
            t!("app.menu.services"),
            SystemMenuType::Services,
        ));
        items.push(MenuItem::separator());
        items.push(MenuItem::action(
            t!("app.menu.hide_app", app = APP_NAME),
            Hide,
        ));
        items.push(MenuItem::action(t!("app.menu.hide_others"), HideOthers));
        items.push(MenuItem::action(t!("app.menu.show_all"), ShowAll));
    }

    items.push(MenuItem::separator());
    items.push(MenuItem::action(
        t!("app.menu.quit_app", app = APP_NAME),
        Quit,
    ));
    items
}

/// The File menu.
///
/// macOS already has Settings in the application menu, Windows and Linux have
/// no application menu of their own, so Settings belongs under File there.
fn file_items() -> Vec<MenuItem> {
    let mut items = vec![
        MenuItem::action(t!("app.menu.manage_projects"), ManageProjects),
        MenuItem::action(t!("app.menu.open_project_folder"), OpenProject),
        MenuItem::action(t!("app.menu.open_definition"), OpenDefinition),
        MenuItem::separator(),
        MenuItem::action(t!("app.menu.save_tune"), SaveTune),
        MenuItem::action(t!("app.menu.close_project"), CloseProject),
    ];
    if !cfg!(target_os = "macos") {
        items.push(MenuItem::separator());
        items.push(MenuItem::action(t!("app.menu.settings"), Settings));
    }
    items
}

/// The Connection menu: the ECU link, separate from file handling.
fn connection_items() -> Vec<MenuItem> {
    vec![
        MenuItem::action(t!("app.menu.connect"), ConnectEcu),
        MenuItem::action(t!("app.menu.disconnect"), DisconnectEcu),
        MenuItem::separator(),
        MenuItem::action(t!("app.menu.save_session_trace"), SaveSessionTrace),
        MenuItem::separator(),
        MenuItem::action(t!("app.menu.enable_writes"), EnableWrites),
        MenuItem::action(t!("app.menu.write_to_ram"), WriteToRam),
        MenuItem::action(t!("app.menu.burn_to_flash"), BurnToFlash),
        MenuItem::action(t!("app.menu.adopt_ecu_tune"), AdoptEcuTune),
        MenuItem::action(t!("app.menu.revert_tune"), RevertTune),
        MenuItem::separator(),
        MenuItem::action(t!("app.menu.import_tune"), ImportTune),
        MenuItem::action(t!("app.menu.write_journal"), WriteJournal),
        MenuItem::action(t!("app.menu.snapshots"), OpenSnapshots),
    ]
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
    super::dialog::open(window, cx, move |dialog, _, _| {
        dialog.title(APP_NAME).child(t!("app.about.line")).child(
            div()
                .text_sm()
                .text_color(muted)
                .child(t!("app.about.version", version = env!("CARGO_PKG_VERSION"))),
        )
    });
}
