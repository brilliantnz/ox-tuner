//! One firmware probe on one port.
//!
//! `TestConnection` verifies a definition that is already loaded; this asks an
//! unknown ECU for its signature without one. The two stay separate because a
//! probe has no definition to compare against: there is no verdict, only the
//! signature text the ECU sent.

use std::time::Duration;

use crate::contract::FirmwareProbe;
use oxtuner_protocol::{PROBE_PORT_SETTLE, ProbeSpec};

use super::{
    Event, EventTx, Failure, FailureCode, IoOp, IoOutcome, IoPlan, IoThread, OpId, Outcome,
    PortChoice, send,
};

/// How long the probe waits for a reply before the port is declared silent.
///
/// The connection path keeps the protocol's 1.5 s signature timeout; a scan
/// tries several ports and must not spend that on each one.
const PROBE_TIMEOUT: Duration = Duration::from_millis(350);

/// The share of the wait reserved for the port open.
///
/// Nothing bounds the open itself, so a port that takes longer than this is
/// reported as silent.
const PROBE_OPEN_BUDGET: Duration = Duration::from_millis(750);

/// How long the core waits for the I/O thread to answer.
///
/// Covers the settle, the open and one shared reply deadline, with a second
/// deadline of slack for the thread start.
const PROBE_WAIT: Duration = PROBE_PORT_SETTLE
    .saturating_add(PROBE_OPEN_BUDGET)
    .saturating_add(PROBE_TIMEOUT.saturating_mul(2));

/// Sends one probe and reports the signature, or that the port stayed silent.
pub(super) fn probe_firmware(
    op: OpId,
    port: &PortChoice,
    probe: FirmwareProbe,
    live_open: bool,
    events: &EventTx,
) -> Outcome {
    if live_open {
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
    let link = port.clone();
    let plan = IoPlan::probe(spec, link.clone());

    let (thread, replies) = match IoThread::start(&plan) {
        Ok(pair) => pair,
        Err(error) => return reject(op, probe, &error.to_string(), events),
    };

    // A fresh thread per attempt, so one request id is enough.
    let reply = match thread.submit(
        1,
        IoOp::Signature {
            timeout: Some(PROBE_TIMEOUT),
        },
    ) {
        Ok(()) => replies.recv_timeout(PROBE_WAIT),
        Err(error) => return reject(op, probe, &error.to_string(), events),
    };
    thread.shutdown();

    // A port that could not be opened is not a silent ECU, so its own message
    // is what the user needs to see.
    let signature = match reply {
        Ok(reply) => match reply.outcome {
            IoOutcome::Signature { signature } if !signature.trim().is_empty() => {
                signature.trim().to_owned()
            }
            IoOutcome::Failed(failure) => return reject(op, probe, &failure.message, events),
            _ => return reject(op, probe, "the ECU did not answer", events),
        },
        Err(crossfire::RecvTimeoutError::Timeout) => {
            return reject(op, probe, "the ECU did not answer", events);
        }
        Err(crossfire::RecvTimeoutError::Disconnected) => {
            return reject(
                op,
                probe,
                "the probe thread stopped before answering",
                events,
            );
        }
    };

    tracing::info!(
        %op,
        port = %link.label(),
        probe = probe.as_str(),
        %signature,
        "ECU firmware probed"
    );
    send(
        events,
        Event::FirmwareProbed {
            op,
            port: link.label(),
            baud: match link {
                PortChoice::Serial { baud, .. } => Some(baud),
                PortChoice::Custom(_) => None,
            },
            probe,
            signature,
        },
    );

    Outcome::Continue
}

/// Reports a probe that produced no signature.
fn reject(op: OpId, probe: FirmwareProbe, reason: &str, events: &EventTx) -> Outcome {
    tracing::debug!(%op, ?probe, reason, "firmware probe found nothing");
    send(
        events,
        Event::Failed {
            op,
            failure: Failure {
                code: FailureCode::FirmwareNotDetected,
                message: reason.to_owned(),
            },
        },
    );

    Outcome::Continue
}
