//! `oxtuner` — binary entry point.
//!
//! The start-up sequence lives here, in `main`, the way an application's
//! bootstrap belongs: read the documents, start logging, pick the language,
//! then open the window. Everything the app registers after that is
//! `oxtuner_app::init` and the feature modules.
//!
//! On Windows a release build is a GUI program, so opening it should not also
//! bring up a console window. Debug builds keep theirs for the logs.
//!
//! `--sim` connects the simulator as soon as a project or definition is open.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use oxtuner_app::{
    app, core, init,
    platform::{logging, paths::AppPaths},
    project,
    settings::preferences::AppSettings,
    shell,
    state::session::AppState,
    theme,
};

fn main() {
    // Read before anything is built: the settings choose the language every
    // text is in, and the session decides where the window opens.
    let auto_sim = std::env::args().any(|argument| argument == "--sim");
    let paths = AppPaths::discover();
    let settings = AppSettings::load(&paths);
    let session = AppState::load(&paths);

    // The bundled OxTuner themes are seeded next to the settings file before
    // anything reads them; the watcher installed later picks up user edits.
    theme::seed(&paths);

    // Keep the writer guard alive for the process; dropping it stops flushing
    // the last records.
    let _log_guard = logging::init(&paths, settings.log_level());

    // The locale is global state outside GPUI, so select it before any window
    // or component renders. Only English ships today; the key is persisted so a
    // new language is a locale file plus a dropdown option.
    oxtuner_app::i18n::set_locale(settings.language());

    let app = gpui_kit::application().with_assets(gpui_kit::assets::Assets);

    // macOS: closing the window hides the app (see
    // `app::window::install_close_behavior`), so the Dock icon has to be able to
    // bring it back. Without this the app stays running with no window and no
    // way to reach one. Registered before `run`, because the platform delivers
    // the reopen while the loop is running.
    app.on_reopen(|cx| {
        if let Some(handle) = app::window::main(cx) {
            handle
                .update(cx, |_, window, cx| app::window::bring_forward(window, cx))
                .ok();
        }
    });

    app.run(move |cx| {
        init(cx);

        // Globals must exist before the first render: the shell and the settings
        // window both read them.
        cx.set_global(settings);
        cx.set_global(session);

        // Install the OxTuner light/dark pair, then follow the stored
        // preference: choosing Light or Dark in Settings has to survive a
        // restart, and System follows the OS from here on.
        theme::register(&paths, cx);

        // The headless core: one thread that owns the open project (definition
        // and tune) and the ECU session. Started before the first window so a
        // view can never render core state that has nowhere to come from.
        // `--sim` connects to the simulator as soon as a definition is open,
        // which is the hardware-free development loop.
        core::start(cx, auto_sim);

        // The core is running, so the project the last session left can be
        // reopened. It reads on the core thread; the window opens meanwhile.
        project::reopen_last(cx);

        shell::open_main_window(cx);

        cx.activate(true);
    });
}
