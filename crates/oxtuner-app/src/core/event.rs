//! The event boundary: failure records, the status fold and its follow-up commands.

use gpui_kit::App;
use gpui_kit::component::WindowExt as _;
use oxtuner_core::{Event, Failure, FailureCode, MAX_POLL_HZ, MIN_POLL_HZ, OpId, PortChoice};
use rust_i18n::t;

use super::commands::{
    burn_to_flash, connect_to_simulator, read_all, refresh_tune_status, start_realtime,
    write_to_ram,
};
use super::firmware;
use super::service::CoreService;
use super::status::{DialogOwner, JournalView, ResolveChain, TraceSaved, Update};

/// One second in milliseconds, for the poll interval.
const MILLIS_PER_SECOND: u32 = 1_000;

/// The next command a "use project tune" choice runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApplyStep {
    Write,
    Burn,
    Stop,
}

/// The step one update means for the resolve chain, if any.
///
/// Only the chain's own command advances it: the op is matched before the
/// update is read, so another write's failure cannot cancel the burn the user
/// asked for, and another write's success cannot start one. A link loss, a
/// project change or a rejection ends the chain whatever it was waiting for.
fn next_step(
    chain: Option<&ResolveChain>,
    op: Option<oxtuner_core::OpId>,
    rejected: bool,
    update: &Update,
) -> Option<ApplyStep> {
    let chain = chain?;

    if rejected
        || matches!(
            update,
            Update::Opened(_) | Update::Disconnected | Update::LinkLost(_)
        )
    {
        return Some(ApplyStep::Stop);
    }

    if op != Some(chain.op()) {
        // Another command's answer; the chain keeps waiting for its own.
        return None;
    }

    match (chain, update) {
        (ResolveChain::Resolve(_), Update::ConflictsResolved) => Some(ApplyStep::Write),
        (ResolveChain::Write(_), Update::Written(report)) if report.verified => {
            Some(ApplyStep::Burn)
        }
        _ => Some(ApplyStep::Stop),
    }
}

/// Writes the diagnostic anchor for the outcomes that carry a details action.
///
/// A notification's "Show details" opens Diagnostics filtered by `op`. That
/// only works when a record for the command exists with an `op` field, so the
/// event boundary always writes one. The core's own records keep the technical
/// cause; this one guarantees the link the UI offers can be followed.
fn log_outcome(event: &Event, op: Option<OpId>) {
    match event {
        // A probe miss is a scan step; the core already logs it at debug and
        // the scan reports once when every attempt is spent.
        Event::Failed { failure, .. } if failure.code == FailureCode::FirmwareNotDetected => {}
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
    // A rejection answers without an update: the pending project must still
    // end, or the splash survives until the expiry. The reason is kept beside
    // the flag for the fold below.
    let rejected_reason = match &event {
        Event::Rejected { reason, .. } => Some(*reason),
        _ => None,
    };

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

    // The port list is the core's pushed state, not one dialog's scan: the
    // on-demand answer and the pushed change fold into the same snapshot, and
    // every fold bumps the revision, so a view tells "no ports" apart from
    // "no sample yet". Monotonic on purpose: two dialogs asking at once cannot
    // undo each other's state the way a shared busy flag could.
    let ports = match &event {
        Event::PortsListed { ports, .. } | Event::PortsChanged { ports } => Some(ports),
        _ => None,
    };

    if let Some(ports) = ports {
        let service = cx.global_mut::<CoreService>();
        service.status.ports.clone_from(ports);
        service.status.ports_revision = service.status.ports_revision.saturating_add(1);
        service.status.note_ports(ports);

        // A Detect press that arrived before the first sample asked for that
        // sample instead of scanning; now it can.
        let scan_now = std::mem::take(&mut service.status.scan_after_ports);

        cx.refresh_windows();
        if scan_now {
            crate::core::firmware::start_scan(cx);
        }
        return;
    }

    // Firmware probing is a scan: a reply moves to the registry, a miss moves
    // to the next query. It never becomes a connection test result, and a
    // miss that belongs to no scan is not a visible failure either.
    if let Event::ProbeStarted { op, probe, framing } = &event {
        crate::core::firmware::probe_started(*op, *framing, *probe, cx);
        return;
    }
    if let Event::FirmwareProbed {
        op,
        port,
        baud,
        framing,
        probe,
        signature,
        ..
    } = &event
    {
        crate::core::firmware::probed(*op, port.clone(), *baud, *framing, *probe, signature, cx);
        return;
    }
    if let Event::Failed { op, failure, .. } = &event {
        // Only a refusal from the core keeps its own sentence; a silent port
        // gets the localized summary.
        let miss = if failure.code == FailureCode::ConnectFailed {
            firmware::Miss::Refused(&failure.message)
        } else {
            firmware::Miss::Silent
        };
        if firmware::missed(*op, miss, cx) {
            return;
        }
    }

    // A rejection (the core drained the queue on shutdown) still answers the
    // attempt in flight, so the scan cannot keep waiting for it.
    if let Event::Rejected { op, .. } = &event
        && firmware::missed(*op, firmware::Miss::Stopped, cx)
    {
        return;
    }

    // The core stopped: nothing will answer the probe that is waiting.
    if matches!(event, Event::Stopped) {
        crate::core::firmware::fail_scan(cx, t!("app.new_project.scan_core_stopped").as_ref());
    }

    // A probe miss that belongs to no scan is a scan step, never news.
    if let Event::Failed { failure, .. } = &event
        && failure.code == FailureCode::FirmwareNotDetected
    {
        return;
    }

    // A rejected layout must not be remembered as stored: the next change
    // sends it again, and a refused write is not a silent success.
    if let Event::ProjectLayoutSaved { stored: false, .. } = &event {
        crate::state::session::forget_sent_layout();
    }

    let (update, remember) = match event {
        Event::DefinitionLoaded { view, .. } => {
            tracing::info!(signature = %view.signature, "definition loaded");
            (Update::Loaded(view), None)
        }
        Event::DefinitionInspected { path, preview, .. } => {
            (Update::Inspected { path, preview }, None)
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
        // `ProbeStarted` is answered above and only reaches this arm as the
        // exhaustive fallback; the wizard's progress line reads it today.
        Event::FirmwareProbed { .. }
        | Event::ProbeStarted { .. }
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
    if conflict_review {
        cx.global_mut::<CoreService>().status.review_pending = true;
    }
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
    // Every accepted command answers exactly once — opened, failed or
    // rejected — and a rejection carries no update, so it arrives as its own
    // flag instead of matching an update arm.
    let finishes_open = finishes_open(&update, rejected_reason.is_some());
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

    // A dialog that started an asynchronous task ends with it; the owner keeps
    // an unrelated dialog (a settings sheet, an import consent, the review)
    // from being closed by a success it did not ask for.
    let dialog_owner = cx.global::<CoreService>().status.dialog_owner;
    let task_finished = match &update {
        Update::Connected(_) => dialog_owner == Some(DialogOwner::Connect),
        Update::Opened(_) => {
            matches!(
                dialog_owner,
                Some(DialogOwner::Connect | DialogOwner::Wizard)
            )
        }
        _ => false,
    };

    // The project choice writes and burns after the resolution; decide the
    // next step before the update is folded into the status.
    let apply_step = next_step(
        cx.global::<CoreService>().status.resolve_chain.as_ref(),
        event_op,
        rejected_reason.is_some(),
        &update,
    );

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
        if let Some(path) = import_consent {
            cx.global_mut::<CoreService>().status.import_consent = Some(path);
        }
        if let Some(root) = remember {
            crate::project::remember(&root, cx);
        }
        // Only the main layout shows the core status; the Settings window does not.
        cx.refresh_windows();
    }

    // Every accepted command answers exactly once — opened, failed or
    // rejected — so the pending project ends on every answer type. A
    // rejection carries no update, which is why this sits outside the fold
    // above instead of beside the other `finishes_open` arm. The splash
    // clears and the screen repaints here; a rejection that ended a pending
    // project also leaves its reason as the project failure, or the wizard
    // would return to a review with no message.
    if finishes_open {
        let status = &mut cx.global_mut::<CoreService>().status;
        let had_pending = status.pending_project.is_some();
        status.finish_pending_project(event_op);
        if had_pending
            && status.pending_project.is_none()
            && let Some(reason) = rejected_reason
        {
            status.failure = Some(Failure {
                code: FailureCode::ProjectRejected,
                message: reason.as_str().to_owned(),
            });
            status.failure_serial = status.failure_serial.wrapping_add(1);
        }
        cx.refresh_windows();
    }

    if auto_connect {
        connect_to_simulator(cx);
    }
    match apply_step {
        Some(ApplyStep::Write) => {
            let op = write_to_ram(cx);
            cx.global_mut::<CoreService>().status.resolve_chain = op.map(ResolveChain::Write);
        }
        Some(ApplyStep::Burn) => {
            // The chain has no step after the burn, whatever the queue does.
            cx.global_mut::<CoreService>().status.resolve_chain = None;
            let _ = burn_to_flash(cx);
        }
        Some(ApplyStep::Stop) => {
            cx.global_mut::<CoreService>().status.resolve_chain = None;
        }
        None => {}
    }
    // No-stacking makes the dialog open here the one the task started, except
    // for the rare case where the user dismissed it and opened another one
    // while the command was still in flight.
    if task_finished && let Some(window) = crate::app::window::main(cx) {
        window
            .update(cx, |_, window, cx| {
                window.close_dialog(cx);
                cx.global_mut::<CoreService>().status.dialog_owner = None;
            })
            .ok();
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
    if cx.global::<CoreService>().status.review_pending {
        // The prompt replaces whatever dialog is in the way; it is the only
        // path that satisfies the core's write gate, so it cannot wait for a
        // user to close an unrelated dialog.
        cx.defer(|cx| {
            crate::layout::write_dialogs::retry_pending_review(cx);
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

/// Whether the answer ends the pending project.
///
/// Every accepted command answers exactly once — opened, failed or rejected —
/// and a rejection carries no update, so it arrives as its own flag instead
/// of matching an update arm.
fn finishes_open(update: &Update, rejected: bool) -> bool {
    matches!(update, Update::Opened(_) | Update::Failed(_)) || rejected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resolve_chain_moves_only_on_its_own_outcomes() {
        let chain = ResolveChain::Resolve(oxtuner_core::OpId::FIRST_ID);

        assert_eq!(
            next_step(
                Some(&chain),
                Some(oxtuner_core::OpId::FIRST_ID),
                false,
                &Update::ConflictsResolved
            ),
            Some(ApplyStep::Write)
        );
        assert_eq!(
            next_step(Some(&chain), None, false, &Update::LinkLost(String::new())),
            Some(ApplyStep::Stop),
            "a chain cannot outlive its link"
        );
        assert_eq!(
            next_step(Some(&chain), None, true, &Update::Nothing),
            Some(ApplyStep::Stop),
            "a rejected command ends the chain"
        );
        assert_eq!(
            next_step(Some(&chain), None, false, &Update::Nothing),
            None,
            "another command's answer leaves the chain waiting"
        );
        assert_eq!(
            next_step(
                None,
                Some(oxtuner_core::OpId::FIRST_ID),
                false,
                &Update::ConflictsResolved
            ),
            None,
            "a chain that is not running stays still"
        );
    }

    #[test]
    fn a_rejection_ends_the_pending_project() {
        assert!(
            finishes_open(&Update::Nothing, true),
            "a rejection without an update still answers the request"
        );
        assert!(!finishes_open(&Update::Nothing, false));
    }
}
