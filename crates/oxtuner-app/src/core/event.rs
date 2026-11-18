//! The event boundary: failure records, the status fold and its follow-up commands.

use gpui_kit::App;
use oxtuner_core::{Event, FailureCode, MAX_POLL_HZ, MIN_POLL_HZ, OpId, PortChoice};

use super::commands::{connect_to_simulator, read_all, refresh_tune_status, start_realtime};
use super::service::CoreService;
use super::status::{JournalView, TraceSaved, Update};

/// One second in milliseconds, for the poll interval.
const MILLIS_PER_SECOND: u32 = 1_000;

/// Writes the diagnostic anchor for the outcomes that carry a details action.
///
/// A notification's "Show details" opens Diagnostics filtered by `op`. That
/// only works when a record for the command exists with an `op` field, so the
/// event boundary always writes one. The core's own records keep the technical
/// cause; this one guarantees the link the UI offers can be followed.
fn log_outcome(event: &Event, op: Option<OpId>) {
    match event {
        Event::Failed { failure, .. } => {
            // `message` is tracing's own field; the failure text goes in
            // `detail` so the record keeps its event message and the field.
            // One line is enough here: the core keeps the full diagnostic.
            let detail = failure
                .message
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default();

            if let Some(op) = op {
                tracing::warn!(
                    %op,
                    code = failure.code.as_str(),
                    %detail,
                    "the core reported a failure"
                );
            } else {
                tracing::warn!(
                    code = failure.code.as_str(),
                    %detail,
                    "the core reported a failure"
                );
            }
        }
        Event::Rejected { reason, .. } => {
            if let Some(op) = op {
                tracing::warn!(
                    %op,
                    reason = reason.as_str(),
                    "the core rejected the command"
                );
            } else {
                tracing::warn!(reason = reason.as_str(), "the core rejected the command");
            }
        }
        _ => {}
    }
}

/// Folds one event into the status and repaints.
#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive match over every core event keeps the status fold in one place"
)]
pub(super) fn apply(event: Event, cx: &mut App) {
    let event_op = event.op();

    // Every failure and rejection gets one record with the op field that
    // Diagnostics filters on. The core logs its own technical detail, but not
    // every failure path there carries the op; this app-side boundary is the
    // one place every answered command passes through.
    log_outcome(&event, event_op);

    // The notification policy runs before the status is folded, so a reconnect
    // can tell that the link had visibly been lost. Publishing now also means
    // the toast arrives with the same event that changed the status.
    let link_was_lost = cx.global::<CoreService>().status.link_lost.clone();
    crate::notifications::publish_event(&event, link_was_lost.as_deref(), cx);

    // The port list is stored for the dialog, which stays open while it scans:
    // closing and reopening would lose the user's selection and scroll. The core
    // publishes the same snapshot on a change, so both paths land here.
    // A change carries a fresh list, but only the answer to the scan ends the
    // scan: a change that arrives mid-scan must not hide the dialog's state for
    // the rest of the round trip.
    let ports = match &event {
        Event::PortsListed { ports, .. } => Some((ports, true)),
        Event::PortsChanged { ports } => Some((ports, false)),
        _ => None,
    };

    if let Some((ports, answered)) = ports {
        let service = cx.global_mut::<CoreService>();
        service.status.ports.clone_from(ports);

        if answered {
            service.status.scanning = false;
        }

        cx.refresh_windows();
        return;
    }

    let (update, remember) = match event {
        Event::DefinitionLoaded { view, .. } => {
            tracing::info!(signature = %view.signature, "definition loaded");
            (Update::Loaded(view), None)
        }
        Event::ProjectOpened { view, .. } => {
            tracing::info!(
                root = %view.root.display(),
                signature = %view.definition.signature,
                read_only = view.read_only,
                "project opened"
            );
            // Remember the folder only after the core opened it: the recent
            // list must not gain paths that turned out not to be projects.
            let root = view.root.clone();
            (Update::Opened(Box::new(view)), Some(root))
        }
        Event::TuneSaved { outcome, .. } => {
            tracing::info!(
                path = %outcome.path.display(),
                cells = outcome.cells,
                "tune saved"
            );
            (Update::Saved(outcome), None)
        }
        Event::TableRead { table, .. } => (
            Update::Table {
                table: Box::new(table),
                edited: false,
            },
            None,
        ),
        Event::TableEdited { table, .. } => (
            Update::Table {
                table: Box::new(table),
                edited: true,
            },
            None,
        ),
        Event::DialogRead { dialog, .. } => (
            Update::Dialog {
                dialog,
                edited: false,
            },
            None,
        ),
        Event::DialogEdited { dialog, .. } => (
            Update::Dialog {
                dialog,
                edited: true,
            },
            None,
        ),
        Event::MenuChanged { menu } => (Update::Menu(menu), None),
        Event::ConnectionTested { view, .. } => (Update::Tested(view), None),
        Event::Connected { view, .. } => (Update::Connected(view), None),
        Event::Reconnected { view } => (Update::Reconnected(view), None),
        Event::ConnectionLost { reason } => (Update::LinkLost(reason), None),
        Event::Disconnected { .. } => (Update::Disconnected, None),
        Event::PagesRead { report, .. } => (Update::PagesRead(report), None),
        Event::EcuAdopted { .. } => (Update::EcuAdopted, None),
        Event::RealtimeState { running, .. } => (Update::Realtime(running), None),
        Event::TraceSaved { path, frames, .. } => (
            Update::Trace {
                saved: TraceSaved { path, frames },
            },
            None,
        ),
        Event::TuneWritten { report, .. } => (Update::Written(report), None),
        Event::TuneBurned { report, .. } => (Update::Burned(report), None),
        Event::ControllerActionRan { .. } => (Update::ControllerActionRan, None),
        Event::ConflictsResolved { .. } => (Update::ConflictsResolved, None),
        Event::TuneImported { report, .. } => (Update::Imported(report), None),
        Event::TuneReverted { report, .. } => (Update::Reverted(report), None),
        Event::TuneStatusRead { status, .. } => (Update::TuneStatus(status), None),
        Event::WriteAccessChanged {
            writes_enabled,
            connection,
            ..
        } => (
            Update::WriteAccess {
                enabled: writes_enabled,
                connection,
            },
            None,
        ),
        Event::WriteFailureAcknowledged { .. } => (Update::Acknowledged, None),
        Event::JournalRead { path, entries, .. } => {
            (Update::Journal(JournalView { path, entries }), None)
        }
        Event::SnapshotsListed { paths, .. } => (Update::Snapshots(paths), None),
        Event::LogState {
            running, report, ..
        } => (Update::Log { running, report }, None),
        Event::DefinitionUnloaded { .. } => (Update::Unloaded, None),
        Event::Failed { failure, .. } => (Update::Failed(failure), None),
        // No view reads a probe result yet, so the core log carries the answer.
        Event::FirmwareProbed { .. }
        | Event::ProjectLayoutSaved { .. }
        | Event::PortsListed { .. }
        | Event::PortsChanged { .. }
        | Event::Rejected { .. }
        | Event::Stopped => (Update::Nothing, None),
    };

    let auto_connect = matches!(&update, Update::Loaded(_) | Update::Opened(_))
        && cx.global::<CoreService>().auto_sim
        && cx.global::<CoreService>().status.connection.is_none();
    let conflict_review =
        matches!(&update, Update::PagesRead(report) if !report.conflicts.is_empty());
    let import_consent = if matches!(
        &update,
        Update::Failed(failure) if failure.code == FailureCode::ImportNeedsConsent
    ) {
        cx.global_mut::<CoreService>().pending_import.take()
    } else {
        None
    };
    let needs_import_consent = import_consent.is_some();
    let refresh_status = match &update {
        Update::Opened(_)
        | Update::PagesRead(_)
        | Update::Written(_)
        | Update::Burned(_)
        | Update::ControllerActionRan
        | Update::ConflictsResolved
        | Update::Imported(_)
        | Update::Reverted(_)
        | Update::Saved(_) => true,
        Update::Table { edited, .. } | Update::Dialog { edited, .. } => *edited,
        Update::Failed(failure) => matches!(
            failure.code,
            FailureCode::WriteFailed | FailureCode::BurnFailed | FailureCode::ImportNeedsConsent
        ),
        _ => false,
    };
    let read_pages = matches!(&update, Update::Connected(_) | Update::Reconnected(_));
    let poll = matches!(&update, Update::PagesRead(_));
    let finishes_open = matches!(&update, Update::Opened(_) | Update::Failed(_));
    // A project carries its own panel layout; standalone tunes keep the
    // installation's.
    let project_layout = match &update {
        Update::Opened(view) if view.in_project => Some(view.layout.clone()),
        _ => None,
    };
    // The core persisted this connection into the project state; mirror it in the
    // open view so a dialog opened in the same session starts from it too.
    let confirmed_connection = match &update {
        Update::Connected(connection) if !connection.custom => {
            let baud = cx
                .global::<CoreService>()
                .status
                .pending_connect
                .as_ref()
                .and_then(|pending| match pending {
                    PortChoice::Serial { baud, .. } => Some(*baud),
                    PortChoice::Custom(_) => None,
                });
            Some((connection.port.clone(), baud))
        }
        _ => None,
    };

    if !matches!(&update, Update::Nothing) {
        cx.global_mut::<CoreService>().status.apply(update);
        if let Some(layout) = project_layout {
            crate::state::session::AppState::update_global(cx, |state| {
                state.layout.apply_project_layout(&layout);
            });
        }
        if let Some((port, baud)) = confirmed_connection
            && let Some(project) = cx.global_mut::<CoreService>().status.project.as_mut()
        {
            project.connection.last_port = Some(port);
            project.connection.baud_override = baud;
        }
        if finishes_open {
            cx.global_mut::<CoreService>()
                .status
                .finish_pending_project(event_op);
        }
        if let Some(path) = import_consent {
            cx.global_mut::<CoreService>().status.import_consent = Some(path);
        }
        if let Some(root) = remember {
            crate::project::remember(&root, cx);
        }
        // Only the main layout shows the core status; the Settings window does not.
        cx.refresh_windows();
    }

    if auto_connect {
        connect_to_simulator(cx);
    }
    if read_pages {
        // The session is read-only until proven otherwise; pulling the tune in
        // is the first thing it does, and it is also what fills the grid's
        // write-state after a reconnect.
        read_all(cx);
    }
    if poll {
        // The user's poll rate lives in the global connection settings; the
        // core clamps it to the same range.
        let hz = crate::settings::preferences::AppSettings::global_poll_rate_hz(cx)
            .clamp(MIN_POLL_HZ, MAX_POLL_HZ);
        start_realtime(MILLIS_PER_SECOND / hz, cx);
    }
    if refresh_status {
        refresh_tune_status(cx);
    }
    if conflict_review {
        cx.defer(|cx| {
            if let Some(window) = crate::app::window::main(cx) {
                window
                    .update(cx, |_, window, cx| {
                        crate::layout::write_dialogs::show_conflicts(window, cx);
                    })
                    .ok();
            }
        });
    }
    if needs_import_consent {
        cx.defer(|cx| {
            if let Some(window) = crate::app::window::main(cx) {
                window
                    .update(cx, |_, window, cx| {
                        crate::layout::write_dialogs::show_import_consent(window, cx);
                    })
                    .ok();
            }
        });
    }
}
