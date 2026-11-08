//! Session log commands.

use gpui_kit::App;
use oxtuner_core::FailureCode;

use super::send;
use crate::core::CoreService;
use crate::platform::paths::AppPaths;

/// Starts a session log beside the project, or in the app's log folder.
pub(crate) fn start_log(cx: &mut App) {
    let directory = CoreService::status(cx).project.as_ref().map_or_else(
        || AppPaths::discover().logs_dir,
        |project| project.root.join(".ox-tuner").join("logs"),
    );

    send(cx, FailureCode::LogFailed, "start-log", move |handle| {
        handle.start_log(directory)
    });
}

/// Stops the session log, flushing it.
pub(crate) fn stop_log(cx: &mut App) {
    send(cx, FailureCode::LogFailed, "stop-log", |handle| {
        handle.stop_log()
    });
}
