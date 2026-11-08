//! Connection, port scan, realtime and trace commands.

use gpui_kit::App;
use oxtuner_core::{FailureCode, LinkMode, PortChoice};

use super::{handle_or_report, report_failure, send};
use crate::core::CoreService;

/// Starts a connection to the simulator with the default configuration.
pub(crate) fn connect_to_simulator(cx: &mut App) {
    connect(crate::link::simulator(), None, cx);
}

/// Asks for the port list, then opens the connect dialog when it arrives.
pub(crate) fn request_connect(cx: &mut App) {
    // The dialog opens at once and stays open; its port list fills in when the
    // scan answers, so a refresh never costs the user their selection.
    cx.defer(|cx| {
        if let Some(window) = crate::app::window::main(cx) {
            window
                .update(cx, |_, window, cx| {
                    crate::layout::connect_dialog::show(window, cx);
                })
                .ok();
        }
    });
    refresh_ports(cx);
}

/// Rescans the serial ports without touching the dialog.
pub(crate) fn refresh_ports(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ConnectFailed) else {
        return;
    };
    cx.global_mut::<CoreService>().status.scanning = true;
    if let Err(error) = handle.list_ports() {
        tracing::warn!(%error, "the core refused the port scan");
        cx.global_mut::<CoreService>().status.scanning = false;
        report_failure(cx, FailureCode::ConnectFailed, error.to_string());
    }
}

/// Reads a port's signature without opening a session.
pub(crate) fn test_connection(port: PortChoice, framing: Option<LinkMode>, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TestFailed) else {
        return;
    };
    let label = port.label();
    cx.global_mut::<CoreService>().status.testing = Some(label);
    if let Err(error) = handle.test_connection(port, framing) {
        tracing::warn!(%error, "the core refused the connection test");
        cx.global_mut::<CoreService>().status.testing = None;
        report_failure(cx, FailureCode::TestFailed, error.to_string());
    }
}

/// Opens a connection.
pub(crate) fn connect(port: PortChoice, framing: Option<LinkMode>, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ConnectFailed) else {
        return;
    };
    cx.global_mut::<CoreService>().status.pending_connect = Some(port.clone());
    if let Err(error) = handle.connect(port, framing) {
        tracing::warn!(%error, "the core refused the connection");
        cx.global_mut::<CoreService>().status.pending_connect = None;
        report_failure(cx, FailureCode::ConnectFailed, error.to_string());
    }
}

/// Starts periodic realtime polling at the configured rate.
pub(crate) fn start_realtime(interval_ms: u32, cx: &mut App) {
    send(
        cx,
        FailureCode::RealtimeRejected,
        "start-realtime",
        move |handle| handle.start_realtime(interval_ms),
    );
}

/// Closes the connection.
pub(crate) fn disconnect(cx: &mut App) {
    send(cx, FailureCode::NotConnected, "disconnect", |handle| {
        handle.disconnect()
    });
}

/// Reads every page into the tune images.
pub(crate) fn read_all(cx: &mut App) {
    send(cx, FailureCode::ReadFailed, "read-all", |handle| {
        handle.read_all()
    });
}

/// Makes the tune read from the ECU the working image.
///
/// After a read the project file's values stay on screen, so a write would
/// send the file rather than the controller's tune. This is the explicit way to
/// edit what the ECU actually holds.
pub(crate) fn adopt_ecu_tune(cx: &mut App) {
    send(cx, FailureCode::AdoptRefused, "adopt-ecu-image", |handle| {
        handle.adopt_ecu_image()
    });
}

/// Saves the session trace to the app's trace directory.
pub(crate) fn save_trace(cx: &mut App) {
    let path = crate::platform::paths::AppPaths::discover().next_trace_path();

    send(cx, FailureCode::TraceFailed, "save-trace", move |handle| {
        handle.save_trace(path)
    });
}
