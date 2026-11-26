//! `oxtuner` — binary entry point.
//!
//! The start-up sequence lives here, in `main`, the way an application's
//! bootstrap belongs: start logging, read the documents, pick the language,
//! then open the window. Everything the app registers after that is
//! `oxtuner_app::init` and the feature modules.
//!
//! On Windows a release build is a GUI program, so opening it should not also
//! bring up a console window. Debug builds keep theirs for the logs.
//!
//! `--sim` connects the simulator as soon as a project or definition is open.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use oxtuner_app::{
    app, core, init, layout,
    platform::{logging, paths::AppPaths},
    project,
    settings::preferences::AppSettings,
    state::session::AppState,
    theme,
};

fn main() {
    // The flags and paths are read before anything is built, so the log
    // pipeline can start before the documents load.
    let auto_sim = std::env::args().any(|argument| argument == "--sim");
    let file_output = if std::env::args().any(|argument| argument == "--log-file") {
        logging::FileOutput::On
    } else {
        logging::FileOutput::Auto
    };
    let paths = AppPaths::discover();

    // The pipeline installs the subscriber, so this must happen before the
    // first record. A corrupt settings or session file is quarantined with a
    // warning, and that warning has to reach the store and the log file. The
    // stored verbose preference is applied below, once the settings are read.
    // Keep the handle alive for the process; dropping it flushes the file and
    // stops the non-blocking writer.
    let log_handle = logging::init(paths.log_dir(), file_output);

    // Read after logging: every warning about a corrupt file must be recorded.
    let settings = AppSettings::load(&paths);
    let session = AppState::load(&paths);

    // The locale is global state outside GPUI, so select it before any window
    // or component renders. Only English ships today; the key is persisted so a
    // new language is a locale file plus a dropdown option.
    oxtuner_app::i18n::set_locale(settings.language());

    let app = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);

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
        // Install the pipeline before the first view or action logs anything.
        // The global keeps the writer guard alive for the whole process.
        if let Some(handle) = log_handle {
            logging::install(cx, handle);
        }

        // The pipeline starts at the profile default; the persisted preference
        // takes effect now that the settings have been read.
        logging::set_verbose(cx, settings.log_verbose());

        init(cx);

        // Globals must exist before the first render: the main layout and the
        // settings window both read them.
        cx.set_global(settings);
        cx.set_global(session);

        // Install the OxTuner light/dark pair, then follow the stored
        // preference: choosing Light or Dark in Settings has to survive a
        // restart, and System follows the OS from here on.
        theme::register(cx);

        // One discovery for the process; every command reads this instance
        // instead of asking the platform again.
        AppPaths::install(cx, paths);

        // The projects folder is the default the New Project dialog starts in,
        // so it exists from the first launch, not after the first project.
        AppPaths::get(cx).ensure_projects_dir();

        // The headless core: one thread that owns the open project (definition
        // and tune) and the ECU session. Started before the first window so a
        // view can never render core state that has nowhere to come from.
        // `--sim` connects to the simulator as soon as a definition is open,
        // which is the hardware-free development loop.
        core::start(cx, auto_sim);

        // The core is running, so the project the last session left can be
        // reopened. It reads on the core thread; the window opens meanwhile.
        project::reopen_last(cx);

        layout::open_main_window(cx);

        cx.activate(true);
    });
}
