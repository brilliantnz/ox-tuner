//! One firmware probe on one port.
//!
//! `TestConnection` verifies a definition that is already loaded; this asks an
//! unknown ECU for its signature without one. The two stay separate because a
//! probe has no definition to compare against: there is no verdict, only the
//! signature text the ECU sent.

use std::time::Duration;

use crate::contract::FirmwareProbe;
use oxtuner_protocol::ProbeSpec;

use super::{
    CoreReport, Event, EventTx, Failure, FailureCode, IoOp, IoOutcome, IoPlan, IoThread, OpId,
    Outcome, PortChoice, send,
};

/// How long the probe waits for a reply before the port is declared silent.
///
/// The connection path keeps the protocol's 1.5 s signature timeout; a scan
/// tries several ports and must not spend that on each one.
const PROBE_TIMEOUT: Duration = Duration::from_millis(350);

/// The share of the wait reserved for the port open.
///
/// Nothing bounds the open itself. An open that overruns this comes out of the
/// slack the rest of the wait holds, so a reply that uses its own timeout reads
/// as a silent port.
const PROBE_OPEN_BUDGET: Duration = Duration::from_millis(750);

/// How long the core waits for the I/O thread to answer.
///
/// Covers the probe's own settle, the open and one shared reply deadline, with
/// a second deadline of slack for the thread start. A probe that lengthens its
/// settle stretches the wait with it, so a slow board never times out against
/// a budget computed from the default settle.
fn probe_wait(spec: &ProbeSpec) -> Duration {
    spec.settle
        .saturating_add(PROBE_OPEN_BUDGET)
        .saturating_add(PROBE_TIMEOUT.saturating_mul(2))
}

/// Sends one probe and reports the signature, or that the port stayed silent.
pub(super) fn probe_firmware(
    op: OpId,
    port: &PortChoice,
    probe: FirmwareProbe,
    live_open: bool,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    if live_open {
        report.failures += 1;
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::ConnectFailed,
                    message: "disconnect the ECU before probing".to_owned(),
                },
            },
        );
        return Outcome::Continue;
    }

    let spec: ProbeSpec = probe.spec();
    let wait = probe_wait(&spec);
    let link = port.clone();
    let plan = IoPlan::probe(probe, link.clone());

    let (thread, replies) = match IoThread::start(&plan) {
        Ok(pair) => pair,
        Err(error) => return reject(op, probe, &error.to_string(), events, report),
    };

    // A fresh thread per attempt, so one request id is enough.
    let reply = match thread.submit(
        1,
        IoOp::Signature {
            timeout: Some(PROBE_TIMEOUT),
        },
    ) {
        Ok(()) => replies.recv_timeout(wait),
        Err(error) => return reject(op, probe, &error.to_string(), events, report),
    };
    // A port that could not be opened is not a silent ECU, so its own message
    // is what the user needs to see. The answer is reported before the thread
    // is joined: the join blocks, so joining first would let a stuck port
    // swallow the reply.
    let event = match reply {
        Ok(reply) => match reply.outcome {
            IoOutcome::Signature { signature } if !signature.trim().is_empty() => {
                let signature = signature.trim().to_owned();
                tracing::info!(
                    %op,
                    port = %link.label(),
                    probe = probe.as_str(),
                    %signature,
                    "ECU firmware probed"
                );
                Event::FirmwareProbed {
                    op,
                    port: link.label(),
                    baud: match link {
                        PortChoice::Serial { baud, .. } => Some(baud),
                        PortChoice::Custom(_) => None,
                    },
                    probe,
                    signature,
                }
            }
            IoOutcome::Failed(failure) => {
                tracing::debug!(%op, ?probe, reason = failure.message, "firmware probe failed");
                probe_failed(op, &failure.message)
            }
            _ => {
                tracing::debug!(%op, ?probe, "firmware probe found no signature");
                probe_failed(op, "the ECU did not answer")
            }
        },
        Err(crossfire::RecvTimeoutError::Timeout) => {
            tracing::debug!(%op, ?probe, "firmware probe timed out");
            probe_failed(op, "the ECU did not answer")
        }
        Err(crossfire::RecvTimeoutError::Disconnected) => {
            tracing::debug!(%op, ?probe, "firmware probe thread stopped");
            probe_failed(op, "the probe thread stopped before answering")
        }
    };

    if matches!(event, Event::Failed { .. }) {
        report.failures += 1;
    }
    send(events, event);
    thread.shutdown();

    Outcome::Continue
}

/// Builds the event for a probe that produced no signature.
fn probe_failed(op: OpId, reason: &str) -> Event {
    Event::Failed {
        op,
        failure: Failure {
            code: FailureCode::FirmwareNotDetected,
            message: reason.to_owned(),
        },
    }
}

/// Reports a probe that produced no signature and counts it.
fn reject(
    op: OpId,
    probe: FirmwareProbe,
    reason: &str,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    report.failures += 1;
    tracing::debug!(%op, ?probe, reason, "firmware probe found nothing");
    send(events, probe_failed(op, reason));

    Outcome::Continue
}
