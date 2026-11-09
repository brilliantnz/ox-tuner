//! Tune writes, burns, reverts and the derived tune status.

use gpui_kit::App;
use oxtuner_core::FailureCode;

use super::{handle_or_report, send};
use crate::settings::preferences::AppSettings;

/// Asks the core to sync the tune values that differ from ECU RAM.
pub(crate) fn write_to_ram(cx: &mut App) {
    send(cx, FailureCode::WriteRefused, "write-to-ram", |handle| {
        handle.write_to_ram(Vec::new())
    });
}

/// Asks the core to burn RAM to flash.
pub(crate) fn burn_to_flash(cx: &mut App) {
    send(cx, FailureCode::BurnRefused, "burn-to-flash", |handle| {
        handle.burn_to_flash(Vec::new())
    });
}

/// Discards the unsaved cells one document owns.
pub(crate) fn revert_target(target: &str, cx: &mut App) {
    send(
        cx,
        FailureCode::WriteRefused,
        "revert-target",
        move |handle| handle.revert_target(target.to_owned()),
    );
}

/// Discards local edits after the core snapshots the working image.
pub(crate) fn revert_tune(cx: &mut App) {
    send(cx, FailureCode::WriteRefused, "revert-tune", |handle| {
        handle.revert_tune()
    });
}

/// Turns the global write switch on or off and persists the choice.
pub(crate) fn set_writes_enabled(enabled: bool, cx: &mut App) {
    let accepted = send(
        cx,
        FailureCode::WriteRefused,
        "set-writes-enabled",
        move |handle| handle.set_writes_enabled(enabled),
    );

    if accepted {
        AppSettings::set_writes_enabled(cx, enabled);
    }
}

/// Sends one `[ControllerCommands]` action from a dialog button.
///
/// The core decides whether the session may run it; this sender only carries
/// the dialog and the command name.
pub(crate) fn run_controller_command(dialog: &str, command: &str, cx: &mut App) {
    let dialog = dialog.to_owned();
    let command = command.to_owned();

    send(
        cx,
        FailureCode::ControllerCommandRefused,
        "run-controller-command",
        move |handle| handle.run_controller_command(dialog, command),
    );
}

/// Clears a halted write path after the user reviewed the failure.
pub(crate) fn acknowledge_write_failure(cx: &mut App) {
    send(
        cx,
        FailureCode::WriteFailed,
        "acknowledge-write-failure",
        |handle| handle.acknowledge_write_failure(),
    );
}

/// Refreshes the derived tune status (unsaved, not sent, not burned).
pub(crate) fn refresh_tune_status(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::NothingToSave) else {
        return;
    };
    if let Err(error) = handle.refresh_tune_status() {
        tracing::debug!(%error, "the core refused the status refresh");
    }
}
