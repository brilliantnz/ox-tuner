//! Application-level wiring: actions, the static shortcut table, the menu, and
//! the identity and geometry of the main window.
//!
//! Everything here is app-wide rather than owned by a view. The bootstrap that
//! loads settings and opens the first window lives in `main.rs`, which is where
//! an application's start-up belongs; [`crate::init`] calls [`init`] to
//! register what GPUI and the keymap need before any window exists.
//!
//! Window-dependent commands look the main window up through
//! [`window::main`] instead of capturing a handle at start-up, so a command
//! handler never holds a stale window just because it was registered early.

pub mod actions;
pub mod dialog;
pub mod menu;
pub mod shortcuts;
pub mod window;

use gpui_kit::{App, Window};

use crate::{core, disclaimer, layout, project, settings};

/// The application identifier the platform stores window state under.
pub fn app_identity() -> &'static str {
    "com.oxtuner.OxTuner"
}

/// Registers everything global to the application: gpui-kit, the keymap and the
/// app-level action handlers. Call once inside `Application::run`, before any
/// window is created.
pub fn init(cx: &mut App) {
    // gpui-kit first: it registers the components' own actions and bindings.
    gpui_kit::init(cx);
    // The notification inbox and the toast placement both need gpui-kit's
    // theme and window layers to exist.
    crate::notifications::init(cx);

    // Before ours, so a rebuild starts from gpui-kit's copy rather than an
    // empty keymap.
    shortcuts::keep_kit_bindings(cx);
    // gpui-kit's own text in the language ox-tuner's is in: the saved one,
    // which `main` selected before anything was built.
    crate::i18n::set_locale(crate::i18n::locale());
    shortcuts::apply_shortcuts(cx);

    // Window-independent commands run at app level so they work when nothing in
    // a window holds focus — which is the case for every menu command.
    cx.on_action(|_: &actions::Quit, cx| cx.quit());
    cx.on_action(|_: &actions::Hide, cx| cx.hide());
    cx.on_action(|_: &actions::HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &actions::ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &actions::Settings, cx| settings::open(cx));
    cx.on_action(|_: &actions::OpenDefinition, cx| {
        core::choose_definition(cx);
    });
    cx.on_action(|_: &actions::OpenProject, cx| {
        // The folder picker, for the tuner who knows the path.
        project::choose(cx);
    });
    cx.on_action(|_: &actions::NewProject, cx| {
        with_main_window(cx, crate::ui::new_project::show);
    });
    cx.on_action(|_: &actions::ManageProjects, cx| {
        // The Projects dialog, for everything else about a project.
        cx.defer(|cx| {
            if let Some(window) = window::main(cx) {
                window
                    .update(cx, |_, window, cx| {
                        layout::project_dialog::show(window, cx);
                    })
                    .ok();
            }
        });
    });
    cx.on_action(|_: &actions::CloseProject, cx| {
        core::close_project(cx);
    });
    cx.on_action(|_: &actions::SaveTune, cx| core::save_tune(cx));
    cx.on_action(|_: &actions::ConnectEcu, cx| core::request_connect(cx));
    cx.on_action(|_: &actions::DisconnectEcu, cx| core::disconnect(cx));
    cx.on_action(|_: &actions::SaveSessionTrace, cx| core::save_trace(cx));
    cx.on_action(|_: &actions::WriteToRam, cx| {
        with_main_window(cx, layout::write_dialogs::show_write);
    });
    cx.on_action(|_: &actions::BurnToFlash, cx| {
        with_main_window(cx, layout::write_dialogs::show_burn);
    });
    cx.on_action(|_: &actions::AdoptEcuTune, cx| core::adopt_ecu_tune(cx));
    cx.on_action(|_: &actions::ReviewTuneDifferences, cx| {
        with_main_window(cx, layout::write_dialogs::show_conflicts);
    });
    cx.on_action(|_: &actions::RevertTune, cx| {
        with_main_window(cx, layout::write_dialogs::show_revert);
    });
    cx.on_action(|_: &actions::EnableWrites, cx| {
        with_main_window(cx, layout::write_dialogs::show_enable_writes);
    });
    cx.on_action(|_: &actions::ImportTune, cx| core::choose_import(cx));
    cx.on_action(|_: &actions::WriteJournal, cx| {
        core::read_journal(cx);
        with_main_window(cx, layout::write_dialogs::show_journal);
    });
    cx.on_action(|_: &actions::OpenSnapshots, cx| {
        core::list_snapshots(cx);
        with_main_window(cx, layout::write_dialogs::show_snapshots);
    });
    cx.on_action(|_: &actions::OpenHomepage, cx| {
        cx.open_url("https://github.com/brilliantnz/ox-tuner");
    });

    // Panel visibility is session state, not view state, so the toggles only
    // need the app context.
    cx.on_action(|_: &actions::TogglePrimarySidebar, cx| {
        layout::toggle_primary_sidebar(cx);
    });
    cx.on_action(|_: &actions::ToggleSecondarySidebar, cx| {
        layout::toggle_secondary_sidebar(cx);
    });
    cx.on_action(|_: &actions::ToggleBottomPanel, cx| {
        layout::toggle_bottom_panel(cx);
    });

    // Dialogs need a window; `defer` avoids touching the dispatching window
    // from inside the action handler, where GPUI reports "window not found".
    cx.on_action(|_: &actions::About, cx| {
        cx.defer(|cx| {
            if let Some(window) = window::main(cx) {
                window
                    .update(cx, |_, window, cx| menu::show_about(window, cx))
                    .ok();
            }
        });
    });
    cx.on_action(|_: &actions::ShowDisclaimer, cx| {
        cx.defer(|cx| {
            if let Some(window) = window::main(cx) {
                window
                    .update(cx, |_, window, cx| disclaimer::show(window, cx))
                    .ok();
            }
        });
    });

    // Keyboard defocus lives on the layout root: Escape releases focus after
    // every focused control that wants the key has handled and stopped it.
    // It is not a binding, because a no-context binding is the deepest match in
    // GPUI and would preempt popups and editors.
}

/// Runs `open` against the main window, after the current action dispatch.
///
/// Deferred so a dialog that is closing first has released the window's modal
/// slot: only one dialog may be open at a time.
pub(crate) fn with_main_window(cx: &mut App, open: fn(&mut Window, &mut App)) {
    cx.defer(move |cx| {
        if let Some(window) = window::main(cx) {
            window.update(cx, |_, window, cx| open(window, cx)).ok();
        }
    });
}
