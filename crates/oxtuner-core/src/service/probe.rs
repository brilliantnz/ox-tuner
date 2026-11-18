//! One firmware probe group on one port.
//!
//! `TestConnection` verifies a definition that is already loaded; this asks an
//! unknown ECU for its signature without one. The two stay separate because a
//! probe has no definition to compare against: there is no verdict, only the
//! signature text the ECU sent.
//!
//! A group opens the port once and asks its queries in order, so a silent
//! family or a second framing does not pay for another settle. The group stops
//! at the first answer: only the application can tell an ECU from a chatty
//! adapter, so the next question after an answer is its decision, not this
//! module's.

use std::time::{Duration, Instant};

use crossfire::{Rx, spsc};

use crate::contract::ProbeQuery;

use super::{
    CoreReport, Event, EventTx, Failure, FailureCode, IoFailureKind, IoOp, IoOutcome, IoPlan,
    IoReply, IoThread, OpId, Outcome, PortChoice, send,
};

/// How long one query waits for its reply before the next one is asked.
///
/// The connection path keeps the protocol's 1.5 s signature timeout; a scan
/// tries several ports and must not spend that on each query.
const PROBE_TIMEOUT: Duration = Duration::from_millis(350);

/// The share of the group's budget reserved for the port open.
///
/// `Transport::open` is blocking and has no timeout, so nothing here can stop a
/// stuck open. What the budget does is keep a slow one from being mistaken for a
/// silent ECU: macOS can spend well over a second re-enumerating a board that
/// the previous attempt closed, and the reply must wait for that.
const PROBE_OPEN_BUDGET: Duration = Duration::from_millis(2_000);

/// How long a stopping probe thread is given to finish before it is detached.
///
/// The thread closes its own transport when it exits, so a detached thread
/// releases the port as soon as its blocking call returns. Waiting on it is what
/// would let one wedged port hold the core, so the wait is bounded and the
/// thread is left to close what it opened.
const PROBE_THREAD_GRACE: Duration = Duration::from_millis(500);

/// The shortest answer that can be a firmware signature.
///
/// A wrong baud often leaves a couple of printable bytes in the buffer, and a
/// run that short is noise rather than a signature. Every shipped definition
/// declares a longer one; a longer garbage run is still possible, so the
/// registry lookup is what finally judges the text.
const MIN_SIGNATURE_LEN: usize = 7;

/// How long the core waits for the whole group to finish.
///
/// Covers the settle, the open and every query's own reply deadline, with a
/// second deadline of slack per query for the thread. A group that lengthens its
/// settle stretches the wait with it, so a slow board never times out against a
/// budget computed from the default settle.
fn probe_wait(queries: &[ProbeQuery]) -> Duration {
    let settle = queries
        .iter()
        .map(|query| query.probe.spec().settle)
        .max()
        .unwrap_or_default();
    let replies = PROBE_TIMEOUT.saturating_mul(u32::try_from(queries.len()).unwrap_or(u32::MAX));

    settle
        .saturating_add(PROBE_OPEN_BUDGET)
        .saturating_add(replies.saturating_mul(2))
}

/// Sends one group of queries and reports the first signature it hears.
///
/// Runs on the core thread, like `TestConnection`: one group holds the command
/// loop for the settle, the open and its queries' deadlines. Every query is
/// reported as it starts, so the scan can show progress inside the group. A
/// port that answered nothing is reported once, at the end, and the thread is
/// released with a bounded wait.
pub(super) fn probe_firmware(
    op: OpId,
    port: &PortChoice,
    queries: &[ProbeQuery],
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

    if queries.is_empty() {
        return reject(
            op,
            FailureCode::FirmwareNotDetected,
            "the probe had nothing to ask",
            events,
            report,
        );
    }

    let link = port.clone();
    let plan = IoPlan::probe(queries, link.clone());
    let (thread, replies) = match IoThread::start(&plan) {
        Ok(pair) => pair,
        Err(error) => {
            // The thread never started: the port was not asked, so this is a
            // connect failure, not a silent ECU.
            return reject(
                op,
                FailureCode::ConnectFailed,
                &error.to_string(),
                events,
                report,
            );
        }
    };

    let event = ask_group(
        op,
        &link,
        queries,
        &thread,
        &replies,
        probe_wait(queries),
        events,
    );

    if matches!(event, Event::Failed { .. }) {
        report.failures += 1;
    }

    // The answer is reported before the thread is released: the release waits
    // on the thread, so reporting first keeps a stuck port from swallowing the
    // reply.
    send(events, event);
    thread.shutdown_within(PROBE_THREAD_GRACE);

    Outcome::Continue
}

/// Asks every query of the group over the open thread, up to `wait`.
///
/// Returns the event that ends the group: the first answer, a link failure, or
/// the miss that says the port answered nothing.
fn ask_group(
    op: OpId,
    link: &PortChoice,
    queries: &[ProbeQuery],
    thread: &IoThread,
    replies: &Rx<spsc::Array<IoReply>>,
    wait: Duration,
    events: &EventTx,
) -> Event {
    let deadline = Instant::now() + wait;
    let mut answer = None;
    let mut stop = None;

    for query in queries {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }

        send(
            events,
            Event::ProbeStarted {
                op,
                probe: query.probe,
                framing: query.framing,
            },
        );

        // One request is in flight at a time and a timed-out wait ends the
        // group, so the id is the trace's, not a correlation key. The loop
        // index would suggest otherwise.
        let submitted = thread.submit(
            1,
            IoOp::ProbeSignature {
                query: *query,
                timeout: PROBE_TIMEOUT,
            },
        );
        if let Err(error) = submitted {
            stop = Some(probe_failed(
                op,
                FailureCode::ConnectFailed,
                &error.to_string(),
            ));
            break;
        }

        match replies.recv_timeout(left) {
            Ok(reply) => match classify(reply.outcome, link, op, *query) {
                ProbeReply::Answered(event) => {
                    answer = Some(event);
                    break;
                }
                ProbeReply::Silent => {}
                ProbeReply::Stopped(event) => {
                    stop = Some(event);
                    break;
                }
            },
            Err(crossfire::RecvTimeoutError::Timeout) => break,
            Err(crossfire::RecvTimeoutError::Disconnected) => {
                stop = Some(probe_failed(
                    op,
                    FailureCode::FirmwareNotDetected,
                    "the probe thread stopped before answering",
                ));
                break;
            }
        }
    }

    answer.or(stop).unwrap_or_else(|| {
        probe_failed(
            op,
            FailureCode::FirmwareNotDetected,
            "the ECU did not answer",
        )
    })
}

/// What one query's answer means for the group.
enum ProbeReply {
    /// The ECU answered with something long enough to be a signature.
    Answered(Event),
    /// Nothing usable came back; the group can ask its next query.
    Silent,
    /// The link failed, so the rest of the group would fail too.
    Stopped(Event),
}

/// Turns the I/O thread's answer into the event the core reports.
///
/// A port that could not be opened is not a silent ECU, so its own failure is
/// reported with [`FailureCode::ConnectFailed`] and ends the group. A timeout or
/// a short answer leaves the group free to try its next query.
fn classify(outcome: IoOutcome, link: &PortChoice, op: OpId, query: ProbeQuery) -> ProbeReply {
    match outcome {
        IoOutcome::Signature { signature } if signature.trim().len() >= MIN_SIGNATURE_LEN => {
            let signature = signature.trim().to_owned();
            tracing::info!(
                %op,
                port = %link.label(),
                probe = query.probe.as_str(),
                %signature,
                "ECU firmware probed"
            );

            ProbeReply::Answered(Event::FirmwareProbed {
                op,
                port: link.label(),
                baud: match link {
                    PortChoice::Serial { baud, .. } => Some(*baud),
                    PortChoice::Custom(_) => None,
                },
                framing: query.framing,
                probe: query.probe,
                signature,
            })
        }
        IoOutcome::Failed(failure) => {
            tracing::debug!(
                %op,
                probe = query.probe.as_str(),
                reason = failure.message,
                "firmware probe failed"
            );

            // A link that could not be opened or was lost never answered the
            // question, so it must not read as a silent ECU.
            match failure.kind {
                IoFailureKind::LinkLost => ProbeReply::Stopped(probe_failed(
                    op,
                    FailureCode::ConnectFailed,
                    &failure.message,
                )),
                _ => ProbeReply::Silent,
            }
        }
        _ => {
            tracing::debug!(%op, probe = query.probe.as_str(), "firmware probe found no signature");
            ProbeReply::Silent
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
    code: FailureCode,
    reason: &str,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    report.failures += 1;
    tracing::debug!(%op, reason, "firmware probe found nothing");
    send(events, probe_failed(op, code, reason));

    Outcome::Continue
}
