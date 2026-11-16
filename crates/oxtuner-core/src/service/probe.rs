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
    Event, EventTx, Failure, FailureCode, IoOp, IoOutcome, IoPlan, IoThread, OpId, Outcome,
    PortChoice, send,
};

/// How long the probe waits for a reply before the port is declared silent.
///
/// The connection path keeps the protocol's 1.5 s signature timeout; a scan
/// tries several ports and must not spend that on each one.
const PROBE_TIMEOUT: Duration = Duration::from_millis(350);

/// How long the core waits for the I/O thread to answer.
///
/// Slightly longer than the read timeout so the thread can send the reply it
/// already has before the command is declared a miss.
const PROBE_WAIT: Duration = Duration::from_millis(500);

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
                    message: "disconnect the ECU before probing another port".to_owned(),
                },
            },
        );
        return Outcome::Continue;
    }

    let spec: ProbeSpec = probe.spec();
    let link = port.clone();
    let plan = IoPlan::probe(spec, link.clone());

    let Ok((thread, replies)) = IoThread::start(&plan) else {
        return reject(op, probe, "the port could not be opened", events);
    };

    // A fresh thread per attempt, so one request id is enough.
    let _ = thread.submit(
        1,
        IoOp::ProbeSignature {
            timeout: PROBE_TIMEOUT,
        },
    );
    let reply = replies.recv_timeout(PROBE_WAIT);
    thread.shutdown();

    let signature = match reply {
        Ok(reply) => match reply.outcome {
            IoOutcome::Signature { signature } if !signature.trim().is_empty() => {
                signature.trim().to_owned()
            }
            _ => return reject(op, probe, "the ECU did not answer", events),
        },
        Err(_) => return reject(op, probe, "the ECU did not answer", events),
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
