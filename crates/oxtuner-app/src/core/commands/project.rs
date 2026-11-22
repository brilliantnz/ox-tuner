//! Definition and project lifecycle commands.

use std::path::Path;
use std::time::Duration;

use gpui_kit::{App, PathPromptOptions};
use oxtuner_core::{CreateRequest, FailureCode};
use rust_i18n::t;

use super::{handle_or_report, report_failure, send};
use crate::core::CoreService;

/// How long the opening splash waits for the core's answer.
///
/// A real open parses a definition, so this is generous. The point is that an
/// answer lost by the event stream cannot leave the splash up forever.
const PROJECT_OPEN_TIMEOUT: Duration = Duration::from_secs(15);

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
        Ok(op) => track_pending_project(op, path, cx),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the core refused the project");
            cx.global_mut::<CoreService>()
                .status
                .cancel_pending_project();
            report_failure(cx, FailureCode::ProjectRejected, error.to_string());
        }
    }
}

/// Asks the core to read a definition without opening or creating anything.
///
/// The new-project wizard calls this when the user picks the `.ini`; the answer
/// is `DefinitionInspected` or `Failed`, and the configuration step renders the
/// setting groups it carries.
pub(crate) fn inspect_definition(path: &Path, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::DefinitionRejected) else {
        return;
    };

    if let Err(error) = handle.inspect_definition(path.to_path_buf()) {
        tracing::warn!(%error, path = %path.display(), "the core refused the definition");
        report_failure(cx, FailureCode::DefinitionRejected, error.to_string());
    }
}

/// Asks the core to create a project folder and open it.
///
/// Creation writes the scaffold on the core thread, so the wizard shows the
/// same splash an open does, worded for creating. The answer arrives as
/// `ProjectOpened` or `Failed`, exactly like an open.
pub(crate) fn create_project(request: CreateRequest, cx: &mut App) {
    create(request, None, cx);
}

/// Asks the core to create a project from the controller's tune.
///
/// The core verifies the signature on `port`, reads every page, seeds the
/// scaffold with that image and writes the project. The reply is the same
/// `ProjectOpened` or `Failed`; a failed read never falls back to defaults.
pub(crate) fn create_project_from_ecu(
    request: CreateRequest,
    port: String,
    baud: u32,
    cx: &mut App,
) {
    create(request, Some((port, baud)), cx);
}

/// Sends a create, from the definition's defaults or from a controller read.
fn create(request: CreateRequest, from_ecu: Option<(String, u32)>, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ProjectRejected) else {
        return;
    };

    let root = request.root();
    cx.global_mut::<CoreService>()
        .status
        .begin_pending_creation(&root);

    let sent = match from_ecu {
        Some((port, baud)) => {
            handle.create_project_from_ecu(request, oxtuner_core::PortChoice::serial(port, baud))
        }
        None => handle.create_project(request),
    };
    match sent {
        Ok(op) => track_pending_project(op, &root, cx),
        Err(error) => {
            tracing::warn!(%error, path = %root.display(), "the core refused the create");
            cx.global_mut::<CoreService>()
                .status
                .cancel_pending_project();
            report_failure(cx, FailureCode::ProjectRejected, error.to_string());
        }
    }
}

/// Records the op that will answer a pending project, and expires the state if
/// no answer arrives.
fn track_pending_project(op: oxtuner_core::OpId, path: &Path, cx: &mut App) {
    cx.global_mut::<CoreService>()
        .status
        .pending_project_sent(op);

    let pending_path = path.to_path_buf();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(PROJECT_OPEN_TIMEOUT).await;
        let expired = cx.update(|cx| {
            cx.global_mut::<CoreService>()
                .status
                .expire_pending_project(op)
        });
        if expired {
            tracing::warn!(
                %op,
                path = %pending_path.display(),
                "the project did not answer; the pending state was cleared"
            );
            cx.update(crate::app::window::notify_main);
        }
    })
    .detach();

    cx.refresh_windows();
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

/// Stores the open project's panel layout in its state file.
///
/// Called from the debounced layout write, so a drag never sends one command
/// per frame. The layout is a convenience next to the tune: a refused send is
/// a debug line, not a notification. Returns whether the command was accepted,
/// so the caller only records the layout as sent when it really was.
pub(crate) fn save_project_layout(cx: &mut App, layout: oxtuner_core::ProjectLayout) -> bool {
    let Some(handle) = CoreService::handle(cx) else {
        return false;
    };
    match handle.set_project_layout(layout) {
        Ok(_) => true,
        Err(error) => {
            tracing::debug!(%error, "the core refused the project layout update");

            false
        }
    }
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
                report_failure(cx, FailureCode::PickerFailed, error.to_string());
            });
        }
        Err(error) => {
            tracing::warn!(%error, "the definition picker channel closed");
            cx.update(|cx| {
                report_failure(cx, FailureCode::PickerFailed, error.to_string());
            });
        }
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
