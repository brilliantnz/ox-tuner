//! One firmware probe on one port.
//!
//! `TestConnection` verifies a definition that is already loaded; this asks an
//! unknown ECU for its signature without one. The two stay separate because a
//! probe has no definition to compare against: there is no verdict, only the
//! signature text the ECU sent.

use std::time::Duration;

use crate::LinkMode;
use crate::contract::FirmwareProbe;
use oxtuner_protocol::ProbeSpec;

use super::{
    CoreReport, Event, EventTx, Failure, FailureCode, IoFailureKind, IoOp, IoOutcome, IoPlan,
    IoReply, IoThread, OpId, Outcome, PortChoice, send,
};

/// How long the probe waits for a reply before the port is declared silent.
///
/// The connection path keeps the protocol's 1.5 s signature timeout; a scan
/// tries several ports and must not spend that on each one.
const PROBE_TIMEOUT: Duration = Duration::from_millis(350);

/// The share of the wait reserved for the port open.
///
/// `Transport::open` is blocking and has no timeout, so nothing here can stop a
/// stuck open. What the budget does is keep a slow one from being mistaken for a
/// silent ECU: macOS can spend well over a second re-enumerating a board that
/// the previous attempt closed, and the reply must wait for that.
const PROBE_OPEN_BUDGET: Duration = Duration::from_millis(2_000);

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

/// The shortest answer that can be a firmware signature.
///
/// A wrong baud often leaves a couple of printable bytes in the buffer, and a
/// run that short is noise rather than a signature. Every shipped definition
/// declares a longer one; a longer garbage run is still possible, so the
/// registry lookup is what finally judges the text.
const MIN_SIGNATURE_LEN: usize = 7;

/// Sends one probe and reports the signature, or that the port stayed silent.
///
/// Runs on the core thread, like `TestConnection`: one attempt holds the
/// command loop for the settle, the open and the reply timeout. A scan sends
/// one attempt per command, so the UI decides between attempts, and a stuck
/// port is released when the thread is shut down at the end of the attempt.
pub(super) fn probe_firmware(
    op: OpId,
    port: &PortChoice,
    probe: FirmwareProbe,
    framing: LinkMode,
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
    let plan = IoPlan::probe(probe, framing, link.clone());

    let (thread, replies) = match IoThread::start(&plan) {
        Ok(pair) => pair,
        Err(error) => {
            // The thread never started: the port was not asked, so this is a
            // connect failure, not a silent ECU.
            return reject(
                op,
                probe,
                FailureCode::ConnectFailed,
                &error.to_string(),
                events,
                report,
            );
        }
    };

    // A fresh thread per attempt, so one request id is enough.
    let reply = match thread.submit(
        1,
        IoOp::Signature {
            timeout: Some(PROBE_TIMEOUT),
        },
    ) {
        Ok(()) => replies.recv_timeout(wait),
        Err(error) => {
            return reject(
                op,
                probe,
                FailureCode::ConnectFailed,
                &error.to_string(),
                events,
                report,
            );
        }
    };
    // A port that could not be opened is not a silent ECU, so its own message
    // is what the user needs to see. The answer is reported before the thread
    // is joined: the join blocks, so joining first would let a stuck port
    // swallow the reply.
    let event = reply_event(&link, probe, framing, op, reply);

    if matches!(event, Event::Failed { .. }) {
        report.failures += 1;
    }
    send(events, event);
    thread.shutdown();

    Outcome::Continue
}

/// Turns the I/O thread's answer into the event the core reports.
///
/// A port that could not be opened is not a silent ECU, so its own failure is
/// reported with [`FailureCode::ConnectFailed`]; a timeout, an empty reply or a
/// short one is [`FailureCode::FirmwareNotDetected`].
fn reply_event(
    link: &PortChoice,
    probe: FirmwareProbe,
    framing: LinkMode,
    op: OpId,
    reply: Result<IoReply, crossfire::RecvTimeoutError>,
) -> Event {
    match reply {
        Ok(reply) => match reply.outcome {
            IoOutcome::Signature { signature } if signature.trim().len() >= MIN_SIGNATURE_LEN => {
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
                        PortChoice::Serial { baud, .. } => Some(*baud),
                        PortChoice::Custom(_) => None,
                    },
                    framing,
                    probe,
                    signature,
                }
            }
            IoOutcome::Failed(failure) => {
                tracing::debug!(%op, ?probe, reason = failure.message, "firmware probe failed");

                // A link that could not be opened or was lost never answered
                // the question, so it must not read as a silent ECU.
                let code = match failure.kind {
                    IoFailureKind::LinkLost => FailureCode::ConnectFailed,
                    _ => FailureCode::FirmwareNotDetected,
                };
                probe_failed(op, code, &failure.message)
            }
            _ => {
                tracing::debug!(%op, ?probe, "firmware probe found no signature");
                probe_failed(
                    op,
                    FailureCode::FirmwareNotDetected,
                    "the ECU did not answer",
                )
            }
        },
        Err(crossfire::RecvTimeoutError::Timeout) => {
            tracing::debug!(%op, ?probe, "firmware probe timed out");
            probe_failed(
                op,
                FailureCode::FirmwareNotDetected,
                "the ECU did not answer",
            )
        }
        Err(crossfire::RecvTimeoutError::Disconnected) => {
            tracing::debug!(%op, ?probe, "firmware probe thread stopped");
            probe_failed(
                op,
                FailureCode::FirmwareNotDetected,
                "the probe thread stopped before answering",
            )
        }
    }
}

/// Builds the event for a probe that produced no signature.
fn probe_failed(op: OpId, code: FailureCode, reason: &str) -> Event {
    Event::Failed {
        op,
        failure: Failure {
            code,
            message: reason.to_owned(),
        },
    }
}

/// Reports a probe that could not be asked at all, and counts it.
fn reject(
    op: OpId,
    probe: FirmwareProbe,
    code: FailureCode,
    reason: &str,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    report.failures += 1;
    tracing::debug!(%op, ?probe, reason, "firmware probe found nothing");
    send(events, probe_failed(op, code, reason));

    Outcome::Continue
}
