//! Command senders, grouped by the part of the session they act on.

mod edits;
mod log;
mod project;
mod session;
mod transfer;
mod tune;

pub(crate) use edits::{edit_axis, edit_scalar, edit_table, read_dialog, read_table};
pub(crate) use log::{start_log, stop_log};
pub(crate) use project::{
    choose_definition, close_project, open_project, refresh_project_availability,
    save_project_layout, save_tune,
};
pub(crate) use session::{
    adopt_ecu_tune, connect, connect_to_simulator, disconnect, read_all, refresh_ports,
    request_connect, save_trace, start_realtime, test_connection,
};
pub(crate) use transfer::{
    choose_import, import_tune_with_consent, list_snapshots, read_journal, restore_snapshot,
};
pub(crate) use tune::{
    acknowledge_write_failure, burn_to_flash, refresh_tune_status, resolve_conflicts,
    revert_target, revert_tune, run_controller_command, set_writes_enabled, write_to_ram,
};

use std::future::Future;

use gpui_kit::App;
use oxtuner_core::{CoreHandle, Failure, FailureCode};

use super::service::CoreService;

/// The core handle, or a recorded failure when the core never started.
fn handle_or_report(cx: &mut App, code: FailureCode) -> Option<CoreHandle> {
    if let Some(handle) = CoreService::handle(cx) {
        Some(handle)
    } else {
        tracing::error!("the core is not running");
        report_failure(cx, code, "the core is not running".to_owned());
        None
    }
}

/// Sends one command and reports a refused send the same way everywhere.
///
/// Returns whether the command was accepted by the queue. The answer itself
/// still arrives as an event; a command that needs pending state sets it before
/// calling this.
pub(super) fn send<T>(
    cx: &mut App,
    code: FailureCode,
    command: &'static str,
    send: impl FnOnce(CoreHandle) -> Result<T, oxtuner_core::CoreError>,
) -> bool {
    let Some(handle) = handle_or_report(cx, code) else {
        return false;
    };

    match send(handle) {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(%error, command, "the core refused the command");
            report_failure(cx, code, error.to_string());
            false
        }
    }
}

/// Shows a failure that never reached the core.
///
/// The global is installed by [`start`] before the first window exists, so this
/// normally has somewhere to put the message; the guard is for the boot order,
/// where a failure can only be logged.
pub(crate) fn report_failure(cx: &mut App, code: FailureCode, message: String) {
    if !cx.has_global::<CoreService>() {
        tracing::error!(code = code.as_str(), %message, "failure before the core was installed");
        return;
    }

    let status = &mut cx.global_mut::<CoreService>().status;
    status.failure_serial = status.failure_serial.wrapping_add(1);
    status.failure = Some(Failure { code, message });
    cx.refresh_windows();
}

/// Tells the core to stop and returns the future that resolves once it is gone.
///
/// Every blocking step runs on the background pool: the foreground executor has
/// to keep running the event pump until [`oxtuner_core::Event::Stopped`], or
/// the core's last sends block on a full events queue and the join would never
/// return. The returned future is awaited by the quit hook (`on_app_quit`), so
/// quitting is deferred until the core's thread is joined.
pub(crate) fn request_shutdown(cx: &mut App) -> impl Future<Output = ()> + use<> {
    let (handle, thread) = cx.global_mut::<CoreService>().take_core();

    let stopped = cx.background_executor().spawn(async move {
        // Bounded: a shutdown that cannot be queued is reported, never waited
        // on forever.
        if let Some(handle) = handle
            && let Err(error) = handle.shutdown()
        {
            tracing::debug!(%error, "the core was already stopping");
        }

        if let Some(thread) = thread {
            let report = thread.join();
            tracing::debug!(
                handled = report.handled,
                rejected = report.rejected,
                "the core stopped"
            );
        }
    });

    async move {
        stopped.await;
    }
}
