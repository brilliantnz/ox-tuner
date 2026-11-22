#![allow(
    clippy::wildcard_imports,
    reason = "the service modules are one implementation split across files"
)]

use super::dispatch::send;

use super::transfer::{
    advance_write, fail_transaction, finish_command_failed, record_command_result,
};
use super::*;
use crate::contract::ConflictReport;
use oxtuner_tune::{CellValue, Conflict};

#[expect(
    clippy::too_many_arguments,
    reason = "the teardown owns every resource the session holds"
)]
pub(super) fn teardown(
    live: &mut Option<Live>,
    session: &mut Session,
    safety: &mut Safety,
    _journal_dir: Option<&std::path::Path>,
    latest: &LatestFrame,
    events: &EventTx,
    history: &ChartHistory,
    log: &LogStatus,
) {
    let Some(mut live) = live.take() else {
        return;
    };

    if let Some(transaction) = live.txns.write_tx.take() {
        let reason = "the session was closed while the write was running";
        safety.halt(reason);
        if let Some(journal) = &transaction.journal {
            let entry = JournalEntry {
                at: journal::timestamp(),
                operation: transaction.kind.journal_name().to_owned(),
                phase: "result".to_owned(),
                port: live.connection.link_label.clone(),
                signature: live.connection.signature.clone(),
                pages: journal::pages_label(&transaction.pages),
                cells: transaction.cells,
                variables: transaction.variables,
                result: "failed".to_owned(),
                detail: reason.to_owned(),
            };
            if let Err(error) = journal.append(&entry) {
                tracing::warn!(%error, "the journal result could not be written");
            }
        }
        session.mark_unknown();
        tracing::warn!(operation = transaction.kind.event(), %reason, "write interrupted");
    }

    // Dropping the logger flushes it; the shared status stops.
    if live.logging.logger.take().is_some() {
        tracing::info!("the session log was closed with the session");
    }
    log.mark_stopped();

    finish_command_failed(
        &mut live,
        "the session was closed before the command completed",
    );
    for pending in std::mem::take(&mut live.requests.pending).into_values() {
        match pending.kind {
            // Background work the UI never asked for: no answer to send.
            PendingKind::Realtime | PendingKind::Reconnect => {}
            // The intent of a disconnect is fulfilled by closing the session.
            PendingKind::Disconnect => send(events, Event::Disconnected { op: pending.op }),
            _ => send(
                events,
                Event::Failed {
                    op: pending.op,
                    failure: Failure {
                        code: failure_code(pending.kind),
                        message: if matches!(
                            pending.kind,
                            PendingKind::CreateSignature
                                | PendingKind::CreateRead
                                | PendingKind::CreateVersion
                        ) {
                            "the core stopped before the ECU read finished".to_owned()
                        } else {
                            "the session was closed before the operation completed".to_owned()
                        },
                    },
                },
            ),
        }
    }
    live.connection.thread.shutdown();
    latest.clear();
    history.clear();
}

/// How long the core may wait for the next command.
pub(super) fn next_wait(live: Option<&Live>) -> Option<Duration> {
    let live = live?;
    if !live.requests.pending.is_empty() {
        return Some(PENDING_WAIT);
    }

    let now = Instant::now();
    let poll = live
        .timers
        .poll
        .as_ref()
        .map(|poll| poll.next_at.saturating_duration_since(now));
    let reconnect = live
        .timers
        .reconnect
        .as_ref()
        .map(|reconnect| reconnect.next_at.saturating_duration_since(now));
    let burn = live
        .txns
        .burn_wait
        .as_ref()
        .map(|wait| wait.next_at.saturating_duration_since(now));
    [poll, reconnect, burn]
        .into_iter()
        .flatten()
        .min()
        .map(|wait| wait.max(Duration::from_millis(1)))
}

/// Rebuilds the chart history's channel list from a definition, clearing old
/// frames because their names belong to the previous definition.
pub(super) fn configure_history(history: &ChartHistory, definition: &Definition) {
    let channels = RealtimeDecoder::new(definition)
        .columns()
        .into_iter()
        .map(|(name, units)| ChannelInfo::new(name, units))
        .collect();
    history.configure(channels);
}

/// Places a decoded frame's values into the log's column order.
///
/// A decoder may skip a channel that cannot be read from a short or malformed
/// block, and a computed channel may not resolve. The log keeps its full header
/// and writes an empty cell for those, rather than shifting every later column.
pub(super) fn align_log_values(columns: &[String], frame: &RealtimeFrame) -> Vec<f64> {
    // A frame normally carries every column in definition order, so the common
    // case is a plain copy with no lookup table to build and throw away.
    if frame.values.len() == columns.len()
        && frame
            .values
            .iter()
            .zip(columns)
            .all(|(value, name)| value.name.as_str() == name.as_str())
    {
        return frame.values.iter().map(|value| value.value).collect();
    }
    let lookup: std::collections::HashMap<&str, f64> = frame
        .values
        .iter()
        .map(|value| (value.name.as_str(), value.value))
        .collect();
    columns
        .iter()
        .map(|name| lookup.get(name.as_str()).copied().unwrap_or(f64::NAN))
        .collect()
}

/// Starts a connection for `Connect`.
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the handler mirrors the fields of one protocol request and keeps the connect sequence in order"
)]
pub(super) fn connect(
    op: OpId,
    session: &mut Session,
    live: &mut Option<Live>,
    events: &EventTx,
    report: &mut CoreReport,
    port: &PortChoice,
    framing: Option<LinkMode>,
    probe: bool,
) -> Outcome {
    if live.is_some() {
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::ConnectFailed,
                    message: if probe {
                        "disconnect the ECU before testing another port".to_owned()
                    } else {
                        "a connection is already open; disconnect it first".to_owned()
                    },
                },
            },
        );
        return Outcome::Continue;
    }

    let Some(definition) = session.definition() else {
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::ConnectFailed,
                    message: "open a project or a definition before connecting".to_owned(),
                },
            },
        );
        return Outcome::Continue;
    };
    let Some(behavior) = BehaviorKind::select(definition) else {
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::ConnectFailed,
                    message: format!(
                        "no ECU behavior claims `{}`; its message set is not one this build knows",
                        definition.signature
                    ),
                },
            },
        );
        return Outcome::Continue;
    };

    // A definition on its own has no project images; the connection keeps its
    // own ECU RAM image so a read is still meaningful.
    let standalone = match session {
        Session::Definition(_) => Some(Box::new(Tune::from_definition(definition))),
        Session::Project(_) | Session::Empty => None,
    };

    let link = port.clone();
    let effective_framing = framing.unwrap_or_else(|| default_framing(definition));
    let plan = IoPlan::definition(
        Box::new(definition.clone()),
        behavior,
        framing,
        link.clone(),
    );

    match IoThread::start(&plan) {
        Ok((thread, replies)) => {
            let mut connection = Live::new(
                thread,
                replies,
                &link,
                definition,
                behavior,
                effective_framing,
                standalone,
            );
            connection.connection.probe = probe;
            if let Err(error) =
                connection.request(op, PendingKind::Connect, IoOp::Signature { timeout: None })
            {
                tracing::warn!(%op, %error, "the signature query could not be queued");
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::ConnectFailed,
                            message: error.to_string(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if !probe {
                report.connects += 1;
            }
            *live = Some(connection);
        }
        Err(error) => {
            tracing::warn!(%op, %error, "the connection could not start");
            report.failures += 1;
            send(
                events,
                Event::Failed {
                    op,
                    failure: Failure {
                        code: FailureCode::ConnectFailed,
                        message: error.to_string(),
                    },
                },
            );
        }
    }

    Outcome::Continue
}

/// The framing a definition asks for.
pub(super) fn default_framing(definition: &Definition) -> LinkMode {
    match definition.comms.envelope.as_deref() {
        Some(format) if format.eq_ignore_ascii_case("msEnvelope_1.0") => LinkMode::Envelope,
        _ => LinkMode::Legacy,
    }
}

/// Drains replies and runs the poll/reconnect timers.
#[expect(
    clippy::too_many_arguments,
    reason = "the handler mirrors the fields of one protocol request"
)]
pub(super) fn service_live(
    live: &mut Option<Live>,
    pending_create: &mut Option<super::create::PendingCreate>,
    session: &mut Session,
    safety: &mut Safety,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    latest: &LatestFrame,
    frames: &FrameNotify,
    history: &ChartHistory,
    log: &LogStatus,
    report: &mut CoreReport,
) {
    let Some(connection) = live.as_mut() else {
        return;
    };

    while let Ok(reply) = connection.connection.replies.try_recv() {
        let keep = on_reply(
            connection,
            reply,
            pending_create,
            session,
            safety,
            events,
            latest,
            frames,
            history,
            log,
            report,
        );
        if !keep {
            teardown(
                live,
                session,
                safety,
                journal_dir,
                latest,
                events,
                history,
                log,
            );
            return;
        }
    }

    let Some(connection) = live.as_mut() else {
        return;
    };

    if connection.reconnect_due()
        && !connection.pending_kind(PendingKind::Reconnect)
        && let Err(error) =
            connection.request(OpId::new(0), PendingKind::Reconnect, IoOp::Reconnect)
    {
        tracing::debug!(%error, "the reconnect could not be queued");
        schedule_reconnect(connection);
    }

    // A burn that the firmware queued needs its own samples; the realtime
    // poll stays off while a transaction owns the link.
    if connection.burn_due() {
        let op = connection
            .txns
            .write_tx
            .as_ref()
            .map(|transaction| transaction.op);
        match op {
            Some(op) if !connection.pending_kind(PendingKind::Write) => {
                if let Some(wait) = connection.txns.burn_wait.as_mut() {
                    wait.next_at = Instant::now() + BURN_POLL_INTERVAL;
                }
                if let Err(error) = connection.request(op, PendingKind::Write, IoOp::ReadRealtime) {
                    let Some(transaction) = connection.txns.write_tx.take() else {
                        return;
                    };
                    connection.txns.burn_wait = None;
                    fail_transaction(
                        connection,
                        transaction,
                        session,
                        safety,
                        events,
                        report,
                        error.to_string(),
                        false,
                    );
                }
            }
            Some(_) => {}
            None => connection.txns.burn_wait = None,
        }
    }

    if connection.poll_due()
        && !connection.pending_kind(PendingKind::Realtime)
        && !connection.pending_kind(PendingKind::ReadAll)
        && !connection.pending_kind(PendingKind::Write)
        && !connection.pending_kind(PendingKind::Command)
        && !connection.pending_kind(PendingKind::Reconnect)
        && connection.txns.write_tx.is_none()
        && connection.txns.command_tx.is_none()
    {
        if let Err(error) =
            connection.request(OpId::new(0), PendingKind::Realtime, IoOp::ReadRealtime)
        {
            tracing::debug!(%error, "the realtime poll could not be queued");
            connection.stop_polling();
        } else {
            connection.polled();
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the reply handler mirrors the fields of one protocol request"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the reply handler is one exhaustive match over every I/O outcome"
)]
pub(super) fn on_reply(
    live: &mut Live,
    reply: IoReply,
    pending_create: &mut Option<super::create::PendingCreate>,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    latest: &LatestFrame,
    frames: &FrameNotify,
    history: &ChartHistory,
    log: &LogStatus,
    report: &mut CoreReport,
) -> bool {
    let Some(pending) = live.requests.pending.remove(&reply.id) else {
        tracing::debug!(id = reply.id, "an I/O reply had no pending request");
        return true;
    };

    match (pending.kind, reply.outcome) {
        (PendingKind::CreateSignature, IoOutcome::Signature { signature }) => {
            let Some(create) = pending_create.as_mut() else {
                tracing::error!("a create signature reply had no pending create");
                return false;
            };
            if let Err(message) = super::create::signature_reached(create, live, signature) {
                super::create::fail(pending_create, &message, events, report);
                return false;
            }
            true
        }
        (PendingKind::CreateVersion, IoOutcome::Version { text }) => {
            let Some(create) = pending_create.as_mut() else {
                tracing::error!("a create version reply had no pending create");
                return false;
            };
            if let Err(message) = super::create::version_reached(create, live, Some(text)) {
                super::create::fail(pending_create, &message, events, report);
                return false;
            }
            true
        }
        (PendingKind::CreateVersion, IoOutcome::Failed(failure)) => {
            tracing::warn!(
                kind = failure.kind.as_str(),
                %failure.message,
                "the create version query failed; the signature remains the firmware identity"
            );
            let Some(create) = pending_create.as_mut() else {
                tracing::error!("a create version failure had no pending create");
                return false;
            };
            if let Err(message) = super::create::version_reached(create, live, None) {
                super::create::fail(pending_create, &message, events, report);
                return false;
            }
            true
        }
        (PendingKind::CreateVersion, outcome) => {
            tracing::warn!(?outcome, "the create version query got an unexpected reply");
            let Some(create) = pending_create.as_mut() else {
                return false;
            };
            if let Err(message) = super::create::version_reached(create, live, None) {
                super::create::fail(pending_create, &message, events, report);
                return false;
            }
            true
        }
        (PendingKind::CreateRead, IoOutcome::All { pages }) => {
            let Some(create) = pending_create.as_mut() else {
                tracing::error!("a create page reply had no pending create");
                return false;
            };
            if let Err(message) = super::create::pages_reached(create, live, &pages) {
                super::create::fail(pending_create, &message, events, report);
                return false;
            }
            true
        }
        (PendingKind::CreateSignature | PendingKind::CreateRead, IoOutcome::Failed(failure)) => {
            super::create::fail(pending_create, &failure.message, events, report);
            false
        }
        (PendingKind::CreateSignature | PendingKind::CreateRead, outcome) => {
            let message = format!("the create read received an unexpected reply: {outcome:?}");
            super::create::fail(pending_create, &message, events, report);
            false
        }
        (PendingKind::Connect, IoOutcome::Signature { signature }) => {
            live.set_signature(signature);
            let summary = live.summary(safety.writes_enabled());
            if live.connection.probe {
                tracing::info!(
                    port = %summary.port,
                    signature = %summary.signature,
                    verdict = summary.verdict.as_str(),
                    "ECU test connection"
                );
                send(
                    events,
                    Event::ConnectionTested {
                        op: pending.op,
                        view: summary,
                    },
                );
                // A probe is not a session: its port is released at once.
                return false;
            }

            // The version text is separate metadata, not the signature. A
            // controller that does not answer still gets a usable session.
            if let Some(command) = live.connection.version_command.clone() {
                if let Err(error) = live.request(
                    pending.op,
                    PendingKind::ConnectVersion,
                    IoOp::Version { command },
                ) {
                    tracing::warn!(%error, "the version query could not be queued");
                    announce_connection(live, pending.op, session, events, safety);
                }
            } else {
                announce_connection(live, pending.op, session, events, safety);
            }
            true
        }
        (PendingKind::ConnectVersion, IoOutcome::Version { text }) => {
            if !text.trim().is_empty() {
                live.connection.version_text = Some(text);
            }
            announce_connection(live, pending.op, session, events, safety);
            true
        }
        (PendingKind::ConnectVersion, IoOutcome::Failed(failure)) => {
            if failure.kind == IoFailureKind::LinkLost {
                connection_lost(live, session, events, latest, failure.message);
                return true;
            }
            // Optional metadata: report it and keep the session.
            tracing::warn!(
                kind = failure.kind.as_str(),
                %failure.message,
                "the firmware version query failed; the signature still identifies the session"
            );
            announce_connection(live, pending.op, session, events, safety);
            true
        }
        (PendingKind::ConnectVersion, outcome) => {
            tracing::warn!(?outcome, "the version query got an unexpected reply");
            announce_connection(live, pending.op, session, events, safety);
            true
        }
        (PendingKind::Connect, IoOutcome::Failed(failure)) => {
            let probe = live.connection.probe;
            tracing::warn!(
                kind = failure.kind.as_str(),
                %failure.message,
                probe,
                "connect failed"
            );
            report.failures += 1;
            send(
                events,
                Event::Failed {
                    op: pending.op,
                    failure: Failure {
                        code: if probe {
                            FailureCode::TestFailed
                        } else {
                            FailureCode::ConnectFailed
                        },
                        message: failure.message,
                    },
                },
            );
            false
        }
        (PendingKind::Disconnect, _) => {
            tracing::info!("ECU disconnected");
            stop_for_disconnect(live);
            session.mark_unknown();
            latest.clear();
            send(events, Event::Disconnected { op: pending.op });
            false
        }
        (PendingKind::ReadAll, IoOutcome::All { pages }) => {
            match commit_pages(session, live, &pages) {
                Ok(summary) => {
                    tracing::info!(
                        pages = summary.pages,
                        bytes = summary.bytes,
                        constants = summary.constants,
                        skipped = summary.skipped,
                        "pages read"
                    );
                    report.page_reads += 1;
                    live.images.page_bytes = pages
                        .iter()
                        .map(|read| (read.page, read.bytes.clone()))
                        .collect();
                    live.images.pages_complete = summary.is_complete();
                    // The documented connect flow is signature → read all pages
                    // → start the poller. Starting it earlier would interleave
                    // realtime replies with the page read on the wire.
                    if live.connection.auto_poll
                        && live.connection.realtime_block.is_some()
                        && live.timers.poll.is_none()
                    {
                        live.start_polling(
                            u32::try_from(DEFAULT_POLL_INTERVAL.as_millis()).unwrap_or(100),
                        );
                    }
                    send(
                        events,
                        Event::PagesRead {
                            op: pending.op,
                            report: summary,
                        },
                    );
                }
                Err(message) => {
                    tracing::warn!(%message, "the page read could not be applied");
                    report.failures += 1;
                    send(
                        events,
                        Event::Failed {
                            op: pending.op,
                            failure: Failure {
                                code: FailureCode::ReadFailed,
                                message,
                            },
                        },
                    );
                }
            }
            true
        }
        (PendingKind::Realtime, IoOutcome::Realtime { block }) => {
            let sequence = live.telemetry.sequence;
            live.telemetry.sequence = sequence.wrapping_add(1);
            let mut frame = live.telemetry.decoder.decode(&block, sequence);
            if frame.is_empty() {
                return true;
            }
            // Indicators read the same decoded frame and the tune the user is
            // editing, so the strip and Telemetry cannot disagree.
            let tune = match session {
                Session::Project(project) => Some(project.tune()),
                Session::Definition(_) => live.images.standalone.as_deref(),
                Session::Empty => None,
            };
            let indicators = live.telemetry.decoder.indicators(&frame, tune);
            frame.indicators = indicators;
            let first = *live.telemetry.first_frame.get_or_insert_with(Instant::now);
            let time = first.elapsed().as_secs_f64();
            live.telemetry.last_frame_time = time;
            history.push_frame_iter(
                time,
                frame
                    .values
                    .iter()
                    .map(|value| (value.name.as_str(), value.value)),
            );
            if let Some(logger) = &live.logging.logger {
                let values = align_log_values(&live.logging.log_columns, &frame);
                if !logger.try_log(time, &values) {
                    report.log_drops = report.log_drops.saturating_add(1);
                }
            }
            latest.set(frame);
            // A wake, not an event: acquisition must never wait for the UI
            // One pending token covers every frame that arrived
            // since the last repaint.
            frames.wake();
            report.realtime_frames += 1;
            true
        }
        (PendingKind::Write, outcome) => {
            advance_write(
                live, pending.op, outcome, session, safety, events, latest, history, log, report,
            );
            true
        }
        (PendingKind::Command, IoOutcome::Command { report: sent }) => {
            let Some(transaction) = live.txns.command_tx.take() else {
                tracing::warn!(op = %pending.op, "a controller command reply had no transaction");
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op: pending.op,
                        failure: Failure {
                            code: FailureCode::ControllerActionFailed,
                            message: "the controller command lost its transaction record"
                                .to_owned(),
                        },
                    },
                );
                return true;
            };

            record_command_result(&transaction, live, "ok", "sent");
            // A raw controller action bypasses memory synchronization, so the
            // ECU side is no longer the image the last read confirmed.
            session.mark_unknown();
            live.polled();

            let summary = ControllerActionReport {
                dialog: transaction.dialog,
                command: transaction.name,
                commands: sent.commands,
                bytes: sent.bytes,
                replies: sent.replies,
            };
            tracing::info!(
                op = %pending.op,
                command = %summary.command,
                commands = summary.commands,
                bytes = summary.bytes,
                "controller command sent"
            );
            send(
                events,
                Event::ControllerActionRan {
                    op: pending.op,
                    report: summary,
                },
            );
            true
        }
        (PendingKind::Trace, IoOutcome::TraceSaved { path, frames }) => {
            tracing::info!(path = %path.display(), frames, "session trace saved");
            send(
                events,
                Event::TraceSaved {
                    op: pending.op,
                    path,
                    frames,
                },
            );
            true
        }
        (PendingKind::Reconnect, IoOutcome::Reconnected { signature }) => {
            live.set_signature(signature);
            let summary = live.summary(safety.writes_enabled());
            tracing::info!(
                signature = %summary.signature,
                verdict = summary.verdict.as_str(),
                "ECU reconnected"
            );
            send(events, Event::Reconnected { view: summary });
            true
        }
        (PendingKind::Reconnect, IoOutcome::Failed(failure)) => {
            tracing::debug!(kind = failure.kind.as_str(), "a reconnect attempt failed");
            schedule_reconnect(live);
            true
        }
        (kind, IoOutcome::Failed(failure)) => {
            let code = failure_code(kind);
            tracing::warn!(
                kind = failure.kind.as_str(),
                operation = ?kind,
                %failure.message,
                "a wire operation failed"
            );
            if kind == PendingKind::Command {
                finish_command_failed(live, &failure.message);
            }
            if kind != PendingKind::Realtime {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op: pending.op,
                        failure: Failure {
                            code,
                            message: failure.message.clone(),
                        },
                    },
                );
            }

            match failure.kind {
                IoFailureKind::LinkLost | IoFailureKind::Timeout
                    if kind != PendingKind::Connect =>
                {
                    connection_lost(live, session, events, latest, failure.message);
                }
                IoFailureKind::Protocol | IoFailureKind::Unsupported
                    if kind == PendingKind::Realtime =>
                {
                    // The definition's realtime command does not work on this
                    // firmware; stop asking, keep the session.
                    live.stop_polling();
                }
                _ => {}
            }
            true
        }
        (kind, outcome) => {
            tracing::debug!(?kind, ?outcome, "an I/O reply did not match its request");
            true
        }
    }
}

/// Sends the session summary once the handshake and the optional version query
/// are done.
fn announce_connection(
    live: &mut Live,
    op: OpId,
    session: &mut Session,
    events: &EventTx,
    safety: &Safety,
) {
    let summary = live.summary(safety.writes_enabled());
    tracing::info!(
        port = %summary.port,
        signature = %summary.signature,
        version = summary.version_text.as_deref().unwrap_or(""),
        verdict = summary.verdict.as_str(),
        read_only = summary.read_only,
        "ECU connected"
    );
    send(events, Event::Connected { op, view: summary });

    // A connect that answered is the one worth reopening next time;
    // remembering it in the project keeps each project on its own ECU. A
    // reconnect to the same port has nothing to persist, so the state file is
    // left alone.
    if let Session::Project(project) = session
        && let Some(saved) = &live.connection.saved_connection
        && {
            let connection = &project.state().connection;
            connection.last_port.as_deref() != Some(saved.port.as_str())
                || connection.baud_override != Some(saved.baud)
        }
        && let Err(error) = project.set_connection(Some(saved.port.clone()), Some(saved.baud))
    {
        tracing::warn!(%error, "the project device could not be persisted");
    }
}

/// Turns a pending kind into the failure code its UI switches on.
pub(super) fn failure_code(kind: PendingKind) -> FailureCode {
    match kind {
        PendingKind::Connect | PendingKind::ConnectVersion => FailureCode::ConnectFailed,
        PendingKind::CreateSignature | PendingKind::CreateRead | PendingKind::CreateVersion => {
            FailureCode::ProjectRejected
        }
        PendingKind::ReadAll => FailureCode::ReadFailed,
        PendingKind::Realtime => FailureCode::RealtimeRejected,
        PendingKind::Trace => FailureCode::TraceFailed,
        PendingKind::Write => FailureCode::WriteFailed,
        PendingKind::Command => FailureCode::ControllerActionFailed,
        PendingKind::Disconnect | PendingKind::Reconnect => FailureCode::NotConnected,
    }
}

/// Marks the session as lost and schedules a reconnect.
pub(super) fn connection_lost(
    live: &mut Live,
    session: &mut Session,
    events: &EventTx,
    latest: &LatestFrame,
    reason: String,
) {
    tracing::warn!(%reason, "the ECU link was lost");
    live.connection.connected = false;
    live.connection.revision = live.connection.revision.wrapping_add(1);
    live.stop_polling();
    session.mark_unknown();
    latest.clear();
    // A lost link ends the log's input; close it cleanly rather than leave an
    // open file that grows only when a reconnect happens.
    if live.logging.logger.take().is_some() {
        tracing::info!("the session log was closed because the link was lost");
    }

    finish_command_failed(live, &format!("the link was lost: {reason}"));
    // Every conversation that was waiting is answered now: a vanished link
    // must not leave a command unanswered.
    for pending in std::mem::take(&mut live.requests.pending).into_values() {
        if pending.kind == PendingKind::Realtime || pending.kind == PendingKind::Reconnect {
            continue;
        }
        send(
            events,
            Event::Failed {
                op: pending.op,
                failure: Failure {
                    code: failure_code(pending.kind),
                    message: format!("the link was lost: {reason}"),
                },
            },
        );
    }

    schedule_reconnect(live);
    send(events, Event::ConnectionLost { reason });
}

/// Schedules the next reconnect attempt with exponential backoff.
pub(super) fn schedule_reconnect(live: &mut Live) {
    let attempts = live
        .timers
        .reconnect
        .map_or(0, |reconnect| reconnect.attempts.saturating_add(1));
    // First retry after RECONNECT_BASE, then doubling: 500 ms, 1 s, 2 s, 4 s, 5 s.
    let delay = RECONNECT_BASE
        .saturating_mul(1u32 << attempts.saturating_sub(1).min(4))
        .min(RECONNECT_MAX);
    live.timers.reconnect = Some(Reconnect {
        attempts,
        next_at: Instant::now() + delay,
    });
}

/// Stops polling without touching the port (disconnect path).
pub(super) fn stop_for_disconnect(live: &mut Live) {
    live.stop_polling();
    live.timers.reconnect = None;
    live.connection.connected = false;
    live.connection.revision = live.connection.revision.wrapping_add(1);
}

/// Applies a complete page read to the session's ECU image.
fn conflict_report(conflict: Conflict) -> ConflictReport {
    ConflictReport {
        name: conflict.name,
        row: conflict.row,
        column: conflict.column,
        working: cell_value_text(conflict.working),
        ecu: cell_value_text(conflict.ecu),
    }
}

fn cell_value_text(value: CellValue) -> String {
    match value {
        CellValue::Number(value) => format!("{value}"),
        CellValue::Raw(value) => format!("{value} (raw)"),
        CellValue::Text(text) => text,
    }
}

pub(super) fn commit_pages(
    session: &mut Session,
    live: &mut Live,
    pages: &[PageRead],
) -> Result<ReadReport, String> {
    let mut summary = ReadReport {
        pages: 0,
        bytes: 0,
        constants: 0,
        skipped: 0,
        signature_verified: live.connection.verdict == SignatureVerdict::Match,
        conflicts: Vec::new(),
    };

    match session {
        Session::Project(project) => {
            let layout = Arc::clone(project.layout());
            let mut ram = project.tune().snapshot();
            for read in pages {
                let page = IniPage::new(read.page)
                    .ok_or_else(|| format!("page {} is not a valid page number", read.page))?;
                let report = decode_page(&layout, page, &read.bytes, &mut ram)
                    .map_err(|error| error.to_string())?;
                summary.pages += 1;
                summary.bytes += read.bytes.len();
                summary.constants += report.decoded;
                summary.skipped += report.skipped;
            }
            // A first read and a later one both compare the ECU with the
            // working image: the decision belongs to the user, not to whether
            // the link is new (ADR-0010).
            let conflicts = project
                .images_mut()
                .reconcile_read(ram)
                .map_err(|error| error.to_string())?;
            summary.conflicts = conflicts.into_iter().map(conflict_report).collect();
        }
        Session::Definition(_) => {
            let tune = live
                .images
                .standalone
                .as_mut()
                .ok_or_else(|| "the connection has no ECU image".to_owned())?;
            let layout = Arc::clone(tune.layout());
            for read in pages {
                let page = IniPage::new(read.page)
                    .ok_or_else(|| format!("page {} is not a valid page number", read.page))?;
                let report = decode_page(&layout, page, &read.bytes, tune)
                    .map_err(|error| error.to_string())?;
                summary.pages += 1;
                summary.bytes += read.bytes.len();
                summary.constants += report.decoded;
                summary.skipped += report.skipped;
            }
        }
        Session::Empty => return Err("no definition is loaded".to_owned()),
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::LiveValue;

    #[test]
    fn the_default_queues_are_bounded() {
        let config = CoreConfig::default();

        assert!(config.command_capacity > 0);
        assert!(
            config.event_capacity >= config.command_capacity + 2,
            "a full accepted burst (one answer or rejection each) plus `Stopped` must fit"
        );
    }

    #[test]
    fn a_report_with_no_work_has_no_mean_latency() {
        let report = CoreReport::default();

        assert_eq!(report.mean_op_latency(), Duration::ZERO);
        assert_eq!(report.handled, 0);
    }

    #[test]
    fn a_report_divides_by_the_handled_count() {
        let report = CoreReport {
            handled: 4,
            total_op_latency: Duration::from_millis(10),
            ..CoreReport::default()
        };

        assert_eq!(report.mean_op_latency(), Duration::from_micros(2500));
    }

    fn live(name: &str, value: f64) -> LiveValue {
        LiveValue {
            name: name.to_owned(),
            value,
            text: String::new(),
            units: String::new(),
        }
    }

    #[test]
    fn a_log_row_keeps_column_order_when_a_channel_is_missing() {
        let columns: Vec<String> = ["rpm", "map", "tps"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();

        // A complete frame is in definition order and copies straight across.
        let full = RealtimeFrame {
            sequence: 0,
            values: vec![live("rpm", 900.0), live("map", 45.0), live("tps", 3.0)],
            indicators: Vec::new(),
            context: HashMap::new(),
        };
        assert_eq!(align_log_values(&columns, &full), [900.0, 45.0, 3.0]);

        // A channel the decoder could not read becomes a gap, not a shift.
        let short = RealtimeFrame {
            sequence: 1,
            values: vec![live("rpm", 900.0), live("tps", 3.0)],
            indicators: Vec::new(),
            context: HashMap::new(),
        };
        let row = align_log_values(&columns, &short);
        assert_eq!(row.len(), columns.len());
        assert_eq!(row[0], 900.0);
        assert!(row[1].is_nan(), "a missing channel is a gap");
        assert_eq!(row[2], 3.0);
    }
}
