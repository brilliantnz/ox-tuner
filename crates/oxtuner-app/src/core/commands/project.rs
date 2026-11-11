//! Definition and project lifecycle commands.

use std::path::Path;

use gpui_kit::{App, PathPromptOptions};
use oxtuner_core::FailureCode;
use rust_i18n::t;

use super::{handle_or_report, report_failure, send};
use crate::core::CoreService;

/// Asks the core to open a project folder.
///
/// The read and parse happen on the core thread; the reply is
/// [`Event::ProjectOpened`] or [`Event::Failed`]. The pending state is set
/// before the command is sent so the window can draw the opening splash on the
/// next frame, and cleared on every outcome — including a queue that refuses
/// the command, which produces no event at all.
pub(crate) fn open_project(path: &Path, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ProjectRejected) else {
        return;
    };

    cx.global_mut::<CoreService>()
        .status
        .begin_pending_project(path);

    match handle.open_project(path) {
        Ok(op) => {
            cx.global_mut::<CoreService>()
                .status
                .pending_project_sent(op);
            cx.refresh_windows();
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the core refused the project");
            cx.global_mut::<CoreService>()
                .status
                .cancel_pending_project();
            report_failure(cx, FailureCode::ProjectRejected, error.to_string());
        }
    }
}

/// Re-checks that the open project's folder still exists.
///
/// Called at lifecycle boundaries — window activation, opening the Projects
/// dialog, and a successful open — never from a render function. A filesystem
/// `stat` per frame is what this replaces; the result is cached in
/// [`CoreStatus::project_missing`] for rendering.
pub(crate) fn refresh_project_availability(cx: &mut App) {
    let path = cx
        .global::<CoreService>()
        .status
        .project
        .as_ref()
        .map(|project| project.root.clone());

    let missing = path.is_some_and(|path| !crate::project::is_available(&path));
    let changed = {
        let status = &mut cx.global_mut::<CoreService>().status;
        let changed = status.project_missing != missing;
        status.project_missing = missing;
        changed
    };

    if changed {
        cx.refresh_windows();
    }
}

/// Asks the core to write the open project's tune file.
pub(crate) fn save_tune(cx: &mut App) {
    send(cx, FailureCode::TuneSaveFailed, "save-tune", |handle| {
        handle.save_tune()
    });
}
/// Closes the open project (and any standalone definition).
pub(crate) fn close_project(cx: &mut App) {
    send(
        cx,
        FailureCode::TableUnavailable,
        "close-project",
        |handle| handle.unload_definition(),
    );
}
/// Asks the OS for an ECU definition file, reads it, and hands it to the core.
pub(crate) fn choose_definition(cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("app.menu.open_definition").to_string().into()),
    });

    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| load(path, cx));
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "the definition picker failed");
            cx.update(|cx| {
                report_failure(cx, FailureCode::DefinitionRejected, error.to_string());
            });
        }
        Err(error) => tracing::warn!(%error, "the definition picker channel closed"),
    })
    .detach();
}

/// Reads `path` and loads it in the core.
///
/// The read happens here, on the UI thread, because it is user-initiated and one
/// shot; the parsing it feeds is already off this thread.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the file dialog hands the chosen path over to this one-shot load"
)]
fn load(path: std::path::PathBuf, cx: &mut App) {
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_owned);

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read the definition");
            report_failure(cx, FailureCode::DefinitionRejected, error.to_string());
            return;
        }
    };

    let Some(handle) = handle_or_report(cx, FailureCode::DefinitionRejected) else {
        return;
    };
    if let Err(error) = handle.load_definition(label, bytes) {
        tracing::warn!(%error, "the core refused the definition");
        report_failure(cx, FailureCode::DefinitionRejected, error.to_string());
    }
}
