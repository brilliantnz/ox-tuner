//! The core thread: one owner of the loaded definition, one command loop.
//!
//! ```text
//! UI ──Command──▶ [commands queue] ──▶ core thread ──▶ [events queue] ──Event──▶ UI
//!                                       owns the session
//! ```
//!
//! What this thread owns is the **session** — nothing, a standalone definition,
//! or an open project (definition + tune images) — and the live ECU session's
//! state machine: the I/O thread handle, pending conversations, the poll and
//! reconnect timers, and the last read summary. The write path and the datalog
//! ring buffer arrive with M7/M8 and land **here**, not in the UI — that is the
//! point of the crate (`docs/architecture/crates/oxtuner-core.md`). Nothing in
//! this module is a stub for them.

use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, atomic::AtomicU64},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossfire::{MRx, MTx, RecvTimeoutError, Rx, mpmc, spsc};
use oxtuner_ini::Definition;
use oxtuner_project::Project;
use oxtuner_protocol::{Behavior, LinkMode};
use oxtuner_tune::{IniPage, Tune, decode_page};

use crate::handle::{CoreHandle, EventStream, Gate};
use crate::io_thread::{
    self, IoFailureKind, IoOp, IoOutcome, IoPlan, IoReply, IoThread, LinkChoice, PageRead,
    RequestId,
};
use crate::message::{
    AxisEdit, CellEdit, Command, ConnectionSummary, DefinitionSummary, DialogSummary, Envelope,
    Event, Failure, FailureCode, OpId, PortChoice, PortSummary, ProjectSummary, ReadAllSummary,
    SaveSummary, SignatureVerdict, TableSummary,
};
use crate::realtime::{LatestFrame, RealtimeDecoder};

/// Queue sizes and thread behaviour.
#[derive(Debug, Clone, Copy)]
pub struct CoreConfig {
    /// Commands that may wait for the core. Small on purpose: a deep queue
    /// only hides a stalled core, and every queued command has to be answered
    /// at shutdown.
    pub command_capacity: usize,
    /// Events that may wait for the UI. Generous: dropping an event would break
    /// the "answered exactly once" guarantee. Keep at least
    /// `command_capacity + 2`, so one answer (or one [`Event::Rejected`]) per
    /// accepted command plus [`Event::Stopped`] fits without the UI consuming —
    /// otherwise the shutdown drain could block on the queue.
    pub event_capacity: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            command_capacity: 64,
            event_capacity: 1024,
        }
    }
}

/// A command queue that cannot take the command right now.
#[derive(Debug)]
pub enum CoreError {
    /// The command was refused before entering the core; it will not be
    /// answered and its `OpId` is never used.
    QueueFull,
    /// The core thread has stopped.
    Stopped,
    /// The thread could not be created.
    Spawn(io::Error),
    /// A connection could not be built (no message set, a bad definition).
    Connect(String),
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => formatter.write_str("the core is busy; the command was refused"),
            Self::Stopped => formatter.write_str("the core has stopped"),
            Self::Spawn(error) => write!(formatter, "could not start the core thread: {error}"),
            Self::Connect(message) => write!(formatter, "could not connect: {message}"),
        }
    }
}

impl std::error::Error for CoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::QueueFull | Self::Stopped | Self::Connect(_) => None,
        }
    }
}

/// What the core did before it stopped.
///
/// Published by joining the thread, which is also how shutdown becomes
/// observable: the report cannot exist while the core still runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoreReport {
    /// Commands handled to completion.
    pub handled: u64,
    /// Commands answered [`Event::Rejected`] because the core was stopping.
    pub rejected: u64,
    /// Definitions loaded successfully.
    pub loads: u64,
    /// Projects opened successfully.
    pub projects_opened: u64,
    /// Tune files saved successfully.
    pub tune_saves: u64,
    /// Commands that failed (a rejected definition, project or save).
    pub failures: u64,
    /// Connections opened.
    pub connects: u64,
    /// Page reads that completed.
    pub page_reads: u64,
    /// Realtime frames decoded.
    pub realtime_frames: u64,
    /// Slowest command, measured from dequeue to reply.
    pub max_op_latency: Duration,
    /// Sum of command durations, for the mean.
    pub total_op_latency: Duration,
    /// How long the thread ran.
    pub uptime: Duration,
}

impl CoreReport {
    /// Mean command duration, or zero when nothing ran.
    pub fn mean_op_latency(&self) -> Duration {
        if self.handled == 0 {
            return Duration::ZERO;
        }
        self.total_op_latency / u32::try_from(self.handled).unwrap_or(u32::MAX)
    }
}

/// How long shutdown may wait for room in the command queue.
///
/// The caller is on its way out, so a short block is better than a lost
/// shutdown; a longer one would make quitting feel stuck.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// How long a refused shutdown still waits for the thread before abandoning it.
///
/// The shutdown command never reached the core, so the thread is wedged (or
/// slower than [`SHUTDOWN_WAIT`] to finish its current command). Quitting must
/// stay bounded — an abandoned thread dies with the process, and the abandon is
/// reported as an error, never silently.
const JOIN_WAIT: Duration = Duration::from_secs(5);

/// How often the abandoned-thread wait checks whether the core has finished.
const JOIN_POLL: Duration = Duration::from_millis(10);

/// Queues a [`Command::Shutdown`], waiting up to [`SHUTDOWN_WAIT`] for room.
///
/// Returns whether the core will see the command (or is already gone).
pub(crate) fn send_shutdown(
    commands: &MTx<mpmc::Array<Envelope>>,
    op: OpId,
) -> Result<(), CoreError> {
    match commands.send_timeout(
        Envelope {
            op,
            command: Command::Shutdown,
        },
        SHUTDOWN_WAIT,
    ) {
        Ok(()) => Ok(()),
        Err(crossfire::SendTimeoutError::Disconnected(_)) => Err(CoreError::Stopped),
        Err(crossfire::SendTimeoutError::Timeout(_)) => Err(CoreError::QueueFull),
    }
}

/// The three pieces of a running core.
///
/// They are handed out separately on purpose: the events stream belongs to
/// whichever task awaits it, the handle can be cloned into anything, and the
/// thread must be joined (or dropped) to shut down.
pub struct CoreParts {
    /// Commands in.
    pub handle: CoreHandle,
    /// Events out. Single consumer by construction.
    pub events: EventStream,
    /// The newest realtime frame, read without an event round trip (ADR-0014).
    pub latest: LatestFrame,
    /// The thread, which joins on shutdown.
    pub thread: CoreThread,
}

/// Owns the core thread and joins it on drop.
///
/// Dropping is a safe shutdown: it queues [`Command::Shutdown`] and joins the
/// thread, so a forgotten core is stopped and reaped, and [`CoreThread::join`]
/// returns its report. One policy is explicit rather than pretty: a core that
/// refuses shutdown is wedged, and a wedged thread is abandoned — loudly, after
/// [`SHUTDOWN_WAIT`] + [`JOIN_WAIT`] — instead of hanging the caller forever.
/// Nothing is ever detached silently.
pub struct CoreThread {
    join: Option<JoinHandle<CoreReport>>,
    commands: MTx<mpmc::Array<Envelope>>,
}

impl CoreThread {
    /// Stops the core and returns its report.
    pub fn join(mut self) -> CoreReport {
        self.finish()
    }

    /// Whether the thread has already stopped.
    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn finish(&mut self) -> CoreReport {
        let Some(join) = self.join.take() else {
            return CoreReport::default();
        };
        match send_shutdown(&self.commands, OpId::new(0)) {
            // The core will stop; the join below is safe to wait on, because
            // the loop is guaranteed to end once `Shutdown` is handled. Its one
            // dependency is a live consumer for the final events (`Stopped`
            // among them) — oxtuner-app's event pump drains until then.
            Ok(()) | Err(CoreError::Stopped) => {}
            // The queue stayed full, so `Shutdown` never reached the core and
            // nothing else can be queued either. The thread is wedged or slower
            // than the wait to finish its current command: give it a bounded
            // moment, then abandon it — loudly and explicitly. Quitting must
            // not hang, and nothing here detaches silently.
            Err(error) => {
                tracing::warn!(%error, "the core did not accept a shutdown command");
                let deadline = Instant::now() + JOIN_WAIT;
                while !join.is_finished() && Instant::now() < deadline {
                    thread::sleep(JOIN_POLL);
                }
                if !join.is_finished() {
                    tracing::error!(
                        "the core thread is still running and was abandoned; it dies with the \
                         process"
                    );
                    return CoreReport::default();
                }
            }
        }
        join.join().unwrap_or_else(|payload| {
            tracing::error!("the core thread panicked: {}", panic_message(&payload));
            CoreReport::default()
        })
    }
}

impl Drop for CoreThread {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Starts the core thread.
pub fn start(config: CoreConfig) -> Result<CoreParts, CoreError> {
    let (commands, command_rx) = mpmc::bounded_blocking(config.command_capacity.max(1));
    let (event_tx, events) = mpmc::bounded_blocking_async(config.event_capacity.max(1));
    let next_op = Arc::new(AtomicU64::new(OpId::FIRST));
    let gate = Arc::new(Gate::default());
    let core_gate = Arc::clone(&gate);
    let latest = LatestFrame::default();
    let core_latest = latest.clone();

    let join = thread::Builder::new()
        .name("oxtuner-core".to_owned())
        .spawn(move || run(command_rx, event_tx, core_gate, core_latest))
        .map_err(CoreError::Spawn)?;

    tracing::debug!("core thread started");

    Ok(CoreParts {
        handle: CoreHandle {
            commands: commands.clone(),
            next_op,
            gate,
        },
        events: EventStream { events },
        latest,
        thread: CoreThread {
            join: Some(join),
            commands,
        },
    })
}

/// The command loop.
///
/// Blocks on the queue: with no poller yet there is nothing to do while idle,
/// and a timed wait would only wake the thread for nothing.
fn run(
    commands: MRx<mpmc::Array<Envelope>>,
    events: MTx<mpmc::Array<Event>>,
    gate: Arc<Gate>,
    latest: LatestFrame,
) -> CoreReport {
    let started = Instant::now();
    let mut report = CoreReport::default();
    let mut session = Session::Empty;
    let mut live: Option<Live> = None;

    // Every handle is gone (`Err`) also ends the loop, which is what makes a
    // dropped UI a shutdown rather than a leak.
    loop {
        let envelope = match next_wait(&live) {
            Some(timeout) => match commands.recv_timeout(timeout) {
                Ok(envelope) => Some(envelope),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match commands.recv() {
                Ok(envelope) => Some(envelope),
                Err(_) => break,
            },
        };

        if let Some(envelope) = envelope {
            let op_started = Instant::now();
            let outcome = handle(
                envelope,
                &mut session,
                &mut live,
                &events,
                &latest,
                &mut report,
            );
            let took = op_started.elapsed();

            report.total_op_latency += took;
            report.max_op_latency = report.max_op_latency.max(took);

            if outcome == Outcome::Stop {
                break;
            }
            report.handled += 1;
        }

        // Replies and timers are serviced after every command and every wait,
        // so a poll or a reconnect never depends on another command arriving.
        service_live(&mut live, &mut session, &events, &latest, &mut report);
    }

    teardown(&mut live, &latest, &events);

    // Close the door first: from here on no command can be queued, so draining
    // below really is the last word. Without this, a send racing the shutdown
    // could be accepted and never answered.
    gate.close();

    // Commands that were accepted while shutdown was racing them are answered
    // explicitly — ADR-0005 rule 5: cancellation is never implicit.
    while let Ok(envelope) = commands.try_recv() {
        if matches!(envelope.command, Command::Shutdown) {
            continue;
        }
        report.rejected += 1;
        send(
            &events,
            Event::Rejected {
                op: envelope.op,
                reason: crate::message::RejectReason::ShuttingDown,
            },
        );
    }

    // Dropping the receiver makes any later `try_send` fail with `Disconnected`,
    // which is the second half of the same guarantee.
    drop(commands);
    send(&events, Event::Stopped);
    report.uptime = started.elapsed();
    tracing::debug!(
        handled = report.handled,
        rejected = report.rejected,
        uptime_ms = report.uptime.as_millis(),
        "core thread stopped"
    );
    report
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Continue,
    Stop,
}

/// What the core owns between commands.
///
/// One enum rather than separate options: a definition and a project cannot both
/// be open, so the "one owner" rule is a property of the type.
#[derive(Debug)]
enum Session {
    /// Nothing loaded.
    Empty,
    /// A definition opened on its own (no tune file).
    ///
    /// Owned for the protocol layer: a connection encodes its commands from
    /// this definition, and a definition-only session keeps the ECU RAM image
    /// here.
    Definition(Box<Definition>),
    /// A TunerStudio project: definition, tune images and the file they came
    /// from.
    ///
    /// Boxed: a `Project` is a few kilobytes of tune images, and the session
    /// lives on the core thread's stack for the whole run — it must not inflate
    /// the enum every command frame touches.
    Project(Box<Project>),
}

impl Session {
    /// The definition the session owns, whichever shape it has.
    fn definition(&self) -> Option<&Definition> {
        match self {
            Self::Empty => None,
            Self::Definition(definition) => Some(definition),
            Self::Project(project) => Some(project.definition()),
        }
    }

    /// Everything unproven on the ECU side becomes unknown (ADR-0010).
    fn mark_unknown(&mut self) {
        if let Self::Project(project) = self {
            project.images_mut().mark_unknown();
        }
    }
}

fn handle(
    envelope: Envelope,
    session: &mut Session,
    live: &mut Option<Live>,
    events: &MTx<mpmc::Array<Event>>,
    latest: &LatestFrame,
    report: &mut CoreReport,
) -> Outcome {
    let Envelope { op, command } = envelope;

    match command {
        Command::Shutdown => {
            tracing::debug!(%op, "shutdown requested");
            Outcome::Stop
        }
        Command::ListPorts => {
            match io_thread::list_ports() {
                Ok(ports) => {
                    let summary: Vec<PortSummary> = ports
                        .into_iter()
                        .map(|port| PortSummary {
                            label: port.label().to_owned(),
                            name: port.name,
                        })
                        .collect();
                    tracing::debug!(%op, ports = summary.len(), "serial ports listed");
                    send(events, Event::PortsListed { op, ports: summary });
                }
                Err(error) => {
                    tracing::warn!(%op, %error, "serial ports could not be listed");
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
        Command::Connect { port, framing } => {
            connect(op, session, live, events, report, port, framing, false)
        }
        Command::TestConnection { port, framing } => {
            connect(op, session, live, events, report, port, framing, true)
        }
        Command::Disconnect => {
            let Some(connection) = live.as_mut() else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "there is no connection to close".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };
            if let Err(error) = connection.request(op, PendingKind::Disconnect, IoOp::Close) {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: error.to_string(),
                        },
                    },
                );
            }
            Outcome::Continue
        }
        Command::ReadAll => {
            let Some(connection) = live.as_mut() else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "there is no connection to read from".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };
            if !connection.connected {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "the link is lost; waiting for it to come back".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if connection.pending_kind(PendingKind::ReadAll) {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::ReadFailed,
                            message: "a page read is already running".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if let Err(error) = connection.request(op, PendingKind::ReadAll, IoOp::ReadAll) {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::ReadFailed,
                            message: error.to_string(),
                        },
                    },
                );
            }
            Outcome::Continue
        }
        Command::StartRealtime { interval_ms } => {
            let Some(connection) = live.as_mut() else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::RealtimeRejected,
                            message: "there is no connection to poll".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };
            if !connection.connected {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::RealtimeRejected,
                            message: "the link is lost; waiting for it to come back".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if connection.realtime_block.is_none() {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::RealtimeRejected,
                            message: "the definition declares no realtime block".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            connection.auto_poll = true;
            connection.start_polling(interval_ms);
            send(events, Event::RealtimeState { op, running: true });
            Outcome::Continue
        }
        Command::StopRealtime => {
            let running = live.as_mut().is_some_and(|connection| {
                connection.auto_poll = false;
                connection.stop_polling();
                true
            });
            if running {
                send(events, Event::RealtimeState { op, running: false });
            } else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "there is no connection to stop".to_owned(),
                        },
                    },
                );
            }
            Outcome::Continue
        }
        Command::SaveTrace { path } => {
            let Some(connection) = live.as_mut() else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "there is no session trace to save".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };
            if !connection.connected {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TraceFailed,
                            message: "there is no session to save while the link is lost"
                                .to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if let Err(error) = connection.request(op, PendingKind::Trace, IoOp::SaveTrace { path })
            {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TraceFailed,
                            message: error.to_string(),
                        },
                    },
                );
            }
            Outcome::Continue
        }
        Command::LoadDefinition {
            label,
            bytes,
            settings,
        } => {
            match oxtuner_ini::parse_bytes(&bytes, &settings) {
                Ok(parsed) => {
                    let summary = DefinitionSummary::new(label.clone(), &parsed);
                    tracing::info!(
                        %op,
                        label = %summary.label,
                        signature = %summary.signature,
                        constants = summary.constants,
                        coverage = summary.coverage_percent,
                        "definition loaded"
                    );
                    // The core is the only owner: the previous session is
                    // dropped here, never handed back to a caller. A live
                    // connection belongs to the definition it was built from,
                    // so it is torn down first.
                    teardown(live, latest, events);
                    *session = Session::Definition(Box::new(parsed));
                    report.loads += 1;
                    send(events, Event::DefinitionLoaded { op, summary });
                }
                Err(error) => {
                    tracing::warn!(%op, %label, %error, "definition rejected");
                    report.failures += 1;
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::DefinitionRejected,
                                message: error.to_string(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        Command::OpenProject { path } => {
            // Reading and parsing happen here, on the core thread: the UI must
            // never wait on a project's INI and `.msq`.
            match Project::open(&path) {
                Ok(project) => {
                    let summary = ProjectSummary::from_project(&project);
                    tracing::info!(
                        %op,
                        root = %summary.root.display(),
                        signature = %summary.definition.signature,
                        loaded = summary.loaded,
                        skipped = summary.skipped,
                        warnings = summary.warnings.len(),
                        read_only = summary.read_only,
                        "project opened"
                    );
                    teardown(live, latest, events);
                    *session = Session::Project(Box::new(project));
                    report.projects_opened += 1;
                    send(events, Event::ProjectOpened { op, summary });
                }
                Err(error) => {
                    tracing::warn!(%op, path = %path.display(), %error, "project rejected");
                    report.failures += 1;
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::ProjectRejected,
                                message: error.to_string(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        Command::SaveTune => {
            match session {
                Session::Project(project) => match project.save() {
                    Ok(outcome) => {
                        tracing::info!(
                            %op,
                            path = %outcome.path.display(),
                            cells = outcome.cells,
                            "tune saved"
                        );
                        report.tune_saves += 1;
                        send(
                            events,
                            Event::TuneSaved {
                                op,
                                outcome: SaveSummary {
                                    path: outcome.path,
                                    cells: outcome.cells,
                                },
                            },
                        );
                    }
                    Err(error) => {
                        tracing::warn!(%op, %error, "tune save failed");
                        report.failures += 1;
                        send(
                            events,
                            Event::Failed {
                                op,
                                failure: Failure {
                                    code: FailureCode::TuneSaveFailed,
                                    message: error.to_string(),
                                },
                            },
                        );
                    }
                },
                Session::Definition(_) | Session::Empty => {
                    tracing::debug!(%op, "nothing to save");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::NothingToSave,
                                message: "no tune is open; open a project folder first".to_owned(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        Command::ReadTable { name } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableUnavailable,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            match TableSummary::from_project(project, &name) {
                Ok(table) => {
                    tracing::debug!(
                        %op,
                        table = %table.id,
                        rows = table.rows,
                        columns = table.cols,
                        "table read"
                    );
                    send(events, Event::TableRead { op, table });
                }
                Err(message) => {
                    tracing::debug!(%op, table = %name, %message, "table unavailable");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::TableUnavailable,
                                message,
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        Command::EditTable { name, edits } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            if let Err(message) = validate_edits(project, &name, &edits) {
                tracing::debug!(%op, table = %name, %message, "table edit rejected");
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message,
                        },
                    },
                );
                return Outcome::Continue;
            }

            for edit in &edits {
                // Validation above covers every error `set_cell_user` can
                // report, so a failure here would be a bug; report it rather
                // than swallow it, and keep the snapshot truthful.
                if let Err(error) =
                    project
                        .tune_mut()
                        .set_cell_user(&name, edit.row, edit.column, edit.value)
                {
                    tracing::error!(%op, table = %name, %error, "a validated edit failed");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::TableEditRejected,
                                message: error.to_string(),
                            },
                        },
                    );
                    return Outcome::Continue;
                }
            }

            match TableSummary::from_project(project, &name) {
                Ok(table) => {
                    tracing::debug!(
                        %op,
                        table = %table.id,
                        cells = edits.len(),
                        "table edited"
                    );
                    send(events, Event::TableEdited { op, table });
                }
                Err(message) => send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message,
                        },
                    },
                ),
            }
            Outcome::Continue
        }
        Command::EditAxis { table, axis, edits } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            let variable = match TableSummary::axis_variable(project, &table, axis) {
                Ok(variable) => variable,
                Err(message) => {
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::TableEditRejected,
                                message,
                            },
                        },
                    );
                    return Outcome::Continue;
                }
            };

            if let Err(message) = validate_axis_edits(project, &variable, &edits) {
                tracing::debug!(%op, table = %table, axis = axis.as_str(), %message, "axis edit rejected");
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message,
                        },
                    },
                );
                return Outcome::Continue;
            }

            for edit in &edits {
                if let Err(error) = project
                    .tune_mut()
                    .set_user_at(&variable, edit.index, edit.value)
                {
                    tracing::error!(%op, table = %table, %error, "a validated axis edit failed");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::TableEditRejected,
                                message: error.to_string(),
                            },
                        },
                    );
                    return Outcome::Continue;
                }
            }

            match TableSummary::from_project(project, &table) {
                Ok(table) => {
                    tracing::debug!(
                        %op,
                        table = %table.id,
                        axis = axis.as_str(),
                        bins = edits.len(),
                        "axis edited"
                    );
                    send(events, Event::TableEdited { op, table });
                }
                Err(message) => send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message,
                        },
                    },
                ),
            }
            Outcome::Continue
        }
        Command::ReadDialog { name } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogUnavailable,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            match DialogSummary::from_project(project, &name) {
                Ok(dialog) => {
                    tracing::debug!(
                        %op,
                        dialog = %dialog.id,
                        fields = dialog.fields.len(),
                        "dialog read"
                    );
                    send(events, Event::DialogRead { op, dialog });
                }
                Err(message) => {
                    tracing::debug!(%op, table = %name, %message, "dialog unavailable");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::DialogUnavailable,
                                message,
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        Command::EditScalar {
            dialog,
            name,
            value,
        } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogEditRejected,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            if let Err(message) = validate_scalar_edit(project, &name, value) {
                tracing::debug!(%op, variable = %name, %message, "scalar edit rejected");
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogEditRejected,
                            message,
                        },
                    },
                );
                return Outcome::Continue;
            }

            if let Err(error) = project.tune_mut().set_user_at(&name, 0, value) {
                tracing::error!(%op, variable = %name, %error, "a validated scalar edit failed");
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogEditRejected,
                            message: error.to_string(),
                        },
                    },
                );
                return Outcome::Continue;
            }

            match DialogSummary::from_project(project, &dialog) {
                Ok(dialog) => {
                    tracing::debug!(
                        %op,
                        dialog = %dialog.id,
                        variable = %name,
                        "scalar edited"
                    );
                    send(events, Event::DialogEdited { op, dialog });
                }
                Err(message) => send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogEditRejected,
                            message,
                        },
                    },
                ),
            }
            Outcome::Continue
        }
        Command::UnloadDefinition => {
            let had_session = !matches!(session, Session::Empty);
            teardown(live, latest, events);
            *session = Session::Empty;
            tracing::debug!(%op, had_session, "session unloaded");
            send(events, Event::DefinitionUnloaded { op });
            Outcome::Continue
        }
    }
}

/// Checks every edit before one is stored, so a rejected request leaves the tune
/// exactly as it was.
fn validate_edits(project: &Project, name: &str, edits: &[CellEdit]) -> Result<(), String> {
    if project.is_read_only() {
        return Err(format!(
            "the tune is read-only: {}",
            project.report().read_only_reasons().join("; ")
        ));
    }

    let Some(spec) = project.layout().variable(name) else {
        return Err(format!("the tune has no variable named `{name}`"));
    };
    if spec.is_controller_owned() {
        return Err(format!(
            "`{name}` is owned by the controller and cannot be edited"
        ));
    }
    if !spec.shape.is_numeric() {
        return Err(format!("`{name}` holds text, not numbers"));
    }
    if spec.scale.is_none() || spec.translate.is_none() {
        return Err(format!(
            "`{name}` has an unresolved scale or translate, so a value cannot be converted"
        ));
    }

    for edit in edits {
        if !edit.value.is_finite() {
            return Err(format!(
                "the value for ({}, {}) is not a finite number",
                edit.row, edit.column
            ));
        }
        if spec.shape.offset(edit.row, edit.column).is_none() {
            return Err(format!(
                "`{name}` has no cell ({}, {})",
                edit.row, edit.column
            ));
        }
    }

    Ok(())
}

/// Checks every axis bin edit before one is stored.
fn validate_axis_edits(project: &Project, name: &str, edits: &[AxisEdit]) -> Result<(), String> {
    if project.is_read_only() {
        return Err(format!(
            "the tune is read-only: {}",
            project.report().read_only_reasons().join("; ")
        ));
    }

    let Some(spec) = project.layout().variable(name) else {
        return Err(format!("the tune has no variable named `{name}`"));
    };
    if spec.is_controller_owned() {
        return Err(format!(
            "`{name}` is owned by the controller and cannot be edited"
        ));
    }
    if !spec.shape.is_numeric() {
        return Err(format!("`{name}` holds text, not numbers"));
    }
    if spec.scale.is_none() || spec.translate.is_none() {
        return Err(format!(
            "`{name}` has an unresolved scale or translate, so a value cannot be converted"
        ));
    }
    let len = spec.shape.element_count();
    if edits.is_empty() {
        return Err("no axis bins to write".to_owned());
    }

    for edit in edits {
        if !edit.value.is_finite() {
            return Err(format!(
                "the value for bin {} is not a finite number",
                edit.index
            ));
        }
        if edit.index >= len {
            return Err(format!("`{name}` has no bin {}", edit.index));
        }
    }

    Ok(())
}

/// Checks a scalar edit before it is stored.
fn validate_scalar_edit(project: &Project, name: &str, value: f64) -> Result<(), String> {
    if project.is_read_only() {
        return Err(format!(
            "the tune is read-only: {}",
            project.report().read_only_reasons().join("; ")
        ));
    }

    let Some(spec) = project.layout().variable(name) else {
        return Err(format!("the tune has no variable named `{name}`"));
    };
    if spec.is_controller_owned() {
        return Err(format!(
            "`{name}` is owned by the controller and cannot be edited"
        ));
    }
    if !spec.shape.is_numeric() {
        return Err(format!("`{name}` holds text, not numbers"));
    }
    if !spec.shape.is_scalar() {
        return Err(format!(
            "`{name}` is an array; edit it from its table or curve"
        ));
    }
    if spec.scale.is_none() || spec.translate.is_none() {
        return Err(format!(
            "`{name}` has an unresolved scale or translate, so a value cannot be converted"
        ));
    }
    if !value.is_finite() {
        return Err(format!("the value for `{name}` is not a finite number"));
    }

    Ok(())
}

/// Sends an event, ignoring a closed channel.
///
/// A closed events channel means the UI is gone, which also ends the command
/// loop on its own; there is nothing useful to do with the error here.
fn send(events: &MTx<mpmc::Array<Event>>, event: Event) {
    if events.send(event).is_err() {
        tracing::debug!("the events channel is closed");
    }
}

/// The text of a panic payload, which is `Box<dyn Any>` and not printable.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> &str {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return message;
    }
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or("unknown panic")
}

/// The default realtime poll rate.
///
/// 10 Hz is the rate the realtime spec targets for gauges and the pre-chart
/// value display; the charts (M8) will own their own schedule.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The slowest and fastest accepted poll rates.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// How long an idle core waits while an I/O reply is outstanding.
const PENDING_WAIT: Duration = Duration::from_millis(5);

/// The first reconnect delay; doubled per attempt, capped.
const RECONNECT_BASE: Duration = Duration::from_millis(500);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// What an accepted command is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    /// The connect handshake (signature).
    Connect,
    /// A full page read.
    ReadAll,
    /// One realtime block.
    Realtime,
    /// A trace save.
    Trace,
    /// A requested disconnect.
    Disconnect,
    /// A reconnect attempt (no command owns it).
    Reconnect,
}

/// One in-flight conversation.
#[derive(Debug, Clone, Copy)]
struct Pending {
    op: OpId,
    kind: PendingKind,
}

/// A running poll schedule.
#[derive(Debug, Clone, Copy)]
struct Poll {
    interval: Duration,
    next_at: Instant,
}

/// A pending reconnect attempt.
#[derive(Debug, Clone, Copy)]
struct Reconnect {
    attempts: u32,
    next_at: Instant,
}

/// A live ECU session: the I/O thread, its pending conversations and the
/// summary the UI shows.
struct Live {
    thread: IoThread,
    replies: Rx<spsc::Array<IoReply>>,
    link_label: String,
    simulator: bool,
    definition_signature: String,
    behavior: String,
    framing: &'static str,
    pages: usize,
    realtime_block: Option<u16>,
    decoder: RealtimeDecoder,
    pending: BTreeMap<u64, Pending>,
    next_id: RequestId,
    poll: Option<Poll>,
    reconnect: Option<Reconnect>,
    sequence: u64,
    /// The ECU RAM image when no project owns one (a definition on its own).
    standalone: Option<Box<Tune>>,
    /// Whether the last handshake succeeded and no loss happened since.
    connected: bool,
    /// Whether a successful page read should start polling. Cleared when the
    /// user stops it, so a stop is not undone by the next read.
    auto_poll: bool,
    /// A signature-only probe: report the verdict, then close.
    probe: bool,
    /// The last signature verdict, kept for reconnects and the summary.
    signature: String,
    verdict: SignatureVerdict,
}

impl Live {
    fn new(
        thread: IoThread,
        replies: Rx<spsc::Array<IoReply>>,
        link: &LinkChoice,
        definition: &Definition,
        behavior: Behavior,
        framing: LinkMode,
        standalone: Option<Box<Tune>>,
    ) -> Self {
        let realtime_block = definition
            .protocol
            .och_block_size
            .and_then(|size| u16::try_from(size).ok())
            .filter(|size| *size > 0);
        Self {
            thread,
            replies,
            link_label: link.label(),
            simulator: link.is_simulator(),
            definition_signature: definition.signature.clone(),
            behavior: behavior.as_dyn().name().to_owned(),
            framing: match framing {
                LinkMode::Legacy => "legacy",
                LinkMode::Envelope => "envelope",
            },
            pages: definition.pages.len(),
            realtime_block,
            decoder: RealtimeDecoder::new(definition),
            pending: BTreeMap::new(),
            next_id: RequestId::FIRST,
            poll: None,
            reconnect: None,
            sequence: 0,
            standalone,
            connected: false,
            auto_poll: true,
            probe: false,
            signature: String::new(),
            verdict: SignatureVerdict::Mismatch,
        }
    }

    /// Queues one request and records what its answer will mean.
    fn request(&mut self, op: OpId, kind: PendingKind, io_op: IoOp) -> Result<(), CoreError> {
        let id = self.next_id;
        self.thread.submit(id.0, io_op)?;
        self.next_id = self.next_id.next();
        self.pending.insert(id.0, Pending { op, kind });
        Ok(())
    }

    /// Whether a request of this kind is already in flight.
    fn pending_kind(&self, kind: PendingKind) -> bool {
        self.pending.values().any(|pending| pending.kind == kind)
    }

    /// Starts (or changes) the poll schedule.
    fn start_polling(&mut self, interval_ms: u32) {
        let interval = Duration::from_millis(u64::from(interval_ms))
            .clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL);
        self.poll = Some(Poll {
            interval,
            next_at: Instant::now(),
        });
    }

    /// Stops polling; the last frame stays readable.
    fn stop_polling(&mut self) {
        self.poll = None;
    }

    /// Whether a poll is due.
    fn poll_due(&self) -> bool {
        self.poll
            .as_ref()
            .is_some_and(|poll| poll.next_at <= Instant::now())
    }

    /// Moves the poll schedule on.
    fn polled(&mut self) {
        if let Some(poll) = &mut self.poll {
            poll.next_at = Instant::now() + poll.interval;
        }
    }

    /// Whether a reconnect attempt is due.
    fn reconnect_due(&self) -> bool {
        self.reconnect
            .as_ref()
            .is_some_and(|reconnect| reconnect.next_at <= Instant::now())
    }

    /// The summary to show for this session.
    fn summary(&self) -> ConnectionSummary {
        let read_only = self.verdict == SignatureVerdict::Mismatch;
        ConnectionSummary {
            port: self.link_label.clone(),
            simulator: self.simulator,
            signature: self.signature.clone(),
            definition_signature: self.definition_signature.clone(),
            verdict: self.verdict,
            read_only,
            read_only_reason: read_only.then(|| {
                format!(
                    "the ECU reports {:?}, the definition expects {:?}",
                    self.signature, self.definition_signature
                )
            }),
            behavior: self.behavior.clone(),
            framing: self.framing,
            pages: self.pages,
            realtime_block: self.realtime_block,
        }
    }

    /// Re-evaluates the signature verdict after a handshake.
    fn set_signature(&mut self, signature: String) {
        self.verdict = if signature.trim() == self.definition_signature.trim() {
            SignatureVerdict::Match
        } else {
            SignatureVerdict::Mismatch
        };
        self.signature = signature;
        self.connected = true;
        self.reconnect = None;
    }
}

/// Stops a live session, releases its port and forgets its frame.
///
/// Every command still waiting for the wire is answered before the port is
/// closed. A session can be replaced by a project load or an explicit unload
/// while a page read is in flight; without this, that `OpId` would never get an
/// answer and the UI would wait forever.
fn teardown(live: &mut Option<Live>, latest: &LatestFrame, events: &MTx<mpmc::Array<Event>>) {
    let Some(mut live) = live.take() else {
        return;
    };
    for pending in std::mem::take(&mut live.pending).into_values() {
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
                        message: "the session was closed before the operation completed".to_owned(),
                    },
                },
            ),
        }
    }
    live.thread.shutdown();
    latest.clear();
}

/// How long the core may wait for the next command.
fn next_wait(live: &Option<Live>) -> Option<Duration> {
    let live = live.as_ref()?;
    if !live.pending.is_empty() {
        return Some(PENDING_WAIT);
    }

    let now = Instant::now();
    let poll = live
        .poll
        .as_ref()
        .map(|poll| poll.next_at.saturating_duration_since(now));
    let reconnect = live
        .reconnect
        .as_ref()
        .map(|reconnect| reconnect.next_at.saturating_duration_since(now));
    match (poll, reconnect) {
        (Some(poll), Some(reconnect)) => Some(poll.min(reconnect).max(Duration::from_millis(1))),
        (Some(wait), None) | (None, Some(wait)) => Some(wait.max(Duration::from_millis(1))),
        (None, None) => None,
    }
}

/// Starts a connection for `Connect`.
#[allow(clippy::too_many_arguments)]
fn connect(
    op: OpId,
    session: &mut Session,
    live: &mut Option<Live>,
    events: &MTx<mpmc::Array<Event>>,
    report: &mut CoreReport,
    port: PortChoice,
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
    let Some(behavior) = Behavior::select(definition) else {
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

    let link = match &port {
        PortChoice::Simulator(config) => LinkChoice::Simulator(config.clone()),
        PortChoice::Serial { port, baud } => LinkChoice::Serial(
            oxtuner_transport::SerialSettings::new(port.clone()).with_baud(*baud),
        ),
    };
    let effective_framing = framing.unwrap_or_else(|| default_framing(definition));
    let plan = IoPlan {
        definition: Box::new(definition.clone()),
        behavior,
        link_mode: framing,
        link: link.clone(),
    };

    match IoThread::start(plan) {
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
            connection.probe = probe;
            if let Err(error) = connection.request(op, PendingKind::Connect, IoOp::Signature) {
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
fn default_framing(definition: &Definition) -> LinkMode {
    match definition.protocol.envelope.as_deref() {
        Some(format) if format.eq_ignore_ascii_case("msEnvelope_1.0") => LinkMode::Envelope,
        _ => LinkMode::Legacy,
    }
}

/// Drains replies and runs the poll/reconnect timers.
fn service_live(
    live: &mut Option<Live>,
    session: &mut Session,
    events: &MTx<mpmc::Array<Event>>,
    latest: &LatestFrame,
    report: &mut CoreReport,
) {
    let Some(connection) = live.as_mut() else {
        return;
    };

    while let Ok(reply) = connection.replies.try_recv() {
        let keep = on_reply(connection, reply, session, events, latest, report);
        if !keep {
            teardown(live, latest, events);
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

    if connection.poll_due()
        && !connection.pending_kind(PendingKind::Realtime)
        && !connection.pending_kind(PendingKind::ReadAll)
        && !connection.pending_kind(PendingKind::Reconnect)
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

/// Handles one I/O reply. Returns `false` when the session is finished.
fn on_reply(
    live: &mut Live,
    reply: IoReply,
    session: &mut Session,
    events: &MTx<mpmc::Array<Event>>,
    latest: &LatestFrame,
    report: &mut CoreReport,
) -> bool {
    let Some(pending) = live.pending.remove(&reply.id) else {
        tracing::debug!(id = reply.id, "an I/O reply had no pending request");
        return true;
    };

    match (pending.kind, reply.outcome) {
        (PendingKind::Connect, IoOutcome::Signature { signature }) => {
            live.set_signature(signature);
            let summary = live.summary();
            if live.probe {
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
                        summary,
                    },
                );
                // A probe is not a session: its port is released at once.
                return false;
            }
            tracing::info!(
                port = %summary.port,
                signature = %summary.signature,
                verdict = summary.verdict.as_str(),
                read_only = summary.read_only,
                "ECU connected"
            );
            send(
                events,
                Event::Connected {
                    op: pending.op,
                    summary,
                },
            );
            true
        }
        (PendingKind::Connect, IoOutcome::Failed(failure)) => {
            let probe = live.probe;
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
                    // The documented connect flow is signature → read all pages
                    // → start the poller. Starting it earlier would interleave
                    // realtime replies with the page read on the wire.
                    if live.auto_poll && live.realtime_block.is_some() && live.poll.is_none() {
                        live.start_polling(
                            u32::try_from(DEFAULT_POLL_INTERVAL.as_millis()).unwrap_or(100),
                        );
                    }
                    send(
                        events,
                        Event::PagesRead {
                            op: pending.op,
                            summary,
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
            let sequence = live.sequence;
            live.sequence = sequence.wrapping_add(1);
            let frame = live.decoder.decode(&block, sequence);
            if frame.values.is_empty() {
                return true;
            }
            latest.set(frame);
            report.realtime_frames += 1;
            send(events, Event::RealtimeFrame { sequence });
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
            let summary = live.summary();
            tracing::info!(
                signature = %summary.signature,
                verdict = summary.verdict.as_str(),
                "ECU reconnected"
            );
            send(events, Event::Reconnected { summary });
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

/// Turns a pending kind into the failure code its UI switches on.
fn failure_code(kind: PendingKind) -> FailureCode {
    match kind {
        PendingKind::Connect => FailureCode::ConnectFailed,
        PendingKind::ReadAll => FailureCode::ReadFailed,
        PendingKind::Realtime => FailureCode::RealtimeRejected,
        PendingKind::Trace => FailureCode::TraceFailed,
        PendingKind::Disconnect | PendingKind::Reconnect => FailureCode::NotConnected,
    }
}

/// Marks the session as lost and schedules a reconnect.
fn connection_lost(
    live: &mut Live,
    session: &mut Session,
    events: &MTx<mpmc::Array<Event>>,
    latest: &LatestFrame,
    reason: String,
) {
    tracing::warn!(%reason, "the ECU link was lost");
    live.connected = false;
    live.stop_polling();
    session.mark_unknown();
    latest.clear();

    // Every conversation that was waiting is answered now: a vanished link
    // must not leave a command unanswered.
    for pending in std::mem::take(&mut live.pending).into_values() {
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
fn schedule_reconnect(live: &mut Live) {
    let attempts = live
        .reconnect
        .map(|reconnect| reconnect.attempts.saturating_add(1))
        .unwrap_or(0);
    // First retry after RECONNECT_BASE, then doubling: 500 ms, 1 s, 2 s, 4 s, 5 s.
    let delay = RECONNECT_BASE
        .saturating_mul(1u32 << attempts.saturating_sub(1).min(4))
        .min(RECONNECT_MAX);
    live.reconnect = Some(Reconnect {
        attempts,
        next_at: Instant::now() + delay,
    });
}

/// Stops polling without touching the port (disconnect path).
fn stop_for_disconnect(live: &mut Live) {
    live.stop_polling();
    live.reconnect = None;
    live.connected = false;
}

/// Applies a complete page read to the session's ECU image.
fn commit_pages(
    session: &mut Session,
    live: &mut Live,
    pages: &[PageRead],
) -> Result<ReadAllSummary, String> {
    let mut summary = ReadAllSummary {
        pages: 0,
        bytes: 0,
        constants: 0,
        skipped: 0,
        signature_verified: live.verdict == SignatureVerdict::Match,
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
            project
                .images_mut()
                .set_ecu_ram(ram)
                .map_err(|error| error.to_string())?;
        }
        Session::Definition(_) => {
            let tune = live
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
    use super::*;

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
}
