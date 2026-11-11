//! Import, snapshot and journal file commands.

use std::path::Path;

use gpui_kit::{App, PathPromptOptions};
use oxtuner_core::{CoreError, CoreHandle, FailureCode};
use rust_i18n::t;

use super::{handle_or_report, report_failure, send};
use crate::core::CoreService;

/// Asks the core to import a `.msq` whose signature matches the open definition.
pub(crate) fn import_tune(path: &Path, cx: &mut App) {
    send_import(cx, path, |handle| handle.import_tune(path));
}

/// Asks the core to import a `.msq` after the user approved a different
/// signature.
pub(crate) fn import_tune_with_consent(path: &Path, cx: &mut App) {
    send_import(cx, path, |handle| handle.import_tune_with_consent(path));
}

fn send_import<T>(
    cx: &mut App,
    path: &Path,
    send: impl FnOnce(CoreHandle) -> Result<T, CoreError>,
) {
    let Some(handle) = handle_or_report(cx, FailureCode::ImportRejected) else {
        return;
    };
    cx.global_mut::<CoreService>().pending_import = Some(path.to_owned());
    if let Err(error) = send(handle) {
        tracing::warn!(%error, path = %path.display(), "the core refused the import");
        cx.global_mut::<CoreService>().pending_import = None;
        report_failure(cx, FailureCode::ImportRejected, error.to_string());
    }
}

/// Restores one of the project's snapshots.
pub(crate) fn restore_snapshot(path: &Path, cx: &mut App) {
    send(
        cx,
        FailureCode::ImportRejected,
        "restore-snapshot",
        |handle| handle.restore_snapshot(path),
    );
}

/// Lists the open project's snapshots.
pub(crate) fn list_snapshots(cx: &mut App) {
    send(
        cx,
        FailureCode::SnapshotFailed,
        "list-snapshots",
        |handle| handle.list_snapshots(),
    );
}

/// Reads the open project's write journal.
pub(crate) fn read_journal(cx: &mut App) {
    send(cx, FailureCode::JournalFailed, "read-journal", |handle| {
        handle.read_journal()
    });
}

/// Asks the OS for a `.msq` and imports it.
pub(crate) fn choose_import(cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("app.import.title").to_string().into()),
    });

    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| import_tune(&path, cx));
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the import picker failed"),
        Err(error) => tracing::warn!(%error, "the import picker channel closed"),
    })
    .detach();
}
