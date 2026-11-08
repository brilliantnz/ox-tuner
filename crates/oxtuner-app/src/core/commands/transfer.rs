//! Import, snapshot and journal file commands.

use std::path::Path;

use gpui_kit::{App, PathPromptOptions};
use oxtuner_core::FailureCode;
use rust_i18n::t;

use super::{handle_or_report, report_failure, send};
use crate::core::CoreService;

/// Asks the core to import a `.msq` into the working image.
pub(crate) fn import_tune(path: &Path, consented: bool, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ImportRejected) else {
        return;
    };
    cx.global_mut::<CoreService>().pending_import = Some(path.to_owned());
    if let Err(error) = handle.import_tune(path, consented) {
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
                cx.update(|cx| import_tune(&path, false, cx));
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the import picker failed"),
        Err(error) => tracing::warn!(%error, "the import picker channel closed"),
    })
    .detach();
}
