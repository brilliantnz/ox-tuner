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
//! reconnect timers, write/burn transactions, the session log and the last read
//! summary. The write path, guardrails, chart history and logger all land here
//! rather than in the UI — that is the point of the crate
//! (`docs/architecture/crates/oxtuner-core.md`).

use std::{
    collections::{BTreeMap, HashSet},
    io,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossfire::{MTx, RecvTimeoutError, Rx, mpsc, spsc};
use oxtuner_datalog::{LogColumn, LogStatus, Logger};
use oxtuner_ini::Definition;
use oxtuner_project::Project;
use oxtuner_protocol::{Behavior, LinkMode};
use oxtuner_tune::{IniPage, Tune, TuneDiff, TuneStatus, decode_page, patch_page};

use crate::handle::{CoreHandle, EventStream, EventTx, Gate, SendFailure, StopFlag};
use crate::history::{ChannelInfo, ChartHistory};
use crate::io_thread::{
    self, IoFailureKind, IoOp, IoOutcome, IoPlan, IoReply, IoThread, PageRead, PageWrite, RequestId,
};
use crate::journal::{self, Journal};
use crate::message::{
    AxisEdit, BurnSummary, CellEdit, Command, ConnectionSummary, DefinitionSummary, DialogSummary,
    Envelope, Event, Failure, FailureCode, ImportSummary, JournalEntry, OpId, PortChoice,
    PortSummary, ProjectSummary, ReadAllSummary, RevertSummary, SaveSummary, SignatureVerdict,
    TableSummary, WriteSummary,
};
use crate::realtime::{FrameNotify, FrameWatch, LatestFrame, RealtimeDecoder, RealtimeFrame};
use crate::safety::{Safety, clamp_user};
use crate::snapshot;

/// Queue sizes and thread behaviour.
#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// Commands that may wait for the core. Small on purpose: a deep queue
    /// only hides a stalled core, and every queued command has to be answered
    /// at shutdown.
    pub command_capacity: usize,
    /// Events that may wait for the UI. Dropping an event outside shutdown would
    /// break the "answered exactly once while reading" guarantee, so this is
    /// generous by default.
    ///
    /// The value is raised to at least `command_capacity + 2` at start-up, so
    /// one answer (or one [`Event::Rejected`]) per accepted command plus
    /// [`Event::Stopped`] fits even when nothing reads. That keeps the common
    /// quit path lossless, but it is not what bounds a stalled consumer: a
    /// long session keeps producing answers, so the queue can still fill. The
    /// stop flag is the guarantee — once shutdown is accepted, an event that
    /// does not fit is dropped and counted instead of blocking (ADR-0023).
    pub event_capacity: usize,
    /// Whether the global write switch starts on (S8). Off unless the user
    /// enabled it in the settings and the app passed that choice in.
    pub writes_enabled: bool,
    /// Where a standalone definition's write journal and logs live. A project
    /// always uses its own `.ox-tuner/` sidecar.
    pub journal_dir: Option<PathBuf>,
    /// Frames retained for the live charts. 6 000 holds one minute at 100 Hz.
    pub history_capacity: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            command_capacity: 64,
            event_capacity: 1024,
            writes_enabled: false,
            journal_dir: None,
            history_capacity: 6_000,
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
    /// Verified RAM writes completed.
    pub writes: u64,
    /// Verified burns completed.
    pub burns: u64,
    /// Read-back verifications that matched.
    pub verifications: u64,
    /// Read-back verifications that differed.
    pub verification_failures: u64,
    /// Log frames dropped because the writer queue was full.
    pub log_drops: u64,
    /// Events the UI never received because shutdown was already in progress
    /// and it had stopped reading (ADR-0023).
    pub dropped_events: u64,
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
    commands: &MTx<mpsc::Array<Envelope>>,
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
    /// Wakes the UI when a newer frame is readable. Coalesced, never blocking
    /// (ADR-0021).
    pub frames: FrameWatch,
    /// The bounded realtime history the charts read (ADR-0014/0015).
    pub history: ChartHistory,
    /// The session log's live counters, when one is running.
    pub log: LogStatus,
    /// The thread, which joins on shutdown.
    pub thread: CoreThread,
}

/// Owns the core thread and joins it on drop.
///
/// Dropping is a safe shutdown: it queues [`Command::Shutdown`], raises the stop
/// flag so a full event queue cannot hold the thread, and joins, so a forgotten
/// core is stopped and reaped and [`CoreThread::join`] returns its report. One
/// policy is explicit rather than pretty: a core that refuses shutdown is
/// wedged, and a wedged thread is abandoned — loudly, after [`SHUTDOWN_WAIT`] +
/// [`JOIN_WAIT`] — instead of hanging the caller forever. Nothing is ever
/// detached silently.
pub struct CoreThread {
    join: Option<JoinHandle<CoreReport>>,
    commands: MTx<mpsc::Array<Envelope>>,
    stop: Arc<StopFlag>,
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
        // Raise the flag first, and only then wait for room in the command
        // queue (ADR-0023). The core thread may be blocked handing an answer to
        // a UI that stopped reading; raising the flag makes that wait bounded,
        // so the core returns to its loop and drains the command queue this
        // `Shutdown` is waiting on. Raising it afterwards would deadlock the two
        // waits against each other until both time out.
        self.stop.request();
        match send_shutdown(&self.commands, OpId::new(0)) {
            Ok(()) | Err(CoreError::Stopped) => {}
            // The queue stayed full even with delivery unblocked, so the thread
            // is wedged inside a command (a slow ECU conversation). Give it a
            // bounded moment, then abandon it — loudly and explicitly. Quitting
            // must not hang, and nothing here detaches silently.
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
    let command_capacity = config.command_capacity.max(1);
    // The floor is enforced rather than only documented: every accepted command
    // owes exactly one answer, so `command_capacity` answers plus `Stopped`
    // must always fit. Without it, a small configured capacity would let a
    // shutdown drain block on the event queue, and the caller would abandon a
    // perfectly healthy core.
    let event_capacity = config
        .event_capacity
        .max(command_capacity.saturating_add(2));
    let (commands, command_rx) = mpsc::bounded_blocking(command_capacity);
    let (event_tx, events) = spsc::bounded_blocking_async(event_capacity);
    let stop = Arc::new(StopFlag::default());
    let event_tx = EventTx::new(event_tx, Arc::clone(&stop));
    let (frame_notify, frame_watch) = crate::realtime::frame_wake();
    let next_op = Arc::new(AtomicU64::new(OpId::FIRST));
    let gate = Arc::new(Gate::default());
    let core_gate = Arc::clone(&gate);
    let latest = LatestFrame::default();
    let history = ChartHistory::new(config.history_capacity);
    let log = LogStatus::new();
    let safety = Safety::new(config.writes_enabled);
    let journal_dir = config.journal_dir.clone();
    let core_latest = latest.clone();
    let core_history = history.clone();
    let core_log = log.clone();

    let join = thread::Builder::new()
        .name("oxtuner-core".to_owned())
        .spawn(move || {
            run(
                command_rx,
                event_tx,
                core_gate,
                core_latest,
                core_history,
                core_log,
                frame_notify,
                safety,
                journal_dir,
            )
        })
        .map_err(CoreError::Spawn)?;

    tracing::debug!("core thread started");

    Ok(CoreParts {
        handle: CoreHandle {
            commands: commands.clone(),
            next_op,
            gate,
            stop: Arc::clone(&stop),
        },
        events: EventStream { events },
        latest,
        frames: frame_watch,
        history,
        log,
        thread: CoreThread {
            join: Some(join),
            commands,
            stop,
        },
    })
}

/// The command loop.
///
/// Blocks on the queue: with no poller yet there is nothing to do while idle,
/// and a timed wait would only wake the thread for nothing.
#[allow(clippy::too_many_arguments)]
fn run(
    commands: Rx<mpsc::Array<Envelope>>,
    events: EventTx,
    gate: Arc<Gate>,
    latest: LatestFrame,
    history: ChartHistory,
    log: LogStatus,
    frames: FrameNotify,
    mut safety: Safety,
    journal_dir: Option<PathBuf>,
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
                &mut safety,
                journal_dir.as_deref(),
                &events,
                &latest,
                &history,
                &log,
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
        service_live(
            &mut live,
            &mut session,
            &mut safety,
            journal_dir.as_deref(),
            &events,
            &latest,
            &frames,
            &history,
            &log,
            &mut report,
        );
    }

    teardown(
        &mut live,
        &mut session,
        &mut safety,
        journal_dir.as_deref(),
        &latest,
        &events,
        &history,
        &log,
    );

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
    report.dropped_events = events.dropped();
    report.uptime = started.elapsed();
    tracing::debug!(
        handled = report.handled,
        rejected = report.rejected,
        dropped_events = report.dropped_events,
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

    /// The open project, when it is the project shape.
    fn project(&self) -> Option<&Project> {
        match self {
            Self::Project(project) => Some(project),
            Self::Definition(_) | Self::Empty => None,
        }
    }

    /// The open project for mutation.
    fn project_mut(&mut self) -> Option<&mut Project> {
        match self {
            Self::Project(project) => Some(project),
            Self::Definition(_) | Self::Empty => None,
        }
    }

    /// The derived write state, when a project is open.
    fn tune_status(&self) -> Option<TuneStatus> {
        self.project().map(|project| project.images().status())
    }

    /// Everything unproven on the ECU side becomes unknown (ADR-0010).
    fn mark_unknown(&mut self) {
        if let Self::Project(project) = self {
            project.images_mut().mark_unknown();
        }
    }

    /// The journal this session writes: the project sidecar, or the configured
    /// fallback for a standalone definition. `None` when neither exists.
    fn journal(&self, fallback: Option<&std::path::Path>) -> Option<Journal> {
        match self {
            Self::Project(project) => Some(Journal::new(journal::project_path(project.root()))),
            Self::Definition(_) | Self::Empty => fallback.map(Journal::new),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle(
    envelope: Envelope,
    session: &mut Session,
    live: &mut Option<Live>,
    safety: &mut Safety,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    latest: &LatestFrame,
    history: &ChartHistory,
    log: &LogStatus,
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
            if let Some(definition) = session.definition() {
                configure_history(history, definition);
            }
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
            if connection.write_tx.is_some() || connection.pending_kind(PendingKind::Write) {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::Busy,
                            message: "a write or burn is still running; closing now resets the \
                                      board and could drop it"
                                .to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
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
            if connection.write_tx.is_some() || connection.pending_kind(PendingKind::Write) {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::ReadFailed,
                            message: "a write or burn is running; read the pages after it finishes"
                                .to_owned(),
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
        Command::AdoptEcuImage => {
            adopt_ecu_image(op, session, events, report);
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
        Command::SetWritesEnabled { enabled } => {
            safety.set_writes_enabled(enabled);
            let connection = live
                .as_ref()
                .map(|connection| connection.summary(safety.writes_enabled()));
            tracing::info!(%op, enabled, "global write switch changed");
            send(
                events,
                Event::WriteAccessChanged {
                    op,
                    writes_enabled: enabled,
                    connection,
                },
            );
            Outcome::Continue
        }
        Command::AcknowledgeWriteFailure => {
            let was_halted = safety.halted().is_some();
            safety.clear_halt();
            tracing::info!(%op, was_halted, "write failure acknowledged");
            send(events, Event::WriteFailureAcknowledged { op });
            Outcome::Continue
        }
        Command::RefreshTuneStatus => {
            match session.tune_status() {
                Some(status) => {
                    send(events, Event::TuneStatusRead { op, status });
                }
                None => send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NothingToSave,
                            message: "no tune is open".to_owned(),
                        },
                    },
                ),
            }
            Outcome::Continue
        }
        Command::ReadJournal => {
            let Some(journal) = session.journal(journal_dir) else {
                send(
                    events,
                    Event::JournalRead {
                        op,
                        path: None,
                        entries: Vec::new(),
                    },
                );
                return Outcome::Continue;
            };
            match journal.read(journal::JOURNAL_READ_LIMIT) {
                Ok(entries) => send(
                    events,
                    Event::JournalRead {
                        op,
                        path: Some(journal.path().to_owned()),
                        entries,
                    },
                ),
                Err(error) => {
                    tracing::warn!(%op, %error, "the write journal could not be read");
                    report.failures += 1;
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::JournalFailed,
                                message: error.to_string(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        Command::WriteToRam { pages } => write_to_ram(
            op,
            session,
            live,
            safety,
            journal_dir,
            events,
            report,
            pages,
        ),
        Command::BurnToFlash { pages } => burn_to_flash(
            op,
            session,
            live,
            safety,
            journal_dir,
            events,
            report,
            pages,
        ),
        Command::RevertTune => revert_tune(op, session, journal_dir, events, report),
        Command::ImportTune { path, consented } => import_tune(
            op,
            session,
            journal_dir,
            events,
            report,
            &path,
            consented,
            "import",
        ),
        Command::RestoreSnapshot { path } => import_tune(
            op,
            session,
            journal_dir,
            events,
            report,
            &path,
            true,
            "restore",
        ),
        Command::ListSnapshots => {
            let Some(project) = session.project() else {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::SnapshotFailed,
                    "no project is open",
                );
            };
            let paths = snapshot::list(project.root());
            send(events, Event::SnapshotsListed { op, paths });
            Outcome::Continue
        }
        Command::StartLog { directory } => start_log(op, live, events, report, &directory, log),
        Command::StopLog => stop_log(op, live, events, report),
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
                    configure_history(history, &parsed);
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
                    configure_history(history, project.definition());
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
        Command::EditTable { name, mut edits } => {
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

            for edit in &mut edits {
                if let Some(spec) = project.layout().variable(&name) {
                    let (clamped, _) = clamp_user(spec, edit.value);
                    edit.value = clamped;
                }
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
        Command::EditAxis {
            table,
            axis,
            mut edits,
        } => {
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

            for edit in &mut edits {
                if let Some(spec) = project.layout().variable(&variable) {
                    let (clamped, _) = clamp_user(spec, edit.value);
                    edit.value = clamped;
                }
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
            mut value,
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

            if let Some(spec) = project.layout().variable(&name) {
                let (clamped, _) = clamp_user(spec, value);
                value = clamped;
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
            *session = Session::Empty;
            history.configure(Vec::new());
            tracing::debug!(%op, had_session, "session unloaded");
            send(events, Event::DefinitionUnloaded { op });
            Outcome::Continue
        }
    }
}

/// Counts the cells and variables where `other` differs from the project's
/// working image. Used for an imported tune, whose layout is a separate
/// allocation but describes the same definition.
fn changed_against(project: &Project, other: &Tune) -> (usize, usize) {
    let mut cells = 0usize;
    let mut variables = 0usize;
    for spec in project.layout().variables() {
        let Some(current) = project.tune().value(&spec.name) else {
            continue;
        };
        let Some(imported) = other.value(&spec.name) else {
            continue;
        };
        let changed = current.changed_cells(imported);
        if changed > 0 {
            cells += changed;
            variables += 1;
        }
    }
    (cells, variables)
}

/// Clamps every non-controller tune value that is about to be written (S2).
///
/// Edits are already clamped when they enter the core. An imported tune or a
/// file written by another tool can still carry an out-of-range value, and the
/// write path must not send those blindly. A value the controller already
/// holds is left alone: it is not part of the write plan, so clamping it would
/// turn "leave the ECU's tune alone" into a write of a different number — a
/// definition's range is not the firmware's limit. Controller-owned values are
/// not part of the write plan either. Returns how many cells changed.
fn clamp_working_changes(project: &mut Project) -> usize {
    let mut clamped: Vec<(String, usize, f64)> = Vec::new();
    {
        let images = project.images();
        let Some(ram) = images.ecu_ram() else {
            return 0;
        };
        for spec in project.layout().variables() {
            if spec.is_controller_owned() {
                continue;
            }
            for index in 0..spec.element_count() {
                let Ok(value) = images.working().user_at(&spec.name, index) else {
                    continue;
                };
                if ram
                    .user_at(&spec.name, index)
                    .is_ok_and(|sent| sent == value)
                {
                    // The ECU holds this value; there is nothing to send.
                    continue;
                }
                let (wanted, changed) = clamp_user(spec, value);
                if changed {
                    clamped.push((spec.name.clone(), index, wanted));
                }
            }
        }
    }
    let count = clamped.len();
    for (name, index, value) in clamped {
        if let Err(error) = project.tune_mut().set_user_at(&name, index, value) {
            tracing::warn!(%name, %error, "a clamped value could not be stored");
        }
    }
    count
}

/// The values that differ from ECU RAM, in layout order.
///
/// This is a tune **sync**, not a replay of edit timestamps: it survives saving
/// the `.msq` (which moves the file baseline) and an ECU that holds different
/// values from the file. Controller-owned values are excluded — the ECU owns
/// them, and writing a default over one is exactly the kind of damage S1–S10
/// exists to prevent.
fn unsent_changes(project: &Project) -> (Vec<String>, Vec<u8>, usize) {
    let working = project.images().working();
    let Some(ram) = project.images().ecu_ram() else {
        return (Vec::new(), Vec::new(), 0);
    };
    let mut variables = Vec::new();
    let mut pages = std::collections::BTreeSet::new();
    let mut cells = 0usize;
    for spec in project.layout().variables() {
        if spec.is_controller_owned() {
            continue;
        }
        let Some(page) = spec.ini_page() else {
            // PC-side values are stored in the tune file, never sent to the ECU.
            continue;
        };
        if spec.address.is_none() {
            // An unresolved address cannot be placed in a page image; skipping
            // it is honest, writing a guessed offset would not be.
            continue;
        }
        let (Some(after), Some(sent)) = (working.value(&spec.name), ram.value(&spec.name)) else {
            continue;
        };
        let remaining = after.changed_cells(sent);
        if remaining == 0 {
            continue;
        }
        variables.push(spec.name.clone());
        cells += remaining;
        pages.insert(page.get());
    }
    (variables, pages.into_iter().collect(), cells)
}

/// The unsent variables and cells on `pages`.
fn unsent_for_pages(project: &Project, pages: &[u8]) -> (Vec<String>, usize) {
    let (variables, _, _) = unsent_changes(project);
    let working = project.images().working();
    let Some(ram) = project.images().ecu_ram() else {
        return (Vec::new(), 0);
    };
    let mut kept = Vec::new();
    let mut cells = 0usize;
    for name in variables {
        let Some(spec) = project.layout().variable(&name) else {
            continue;
        };
        if spec
            .ini_page()
            .is_none_or(|page| !pages.contains(&page.get()))
        {
            continue;
        }
        let (Some(after), Some(sent)) = (working.value(&name), ram.value(&name)) else {
            continue;
        };
        let remaining = after.changed_cells(sent);
        if remaining == 0 {
            continue;
        }
        cells += remaining;
        kept.push(name);
    }
    (kept, cells)
}

/// Counts edits that have not reached ECU RAM.
///
/// A burn persists whatever RAM holds, so an edit that is only in the working
/// image would be silently left behind. The comparison is working → RAM: a
/// value the ECU already holds is not an unsent edit.
fn unsent_edits(project: &Project) -> (usize, usize) {
    let (variables, _, cells) = unsent_changes(project);
    (cells, variables.len())
}

/// Makes the ECU image the working tune.
///
/// Reading pages fills the ECU image but leaves the project file's values on
/// screen, so an unsaved edit is never lost. This is the explicit way to start
/// from what the controller holds instead. The file baseline does not move, so
/// the adopted values show as unsaved changes and a later write is only the
/// delta from the ECU's own tune.
fn adopt_ecu_image(
    op: OpId,
    session: &mut Session,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::AdoptRefused,
            "open a project before taking the ECU's tune",
        );
    };
    let Some(ram) = project.images().ecu_ram().cloned() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::AdoptRefused,
            "read the ECU pages before taking its tune",
        );
    };
    if let Err(error) = project.images_mut().import(ram) {
        return refuse(
            events,
            report,
            op,
            FailureCode::AdoptRefused,
            &error.to_string(),
        );
    }
    tracing::info!("the ECU image became the working tune");
    send(events, Event::EcuAdopted { op });
    Outcome::Continue
}

/// Starts a verified RAM write (S1–S3, S6–S8).
#[allow(clippy::too_many_arguments)]
fn write_to_ram(
    op: OpId,
    session: &mut Session,
    live: &mut Option<Live>,
    safety: &Safety,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    requested: Vec<u8>,
) -> Outcome {
    let Some(connection) = live.as_ref() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "connect to the ECU before writing",
        );
    };
    if connection.write_tx.is_some() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "a write or burn is already running",
        );
    }
    if connection.pending_kind(PendingKind::ReadAll) {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "a page read is running; write after it finishes",
        );
    }
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "open a project before writing",
        );
    };
    let read_only = project.report().read_only_reasons().join("; ");
    if let Err(reason) = safety.gate(
        connection.connected,
        connection.verdict == SignatureVerdict::Match,
        true,
        (!read_only.is_empty()).then_some(read_only.as_str()),
    ) {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            reason.as_str(),
        );
    }
    if project.images().ecu_ram().is_none() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "read the ECU into the tune image before writing (the session does this after connecting)",
        );
    }
    if !connection.pages_complete {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "read every page before writing; an incomplete ECU image cannot be edited safely",
        );
    }
    let clamped = clamp_working_changes(project);
    if clamped > 0 {
        tracing::info!(%op, clamped, "out-of-range values clamped before writing (S2)");
    }
    let (changed_variables, changed_pages, _) = unsent_changes(project);
    if changed_variables.is_empty() || changed_pages.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "there are no values that differ from ECU RAM",
        );
    }

    let targets = if requested.is_empty() {
        changed_pages
    } else {
        let mut chosen = Vec::new();
        for page in requested {
            let Some(declared) = project
                .definition()
                .pages
                .iter()
                .find(|entry| entry.number == page)
            else {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::WriteRefused,
                    &format!("page {page} is not in the definition"),
                );
            };
            if !declared.is_writable() {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::WriteRefused,
                    &format!("page {page} has no write command"),
                );
            }
            if !changed_pages.contains(&page) {
                continue;
            }
            chosen.push(page);
        }
        chosen.sort_unstable();
        chosen.dedup();
        chosen
    };
    if targets.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "the selected pages have no values that differ from ECU RAM",
        );
    }
    let (changed_variables, cells) = unsent_for_pages(project, &targets);
    if changed_variables.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "the selected pages hold no values that still need sending",
        );
    }
    let variables = changed_variables.len();

    let snapshot = match snapshot::write(project, "write") {
        Ok(path) => path,
        Err(message) => {
            return refuse(
                events,
                report,
                op,
                FailureCode::SnapshotFailed,
                message.as_str(),
            );
        }
    };
    let Some(journal) = session.journal(journal_dir) else {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            "no write journal can be written for this session; refusing an unjournaled write",
        );
    };
    let begin = JournalEntry {
        at: journal::timestamp(),
        operation: "write".to_owned(),
        phase: "begin".to_owned(),
        port: connection.link_label.clone(),
        signature: connection.signature.clone(),
        pages: journal::pages_label(&targets),
        cells,
        variables,
        result: "pending".to_owned(),
        detail: format!("snapshot {}", snapshot.display()),
    };
    if let Err(error) = journal.append(&begin) {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            &format!("the write journal could not be written: {error}"),
        );
    }

    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "the session closed before the write started",
        );
    };
    connection.begin_transaction(WriteTxn {
        op,
        kind: TxnKind::Write,
        phase: TxnPhase::ReadBase,
        pages: targets.clone(),
        burned: 0,
        base: BTreeMap::new(),
        expected: BTreeMap::new(),
        readback: BTreeMap::new(),
        changed_variables,
        cells,
        variables,
        snapshot: Some(snapshot),
        journal: Some(journal),
    });
    if let Err(error) =
        connection.request(op, PendingKind::Write, IoOp::ReadPages { pages: targets })
    {
        connection.abort_transaction("the write could not be queued");
        report.failures += 1;
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::WriteRefused,
                    message: error.to_string(),
                },
            },
        );
    }
    Outcome::Continue
}

/// Starts a verified burn (S3–S5, S7, S8).
#[allow(clippy::too_many_arguments)]
fn burn_to_flash(
    op: OpId,
    session: &mut Session,
    live: &mut Option<Live>,
    safety: &Safety,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    requested: Vec<u8>,
) -> Outcome {
    let Some(connection) = live.as_ref() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "connect to the ECU before burning",
        );
    };
    if connection.write_tx.is_some() {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "a write or burn is already running",
        );
    }
    if connection.pending_kind(PendingKind::ReadAll) {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "a page read is running; burn after it finishes",
        );
    }
    let Some(project) = session.project() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "open a project before burning",
        );
    };
    let read_only = project.report().read_only_reasons().join("; ");
    if let Err(reason) = safety.gate(
        connection.connected,
        connection.verdict == SignatureVerdict::Match,
        true,
        (!read_only.is_empty()).then_some(read_only.as_str()),
    ) {
        return refuse(events, report, op, FailureCode::BurnRefused, &reason);
    }
    if !connection.pages_complete {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "read every page before burning",
        );
    }
    if project.images().ecu_ram().is_none() {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "the ECU image is unknown; read it first",
        );
    }
    let (unsent, _) = unsent_edits(project);
    if unsent > 0 {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "the working image has edits that are not in ECU RAM. Write them first; burning would persist the older RAM values",
        );
    }

    let burnable: Vec<u8> = project
        .definition()
        .pages
        .iter()
        .filter(|page| page.is_burnable())
        .map(|page| page.number)
        .collect();
    if burnable.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "the definition declares no burnable page",
        );
    }
    let targets = if requested.is_empty() {
        burnable
    } else {
        let mut chosen = Vec::new();
        for page in requested {
            if !burnable.contains(&page) {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::BurnRefused,
                    &format!("page {page} cannot be burned"),
                );
            }
            chosen.push(page);
        }
        chosen.sort_unstable();
        chosen.dedup();
        chosen
    };

    let snapshot = match snapshot::write(project, "burn") {
        Ok(path) => path,
        Err(message) => return refuse(events, report, op, FailureCode::SnapshotFailed, &message),
    };
    let Some(journal) = session.journal(journal_dir) else {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            "no write journal can be written for this session",
        );
    };
    let begin = JournalEntry {
        at: journal::timestamp(),
        operation: "burn".to_owned(),
        phase: "begin".to_owned(),
        port: connection.link_label.clone(),
        signature: connection.signature.clone(),
        pages: journal::pages_label(&targets),
        cells: 0,
        variables: 0,
        result: "pending".to_owned(),
        detail: format!("snapshot {}", snapshot.display()),
    };
    if let Err(error) = journal.append(&begin) {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            &format!("the write journal could not be written: {error}"),
        );
    }

    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "the session closed before the burn started",
        );
    };
    connection.begin_transaction(WriteTxn {
        op,
        kind: TxnKind::Burn,
        phase: TxnPhase::ReadBase,
        pages: targets.clone(),
        burned: 0,
        base: BTreeMap::new(),
        expected: BTreeMap::new(),
        readback: BTreeMap::new(),
        changed_variables: Vec::new(),
        cells: 0,
        variables: 0,
        snapshot: Some(snapshot),
        journal: Some(journal),
    });
    if let Err(error) =
        connection.request(op, PendingKind::Write, IoOp::ReadPages { pages: targets })
    {
        connection.abort_transaction("the burn could not be queued");
        report.failures += 1;
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::BurnRefused,
                    message: error.to_string(),
                },
            },
        );
    }
    Outcome::Continue
}

/// Discards local edits after snapshotting the working image.
fn revert_tune(
    op: OpId,
    session: &mut Session,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "no tune is open",
        );
    };
    let Ok(diff) = TuneDiff::between(project.images().working(), project.images().baseline())
    else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "the tune images do not share a layout",
        );
    };
    let snapshot = match snapshot::write(project, "revert") {
        Ok(path) => path,
        Err(message) => return refuse(events, report, op, FailureCode::SnapshotFailed, &message),
    };
    project.images_mut().revert_to_baseline();
    tracing::info!(%op, cells = diff.cells, "tune reverted to the file baseline");
    let snapshot_detail = format!("snapshot {}", snapshot.display());
    send(
        events,
        Event::TuneReverted {
            op,
            summary: RevertSummary {
                cells: diff.cells,
                snapshot: Some(snapshot),
            },
        },
    );
    send_status(events, op, project);
    record_local_journal(
        session,
        journal_dir,
        "revert",
        diff.cells,
        0,
        "ok",
        &snapshot_detail,
    );
    Outcome::Continue
}

/// Imports one `.msq` into the working image (S4, S9).
#[allow(clippy::too_many_arguments)]
fn import_tune(
    op: OpId,
    session: &mut Session,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    path: &std::path::Path,
    consented: bool,
    operation: &str,
) -> Outcome {
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::ImportRejected,
            "no tune is open",
        );
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return refuse(
                events,
                report,
                op,
                FailureCode::ImportRejected,
                &format!("{} could not be read: {error}", path.display()),
            );
        }
    };
    let msq = match oxtuner_project::Msq::parse(&bytes) {
        Ok(msq) => msq,
        Err(error) => {
            return refuse(
                events,
                report,
                op,
                FailureCode::ImportRejected,
                &format!("{} is not a readable .msq: {error}", path.display()),
            );
        }
    };
    let check = oxtuner_project::check(&msq, project.definition());
    let signature_mismatch = check
        .blockers
        .iter()
        .any(|issue| issue.kind == oxtuner_project::IssueKind::SignatureMismatch);
    let other_blocker = check
        .blockers
        .iter()
        .find(|issue| issue.kind != oxtuner_project::IssueKind::SignatureMismatch);
    if let Some(blocker) = other_blocker {
        return refuse(
            events,
            report,
            op,
            FailureCode::ImportRejected,
            &format!("the tune cannot be imported: {}", blocker.message),
        );
    }
    if signature_mismatch && !consented {
        report.failures += 1;
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::ImportNeedsConsent,
                    message: format!(
                        "the tune in {} was written for a different ECU signature. Importing it \
                         needs explicit consent (S9)",
                        path.display()
                    ),
                },
            },
        );
        return Outcome::Continue;
    }

    let loaded = oxtuner_project::load::load(&msq, project.definition());
    let (cells, variables) = changed_against(project, &loaded.tune);
    let snapshot = match snapshot::write(project, operation) {
        Ok(path) => path,
        Err(message) => return refuse(events, report, op, FailureCode::SnapshotFailed, &message),
    };
    if let Err(error) = project.images_mut().import(loaded.tune) {
        return refuse(
            events,
            report,
            op,
            FailureCode::ImportRejected,
            &error.to_string(),
        );
    }
    tracing::info!(
        %op,
        path = %path.display(),
        cells,
        consented = signature_mismatch && consented,
        "tune imported"
    );
    let snapshot_detail = format!("snapshot {}", snapshot.display());
    send(
        events,
        Event::TuneImported {
            op,
            summary: ImportSummary {
                path: path.to_owned(),
                cells,
                variables,
                consented: signature_mismatch && consented,
                snapshot: Some(snapshot),
            },
        },
    );
    send_status(events, op, project);
    record_local_journal(
        session,
        journal_dir,
        operation,
        cells,
        variables,
        "ok",
        &snapshot_detail,
    );
    Outcome::Continue
}

/// Starts a CSV session log.
fn start_log(
    op: OpId,
    live: &mut Option<Live>,
    events: &EventTx,
    report: &mut CoreReport,
    directory: &std::path::Path,
    log: &LogStatus,
) -> Outcome {
    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "connect to the ECU before logging",
        );
    };
    if connection.logger.is_some() {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "a session log is already running",
        );
    }
    if !connection.connected {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "the link is lost; reconnect before logging",
        );
    }

    let columns = connection
        .decoder
        .columns()
        .into_iter()
        .map(|(name, units)| LogColumn::new(name, units))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "the definition declares no realtime channel to log",
        );
    }
    let column_names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let path = directory.join(format!("oxtuner-{stamp}.csv"));
    match Logger::start(&path, columns, log.clone()) {
        Ok(logger) => {
            tracing::info!(%op, path = %logger.path().display(), "session log started");
            connection.log_columns = column_names;
            connection.logger = Some(logger);
            send(
                events,
                Event::LogState {
                    op,
                    running: true,
                    summary: None,
                },
            );
        }
        Err(error) => {
            report.failures += 1;
            send(
                events,
                Event::Failed {
                    op,
                    failure: Failure {
                        code: FailureCode::LogFailed,
                        message: format!("{} could not be created: {error}", path.display()),
                    },
                },
            );
        }
    }
    Outcome::Continue
}

/// Stops the session log and flushes it.
fn stop_log(
    op: OpId,
    live: &mut Option<Live>,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "there is no session to stop logging",
        );
    };
    let Some(logger) = connection.logger.take() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "no session log is running",
        );
    };
    match logger.stop() {
        Ok(summary) => {
            tracing::info!(
                %op,
                path = %summary.path.display(),
                frames = summary.frames,
                dropped = summary.dropped,
                "session log stopped"
            );
            send(
                events,
                Event::LogState {
                    op,
                    running: false,
                    summary: Some(summary),
                },
            );
        }
        Err(error) => {
            report.failures += 1;
            send(
                events,
                Event::Failed {
                    op,
                    failure: Failure {
                        code: FailureCode::LogFailed,
                        message: error.to_string(),
                    },
                },
            );
        }
    }
    Outcome::Continue
}

/// Sends a refusal as [`Event::Failed`].
fn refuse(
    events: &EventTx,
    report: &mut CoreReport,
    op: OpId,
    code: FailureCode,
    message: &str,
) -> Outcome {
    tracing::warn!(%op, code = code.as_str(), %message, "write-path command refused");
    report.failures += 1;
    send(
        events,
        Event::Failed {
            op,
            failure: Failure {
                code,
                message: message.to_owned(),
            },
        },
    );
    Outcome::Continue
}

/// Sends the derived tune status after a change.
fn send_status(events: &EventTx, op: OpId, project: &Project) {
    send(
        events,
        Event::TuneStatusRead {
            op,
            status: project.images().status(),
        },
    );
}

/// Appends a local (non-ECU) operation's result to the journal, best effort.
///
/// Writes and burns fail closed when the journal cannot be written; revert,
/// import and restore have already changed only the working image and a
/// snapshot exists, so a journal failure is logged instead of turning a
/// completed operation into an error.
fn record_local_journal(
    session: &Session,
    journal_dir: Option<&std::path::Path>,
    operation: &str,
    cells: usize,
    variables: usize,
    result: &str,
    detail: &str,
) {
    let Some(journal) = session.journal(journal_dir) else {
        return;
    };
    let entry = JournalEntry {
        at: journal::timestamp(),
        operation: operation.to_owned(),
        phase: "result".to_owned(),
        port: "local".to_owned(),
        signature: session
            .definition()
            .map_or_else(String::new, |definition| definition.signature.clone()),
        pages: String::new(),
        cells,
        variables,
        result: result.to_owned(),
        detail: detail.to_owned(),
    };
    if let Err(error) = journal.append(&entry) {
        tracing::warn!(%error, "the local journal result could not be written");
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

/// Sends an event, logging why it did not arrive.
///
/// A closed channel means the UI is gone, which also ends the command loop on
/// its own. A drop means shutdown was in progress and the consumer had stopped
/// reading; it is counted in the report so a lost answer is never silent.
fn send(events: &EventTx, event: Event) {
    match events.send(event) {
        Ok(()) => {}
        Err(SendFailure::Closed) => tracing::debug!("the events channel is closed"),
        Err(SendFailure::Dropped) => {}
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
/// 20 Hz is the default the settings ship with; the charts and the logger read
/// the same polled frames rather than running a second schedule.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The slowest and fastest accepted poll rates.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// How long an idle core waits while an I/O reply is outstanding.
const PENDING_WAIT: Duration = Duration::from_millis(5);

/// The realtime bit that says the firmware still has EEPROM writes queued.
const BURN_PENDING_CHANNEL: &str = "burnPending";

/// How often the burn wait samples that bit.
///
/// The gap leaves the link idle long enough for the firmware's main loop to
/// run another EEPROM pass, and each sample is one short realtime block.
const BURN_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long a burn may stay queued before it is reported as not saved.
///
/// A 2560 writes up to 144 bytes per pass at idle, at roughly 4 ms a byte, so
/// a full 3.4 KiB image is about 13 s. The margin covers the definition's
/// defer delay too.
const BURN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to settle when the definition has no `burnPending` bit.
const BURN_SETTLE: Duration = Duration::from_millis(1500);

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
    /// A write or burn transaction phase.
    Write,
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

/// A burn whose pages the firmware accepted but has not written to flash yet.
///
/// Speeduino answers `b`/`B` before the EEPROM write happens: `burnSinglePage`
/// only queues the page, and the flush runs from the main loop once the link
/// has been idle for a second. Closing the port inside that window resets the
/// board and drops the pending save, so the transaction waits here until the
/// firmware reports the queue empty.
#[derive(Debug, Clone, Copy)]
struct BurnWait {
    /// When the next `burnPending` sample is due.
    next_at: Instant,
    /// When the wait stops trusting the firmware.
    deadline: Instant,
    /// Whether the definition has a `burnPending` bit to read.
    ///
    /// Without one the wait is a fixed settle: the ECU's answer alone cannot
    /// say when its own flash write finished.
    observed: bool,
}

/// What a transaction will do with the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxnKind {
    /// Write changed variables, then read back.
    Write,
    /// Burn RAM pages, then read back.
    Burn,
}

impl TxnKind {
    const fn journal_name(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Burn => "burn",
        }
    }

    const fn failure_code(self) -> FailureCode {
        match self {
            Self::Write => FailureCode::WriteFailed,
            Self::Burn => FailureCode::BurnFailed,
        }
    }

    const fn event(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Burn => "burn",
        }
    }
}

/// Where a write/burn transaction is in its sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxnPhase {
    /// Fresh page read before patching or burning.
    ReadBase,
    /// Sending page images to RAM.
    Write,
    /// Sending burn commands.
    Burn,
    /// Waiting for the firmware to flush its queued EEPROM writes.
    BurnWait,
    /// Reading pages back for verification.
    Verify,
}

/// One write or burn, owned by the live session until it finishes.
///
/// A transaction is the only path from a command to `IoOp::WritePages` or
/// `IoOp::BurnPages`, which is what makes "no auto-burn" structural (S5).
#[derive(Debug)]
struct WriteTxn {
    op: OpId,
    kind: TxnKind,
    phase: TxnPhase,
    /// Pages the transaction covers, sorted.
    pages: Vec<u8>,
    /// Pages already burned; a burn waits for each one's flash write before the
    /// next, because the firmware's queue bit covers the whole batch.
    burned: usize,
    /// Fresh page bytes read before patching or burning.
    base: BTreeMap<u8, Vec<u8>>,
    /// Bytes expected after a write, or before a burn; used for verification.
    expected: BTreeMap<u8, Vec<u8>>,
    /// Bytes read back during verification.
    readback: BTreeMap<u8, Vec<u8>>,
    /// Variables whose values this write sends; empty for a burn.
    changed_variables: Vec<String>,
    cells: usize,
    variables: usize,
    snapshot: Option<PathBuf>,
    journal: Option<Journal>,
}

/// A live ECU session: the I/O thread, its pending conversations and the
/// summary the UI shows.
struct Live {
    thread: IoThread,
    replies: Rx<spsc::Array<IoReply>>,
    link_label: String,
    custom_link: bool,
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
    /// A burn that is queued in the firmware, waiting for its flash write.
    burn_wait: Option<BurnWait>,
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
    /// The transaction in flight, if a write or burn is running.
    write_tx: Option<WriteTxn>,
    /// The live session log, when one is recording.
    logger: Option<Logger>,
    /// Channel names in the running log's column order.
    log_columns: Vec<String>,
    /// Raw page bytes last read from or written to the ECU.
    page_bytes: BTreeMap<u8, Vec<u8>>,
    /// Whether every declared page was read and applied without skips.
    pages_complete: bool,
    /// The first realtime frame's monotonic time, the chart/log time origin.
    first_frame: Option<Instant>,
    /// Monotonic seconds of the last decoded frame.
    last_frame_time: f64,
}

impl Live {
    fn new(
        thread: IoThread,
        replies: Rx<spsc::Array<IoReply>>,
        link: &PortChoice,
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
            custom_link: !link.is_serial(),
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
            burn_wait: None,
            sequence: 0,
            standalone,
            connected: false,
            auto_poll: true,
            probe: false,
            signature: String::new(),
            verdict: SignatureVerdict::Mismatch,
            write_tx: None,
            logger: None,
            log_columns: Vec::new(),
            page_bytes: BTreeMap::new(),
            pages_complete: false,
            first_frame: None,
            last_frame_time: 0.0,
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

    /// Records the transaction the next replies belong to.
    fn begin_transaction(&mut self, transaction: WriteTxn) {
        self.write_tx = Some(transaction);
    }

    /// Forgets a transaction whose first request could not be queued.
    fn abort_transaction(&mut self, reason: &str) {
        let Some(transaction) = self.write_tx.take() else {
            return;
        };
        if let Some(journal) = &transaction.journal {
            let entry = JournalEntry {
                at: journal::timestamp(),
                operation: transaction.kind.journal_name().to_owned(),
                phase: "result".to_owned(),
                port: self.link_label.clone(),
                signature: self.signature.clone(),
                pages: journal::pages_label(&transaction.pages),
                cells: transaction.cells,
                variables: transaction.variables,
                result: "refused".to_owned(),
                detail: reason.to_owned(),
            };
            if let Err(error) = journal.append(&entry) {
                tracing::warn!(%error, "the journal result could not be written");
            }
        }
        // The poll schedule resumes on the next tick; no write reached the wire.
        self.polled();
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

    /// Whether this definition can say when a burn reaches flash.
    ///
    /// Speeduino exposes `burnPending`, and it has to be read one page at a
    /// time: its queue bit is a single flag, so a batch that mixes a deferred
    /// page with an inline one loses the deferred pages when the last inline
    /// save clears the flag. One page per burn keeps every page accounted for.
    fn can_observe_burn(&self) -> bool {
        self.realtime_block.is_some() && self.decoder.has_channel(BURN_PENDING_CHANNEL)
    }

    /// Starts waiting for the firmware to write an accepted burn to flash.
    fn begin_burn_wait(&mut self) {
        let now = Instant::now();
        if self.realtime_block.is_none() {
            // Nothing to sample: the read-back runs at once, as a burn without
            // a status bit always did.
            self.burn_wait = None;
            return;
        }
        let observed = self.can_observe_burn();
        if !observed {
            tracing::debug!(
                "the definition has no {BURN_PENDING_CHANNEL} bit; settling for {} ms",
                BURN_SETTLE.as_millis()
            );
        }
        self.burn_wait = Some(BurnWait {
            next_at: now
                + if observed {
                    BURN_POLL_INTERVAL
                } else {
                    BURN_SETTLE
                },
            deadline: now + BURN_TIMEOUT,
            observed,
        });
    }

    /// Whether the next burn check is due.
    fn burn_due(&self) -> bool {
        self.burn_wait
            .is_some_and(|wait| wait.next_at <= Instant::now())
    }

    /// Whether a reconnect attempt is due.
    fn reconnect_due(&self) -> bool {
        self.reconnect
            .as_ref()
            .is_some_and(|reconnect| reconnect.next_at <= Instant::now())
    }

    /// The summary to show for this session.
    fn summary(&self, writes_enabled: bool) -> ConnectionSummary {
        let read_only = self.verdict == SignatureVerdict::Mismatch;
        ConnectionSummary {
            port: self.link_label.clone(),
            custom: self.custom_link,
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
            writes_enabled,
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
        // A fresh handshake proves the link, not the page image: the app reads
        // every page next, and a write in between would act on stale RAM.
        self.pages_complete = false;
        self.page_bytes.clear();
    }
}

/// Stops a live session, releases its port and forgets its frame.
///
/// Every command still waiting for the wire is answered before the port is
/// closed. A session can be replaced by a project load or an explicit unload
/// while a page read is in flight; without this, that `OpId` would never get an
/// answer and the UI would wait forever.
///
/// A transaction that was in flight is failed loudly and the write path halts:
/// a closed session must never look like a successful write.
#[allow(clippy::too_many_arguments)]
fn teardown(
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

    if let Some(transaction) = live.write_tx.take() {
        let reason = "the session was closed while the write was running";
        safety.halt(reason);
        if let Some(journal) = &transaction.journal {
            let entry = JournalEntry {
                at: journal::timestamp(),
                operation: transaction.kind.journal_name().to_owned(),
                phase: "result".to_owned(),
                port: live.link_label.clone(),
                signature: live.signature.clone(),
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
    if live.logger.take().is_some() {
        tracing::info!("the session log was closed with the session");
    }
    log.mark_stopped();

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
    history.clear();
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
    let burn = live
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
fn configure_history(history: &ChartHistory, definition: &Definition) {
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
fn align_log_values(columns: &[String], frame: &RealtimeFrame) -> Vec<f64> {
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
#[allow(clippy::too_many_arguments)]
fn connect(
    op: OpId,
    session: &mut Session,
    live: &mut Option<Live>,
    events: &EventTx,
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

    let link = port.clone();
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
#[allow(clippy::too_many_arguments)]
fn service_live(
    live: &mut Option<Live>,
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

    while let Ok(reply) = connection.replies.try_recv() {
        let keep = on_reply(
            connection, reply, session, safety, events, latest, frames, history, log, report,
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
            .write_tx
            .as_ref()
            .map(|transaction| transaction.op);
        match op {
            Some(op) if !connection.pending_kind(PendingKind::Write) => {
                if let Some(wait) = connection.burn_wait.as_mut() {
                    wait.next_at = Instant::now() + BURN_POLL_INTERVAL;
                }
                if let Err(error) = connection.request(op, PendingKind::Write, IoOp::ReadRealtime) {
                    let Some(transaction) = connection.write_tx.take() else {
                        return;
                    };
                    connection.burn_wait = None;
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
            None => connection.burn_wait = None,
        }
    }

    if connection.poll_due()
        && !connection.pending_kind(PendingKind::Realtime)
        && !connection.pending_kind(PendingKind::ReadAll)
        && !connection.pending_kind(PendingKind::Write)
        && !connection.pending_kind(PendingKind::Reconnect)
        && connection.write_tx.is_none()
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

/// Moves one write or burn transaction through its read → write/burn → verify
/// sequence.
#[allow(clippy::too_many_arguments)]
fn advance_write(
    live: &mut Live,
    op: OpId,
    outcome: IoOutcome,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    latest: &LatestFrame,
    history: &ChartHistory,
    log: &LogStatus,
    report: &mut CoreReport,
) {
    let Some(mut transaction) = live.write_tx.take() else {
        tracing::warn!(%op, "a write reply arrived without a transaction");
        return;
    };
    debug_assert_eq!(transaction.op, op);

    match outcome {
        IoOutcome::Pages { pages } => match transaction.phase {
            TxnPhase::ReadBase => {
                // The bytes the tune image was built from. A fresh read that
                // disagrees with them means the ECU moved since the last read:
                // a read that raced the reboot, or a tune changed behind our
                // back. Patching would put values over a tune we never saw, so
                // stop and let the user read again (ADR-0010).
                let drifted: Vec<u8> = pages
                    .iter()
                    .filter(|read| {
                        live.page_bytes
                            .get(&read.page)
                            .is_some_and(|known| known != &read.bytes)
                    })
                    .map(|read| read.page)
                    .collect();
                transaction.base = pages
                    .iter()
                    .map(|read| (read.page, read.bytes.clone()))
                    .collect();
                for read in &pages {
                    live.page_bytes.insert(read.page, read.bytes.clone());
                }
                if !drifted.is_empty() {
                    let action = match transaction.kind {
                        TxnKind::Write => "writing",
                        TxnKind::Burn => "burning",
                    };
                    finish_refused(
                        live,
                        transaction,
                        events,
                        report,
                        &format!(
                            "page(s) {} changed on the ECU since the last read; read every page \
                             again before {action}",
                            journal::pages_label(&drifted)
                        ),
                    );
                    return;
                }

                match transaction.kind {
                    TxnKind::Write => {
                        let Some(project) = session.project_mut() else {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the project closed during the write".to_owned(),
                                false,
                            );
                            return;
                        };
                        let Some(mut image) = project.images().ecu_ram().cloned() else {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the ECU image is unknown; refusing to patch it".to_owned(),
                                false,
                            );
                            return;
                        };
                        let layout = Arc::clone(project.layout());
                        for read in &pages {
                            let Some(page) = IniPage::new(read.page) else {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {} is not a valid page number", read.page),
                                    false,
                                );
                                return;
                            };
                            if let Err(error) = decode_page(&layout, page, &read.bytes, &mut image)
                            {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {} could not be decoded: {error}", read.page),
                                    false,
                                );
                                return;
                            }
                        }
                        // The fresh read is the truth about RAM: use it for the
                        // delta instead of the image from connect time.
                        if let Err(error) = project.images_mut().set_ecu_ram(image) {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                format!("the fresh ECU image could not be stored: {error}"),
                                false,
                            );
                            return;
                        }
                        let (variables, changed_pages, _) = unsent_changes(project);
                        if variables.is_empty() {
                            finish_refused(
                                live,
                                transaction,
                                events,
                                report,
                                "the ECU already holds these values",
                            );
                            return;
                        }
                        let target_set: HashSet<u8> = transaction.pages.iter().copied().collect();
                        let targets: Vec<u8> = changed_pages
                            .into_iter()
                            .filter(|page| target_set.contains(page))
                            .collect();
                        if targets.is_empty() {
                            finish_refused(
                                live,
                                transaction,
                                events,
                                report,
                                "there are no values that differ from ECU RAM on these pages",
                            );
                            return;
                        }
                        let (variables, cells) = unsent_for_pages(project, &targets);
                        if variables.is_empty() {
                            finish_refused(
                                live,
                                transaction,
                                events,
                                report,
                                "the selected pages hold no values that still need sending",
                            );
                            return;
                        }
                        transaction.changed_variables = variables.clone();
                        transaction.cells = cells;
                        transaction.variables = variables.len();
                        transaction.pages = targets.clone();

                        let mut writes = Vec::with_capacity(targets.len());
                        for page in &targets {
                            let Some(base) = transaction.base.get(page) else {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {page} was not read before the write"),
                                    false,
                                );
                                return;
                            };
                            let Some(ini_page) = IniPage::new(*page) else {
                                continue;
                            };
                            match patch_page(
                                &layout,
                                ini_page,
                                base,
                                project.tune(),
                                &transaction.changed_variables,
                            ) {
                                Ok(bytes) => {
                                    transaction.expected.insert(*page, bytes.clone());
                                    writes.push(PageWrite {
                                        page: *page,
                                        data: bytes,
                                    });
                                }
                                Err(error) => {
                                    fail_transaction(
                                        live,
                                        transaction,
                                        session,
                                        safety,
                                        events,
                                        report,
                                        format!("page {page} could not be patched: {error}"),
                                        false,
                                    );
                                    return;
                                }
                            }
                        }

                        transaction.phase = TxnPhase::Write;
                        live.write_tx = Some(transaction);
                        if let Err(error) =
                            live.request(op, PendingKind::Write, IoOp::WritePages { pages: writes })
                        {
                            let Some(transaction) = live.write_tx.take() else {
                                return;
                            };
                            fail_transaction(
                                live,
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
                    TxnKind::Burn => {
                        let Some(project) = session.project_mut() else {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the project closed during the burn".to_owned(),
                                false,
                            );
                            return;
                        };
                        let Some(mut image) = project.images().ecu_ram().cloned() else {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the ECU image is unknown; refusing to burn".to_owned(),
                                false,
                            );
                            return;
                        };
                        let layout = Arc::clone(project.layout());
                        let burn_pages = transaction.pages.clone();
                        for page in &burn_pages {
                            let Some(bytes) = transaction.base.get(page) else {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {page} was not read before the burn"),
                                    false,
                                );
                                return;
                            };
                            let Some(ini_page) = IniPage::new(*page) else {
                                continue;
                            };
                            if let Err(error) = decode_page(&layout, ini_page, bytes, &mut image) {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {page} could not be decoded: {error}"),
                                    false,
                                );
                                return;
                            }
                        }
                        if let Err(error) = project.images_mut().set_ecu_ram(image) {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                format!("the fresh ECU image could not be stored: {error}"),
                                false,
                            );
                            return;
                        }
                        let (unsent, _) = unsent_edits(project);
                        if unsent > 0 {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the working image has edits that are not in ECU RAM. Write them first; burning would persist the older RAM values".to_owned(),
                                false,
                            );
                            return;
                        }
                        transaction.expected = transaction.base.clone();
                        let pages = if live.can_observe_burn() {
                            // One page per burn, each followed by a check that
                            // the firmware saved it before the next command.
                            transaction.pages.first().copied().into_iter().collect()
                        } else {
                            transaction.pages.clone()
                        };
                        transaction.phase = TxnPhase::Burn;
                        live.write_tx = Some(transaction);
                        if let Err(error) =
                            live.request(op, PendingKind::Write, IoOp::BurnPages { pages })
                        {
                            let Some(transaction) = live.write_tx.take() else {
                                return;
                            };
                            fail_transaction(
                                live,
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
                }
            }
            TxnPhase::Verify => {
                if !verify_pages(&mut transaction, &pages) {
                    let reason = verification_message(&transaction);
                    fail_transaction(
                        live,
                        transaction,
                        session,
                        safety,
                        events,
                        report,
                        reason,
                        false,
                    );
                    return;
                }
                succeed_transaction(live, transaction, session, safety, events, report);
            }
            TxnPhase::Write | TxnPhase::Burn | TxnPhase::BurnWait => {
                tracing::warn!(%op, "an unexpected page read arrived during a transaction");
                live.write_tx = Some(transaction);
            }
        },
        IoOutcome::Written { pages, bytes } => {
            tracing::debug!(%op, pages, bytes, "pages written; reading them back");
            start_verify(live, op, transaction, session, safety, events, report);
        }
        IoOutcome::Burned { pages } => {
            tracing::debug!(%op, pages, "pages burned; waiting for the flash write");
            // The firmware answers a burn before it writes EEPROM, so reading
            // the pages back here would only prove RAM. Wait for its queue to
            // empty first, or the link may close over an unfinished save.
            transaction.burned = transaction.burned.saturating_add(pages);
            transaction.phase = TxnPhase::BurnWait;
            live.write_tx = Some(transaction);
            live.begin_burn_wait();
            if live.burn_wait.is_none() {
                // No realtime block to sample: nothing to wait on.
                let Some(transaction) = live.write_tx.take() else {
                    return;
                };
                start_verify(live, op, transaction, session, safety, events, report);
            }
        }
        IoOutcome::Realtime { block } => match transaction.phase {
            TxnPhase::BurnWait => {
                let Some(wait) = live.burn_wait else {
                    tracing::warn!(%op, "a burn check arrived without a wait");
                    live.write_tx = Some(transaction);
                    return;
                };
                let queued = wait.observed
                    && live.decoder.channel_raw(&block, BURN_PENDING_CHANNEL) != Some(0.0);
                if queued {
                    if Instant::now() >= wait.deadline {
                        live.burn_wait = None;
                        fail_transaction(
                            live,
                            transaction,
                            session,
                            safety,
                            events,
                            report,
                            format!(
                                "the ECU still reports a pending EEPROM write after {} s. \
                                 Do not close the link: closing resets the board and drops an \
                                 unfinished save",
                                BURN_TIMEOUT.as_secs()
                            ),
                            false,
                        );
                        return;
                    }
                    let mut wait = wait;
                    wait.next_at = Instant::now() + BURN_POLL_INTERVAL;
                    live.burn_wait = Some(wait);
                    live.write_tx = Some(transaction);
                    return;
                }
                live.burn_wait = None;
                if transaction.burned < transaction.pages.len() {
                    // The next page goes out only after this one's save is
                    // accounted for.
                    let page = transaction.pages[transaction.burned];
                    transaction.phase = TxnPhase::Burn;
                    live.write_tx = Some(transaction);
                    if let Err(error) = live.request(
                        op,
                        PendingKind::Write,
                        IoOp::BurnPages { pages: vec![page] },
                    ) {
                        let Some(transaction) = live.write_tx.take() else {
                            return;
                        };
                        fail_transaction(
                            live,
                            transaction,
                            session,
                            safety,
                            events,
                            report,
                            error.to_string(),
                            false,
                        );
                    }
                    return;
                }
                tracing::debug!(%op, "the burn reached flash; reading it back");
                start_verify(live, op, transaction, session, safety, events, report);
            }
            _ => {
                tracing::warn!(%op, "an unexpected realtime block arrived during a transaction");
                live.write_tx = Some(transaction);
            }
        },
        IoOutcome::Failed(failure) => {
            let link_lost = matches!(
                failure.kind,
                IoFailureKind::LinkLost | IoFailureKind::Timeout
            );
            fail_transaction(
                live,
                transaction,
                session,
                safety,
                events,
                report,
                failure.message.clone(),
                link_lost,
            );
            if link_lost {
                connection_lost(live, session, events, latest, failure.message);
                history.clear();
                log.mark_stopped();
            }
        }
        other => {
            tracing::warn!(%op, ?other, "an unexpected reply arrived during a transaction");
            live.write_tx = Some(transaction);
        }
    }
}

/// Queues the read-back that ends a write or burn transaction.
#[allow(clippy::too_many_arguments)]
fn start_verify(
    live: &mut Live,
    op: OpId,
    mut transaction: WriteTxn,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    report: &mut CoreReport,
) {
    transaction.phase = TxnPhase::Verify;
    let pages = transaction.pages.clone();
    live.write_tx = Some(transaction);
    if let Err(error) = live.request(op, PendingKind::Write, IoOp::ReadPages { pages }) {
        let Some(transaction) = live.write_tx.take() else {
            return;
        };
        fail_transaction(
            live,
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

/// Compares each page read back with what the transaction expected.
fn verify_pages(transaction: &mut WriteTxn, pages: &[PageRead]) -> bool {
    for read in pages {
        transaction.readback.insert(read.page, read.bytes.clone());
    }
    transaction.pages.iter().all(|page| {
        let expected = transaction.expected.get(page);
        let actual = transaction.readback.get(page);
        expected.is_some() && expected == actual
    })
}

fn verification_message(transaction: &WriteTxn) -> String {
    let mismatched: Vec<String> = transaction
        .pages
        .iter()
        .filter(|page| {
            let expected = transaction.expected.get(page);
            let actual = transaction.readback.get(page);
            expected.is_none() || expected != actual
        })
        .map(|page| page.to_string())
        .collect();
    format!(
        "the read-back did not match on page {}. The ECU image was reloaded from the read-back; \
         the write path is halted until you review the tune",
        mismatched.join(", ")
    )
}

/// Applies a verified write or burn and reports it.
fn succeed_transaction(
    live: &mut Live,
    transaction: WriteTxn,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    report: &mut CoreReport,
) {
    if session.project().is_none() {
        fail_transaction(
            live,
            transaction,
            session,
            safety,
            events,
            report,
            "the project closed during the operation".to_owned(),
            false,
        );
        return;
    }

    let applied = match transaction.kind {
        TxnKind::Write => apply_verified_write(live, &transaction, session),
        TxnKind::Burn => apply_verified_burn(&transaction, session),
    };
    if let Err(reason) = applied {
        fail_transaction(
            live,
            transaction,
            session,
            safety,
            events,
            report,
            reason,
            false,
        );
        return;
    }

    match transaction.kind {
        TxnKind::Write => {
            report.writes = report.writes.saturating_add(1);
            report.verifications = report.verifications.saturating_add(1);
            record_result(&transaction, live, "ok", "read-back matched");
            send(
                events,
                Event::TuneWritten {
                    op: transaction.op,
                    summary: WriteSummary {
                        pages: transaction.pages.len(),
                        cells: transaction.cells,
                        variables: transaction.variables,
                        verified: true,
                        snapshot: transaction.snapshot.clone(),
                        journal: transaction
                            .journal
                            .as_ref()
                            .map(|journal| journal.path().to_owned()),
                    },
                },
            );
        }
        TxnKind::Burn => {
            report.burns = report.burns.saturating_add(1);
            report.verifications = report.verifications.saturating_add(1);
            record_result(&transaction, live, "ok", "read-back matched");
            send(
                events,
                Event::TuneBurned {
                    op: transaction.op,
                    summary: BurnSummary {
                        pages: transaction.pages.len(),
                        verified: true,
                        snapshot: transaction.snapshot.clone(),
                        journal: transaction
                            .journal
                            .as_ref()
                            .map(|journal| journal.path().to_owned()),
                    },
                },
            );
        }
    }

    if let Some(project) = session.project() {
        send_status(events, transaction.op, project);
    }
    live.polled();
}

/// Applies the read-back of a verified write to the ECU image.
fn apply_verified_write(
    live: &mut Live,
    transaction: &WriteTxn,
    session: &mut Session,
) -> Result<(), String> {
    let Some(project) = session.project_mut() else {
        return Err("the project closed during the write".to_owned());
    };
    let Some(mut image) = project.images().ecu_ram().cloned() else {
        return Err("the ECU image disappeared during the write".to_owned());
    };
    let layout = Arc::clone(project.layout());
    for (page, bytes) in &transaction.readback {
        let Some(ini_page) = IniPage::new(*page) else {
            continue;
        };
        decode_page(&layout, ini_page, bytes, &mut image)
            .map_err(|error| format!("the verified page {page} could not be decoded: {error}"))?;
    }
    project
        .images_mut()
        .set_ecu_ram(image)
        .map_err(|error| format!("the verified ECU image could not be stored: {error}"))?;
    project.images_mut().clear_write_failures();
    live.page_bytes.extend(
        transaction
            .readback
            .iter()
            .map(|(page, bytes)| (*page, bytes.clone())),
    );
    Ok(())
}

/// Records that flash now holds the read-back RAM image.
fn apply_verified_burn(transaction: &WriteTxn, session: &mut Session) -> Result<(), String> {
    let Some(project) = session.project_mut() else {
        return Err("the project closed during the burn".to_owned());
    };
    let burnable = project
        .definition()
        .pages
        .iter()
        .filter(|page| page.is_burnable())
        .count();
    if transaction.pages.len() < burnable {
        // Only some pages were burned; the flash image of the rest is still
        // whatever it was, so claiming the whole image is verified would be a
        // lie. The journal and the summary still record the verified pages.
        return Ok(());
    }
    let Some(ram) = project.images().ecu_ram().cloned() else {
        return Err("the ECU image disappeared during the burn".to_owned());
    };
    project
        .images_mut()
        .set_ecu_flash(ram)
        .map_err(|error| format!("the verified flash image could not be stored: {error}"))?;
    Ok(())
}

/// Ends a transaction that failed (or could not be sent), halting writes.
#[allow(clippy::too_many_arguments)]
fn fail_transaction(
    live: &mut Live,
    transaction: WriteTxn,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    report: &mut CoreReport,
    reason: String,
    link_lost: bool,
) {
    let code = transaction.kind.failure_code();
    if transaction.kind == TxnKind::Write {
        if let Some(project) = session.project_mut() {
            if !transaction.readback.is_empty()
                && let Some(mut image) = project.images().ecu_ram().cloned()
            {
                let layout = Arc::clone(project.layout());
                for (page, bytes) in &transaction.readback {
                    let Some(ini_page) = IniPage::new(*page) else {
                        continue;
                    };
                    let _ = decode_page(&layout, ini_page, bytes, &mut image);
                }
                if let Err(error) = project.images_mut().set_ecu_ram(image) {
                    tracing::warn!(%error, "the failed write's read-back could not be stored");
                }
            }
            mark_failed_cells(project, &transaction.changed_variables);
        }
        if link_lost {
            session.mark_unknown();
        }
    }
    safety.halt(reason.clone());
    report.failures = report.failures.saturating_add(1);
    if !transaction.expected.is_empty() || !transaction.readback.is_empty() {
        report.verification_failures = report.verification_failures.saturating_add(1);
    }
    tracing::warn!(
        op = %transaction.op,
        operation = transaction.kind.event(),
        %reason,
        link_lost,
        "write transaction failed"
    );
    record_result(&transaction, live, "failed", &reason);
    send(
        events,
        Event::Failed {
            op: transaction.op,
            failure: Failure {
                code,
                message: reason,
            },
        },
    );
    live.polled();
}

/// Marks every cell of the changed variables as a failed write.
fn mark_failed_cells(project: &mut Project, names: &[String]) {
    let mut cells: Vec<(String, usize, usize)> = Vec::new();
    for name in names {
        let Some(spec) = project.layout().variable(name) else {
            continue;
        };
        let (columns, rows) = spec.shape.dimensions().unwrap_or((1, 1));
        for row in 0..rows {
            for column in 0..columns {
                cells.push((name.clone(), row, column));
            }
        }
    }
    for (name, row, column) in cells {
        if let Err(error) = project.images_mut().mark_write_failed(&name, row, column) {
            tracing::warn!(%name, %error, "a failed cell could not be marked");
        }
    }
}

/// Records the result line for a transaction's journal, best effort.
fn record_result(transaction: &WriteTxn, live: &Live, result: &str, detail: &str) {
    let Some(journal) = &transaction.journal else {
        return;
    };
    let entry = JournalEntry {
        at: journal::timestamp(),
        operation: transaction.kind.journal_name().to_owned(),
        phase: "result".to_owned(),
        port: live.link_label.clone(),
        signature: live.signature.clone(),
        pages: journal::pages_label(&transaction.pages),
        cells: transaction.cells,
        variables: transaction.variables,
        result: result.to_owned(),
        detail: detail.to_owned(),
    };
    if let Err(error) = journal.append(&entry) {
        tracing::warn!(%error, "the journal result could not be written");
    }
}

/// Ends a transaction before the wire saw it, without halting.
fn finish_refused(
    live: &mut Live,
    transaction: WriteTxn,
    events: &EventTx,
    report: &mut CoreReport,
    reason: &str,
) {
    report.failures = report.failures.saturating_add(1);
    record_result(&transaction, live, "refused", reason);
    send(
        events,
        Event::Failed {
            op: transaction.op,
            failure: Failure {
                code: FailureCode::WriteRefused,
                message: reason.to_owned(),
            },
        },
    );
    live.polled();
}

/// Handles one I/O reply. Returns `false` when the session is finished.
#[allow(clippy::too_many_arguments)]
fn on_reply(
    live: &mut Live,
    reply: IoReply,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    latest: &LatestFrame,
    frames: &FrameNotify,
    history: &ChartHistory,
    log: &LogStatus,
    report: &mut CoreReport,
) -> bool {
    let Some(pending) = live.pending.remove(&reply.id) else {
        tracing::debug!(id = reply.id, "an I/O reply had no pending request");
        return true;
    };

    match (pending.kind, reply.outcome) {
        (PendingKind::Connect, IoOutcome::Signature { signature }) => {
            live.set_signature(signature);
            let summary = live.summary(safety.writes_enabled());
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
                    live.page_bytes = pages
                        .iter()
                        .map(|read| (read.page, read.bytes.clone()))
                        .collect();
                    live.pages_complete = summary.is_complete();
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
            let first = *live.first_frame.get_or_insert_with(Instant::now);
            let time = first.elapsed().as_secs_f64();
            live.last_frame_time = time;
            let named: Vec<(&str, f64)> = frame
                .values
                .iter()
                .map(|value| (value.name.as_str(), value.value))
                .collect();
            history.push_frame(time, &named);
            if let Some(logger) = &live.logger {
                let values = align_log_values(&live.log_columns, &frame);
                if !logger.try_log(time, &values) {
                    report.log_drops = report.log_drops.saturating_add(1);
                }
            }
            latest.set(frame);
            // A wake, not an event: acquisition must never wait for the UI
            // (ADR-0021). One pending token covers every frame that arrived
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
        PendingKind::Write => FailureCode::WriteFailed,
        PendingKind::Disconnect | PendingKind::Reconnect => FailureCode::NotConnected,
    }
}

/// Marks the session as lost and schedules a reconnect.
fn connection_lost(
    live: &mut Live,
    session: &mut Session,
    events: &EventTx,
    latest: &LatestFrame,
    reason: String,
) {
    tracing::warn!(%reason, "the ECU link was lost");
    live.connected = false;
    live.stop_polling();
    session.mark_unknown();
    latest.clear();
    // A lost link ends the log's input; close it cleanly rather than leave an
    // open file that grows only when a reconnect happens.
    if live.logger.take().is_some() {
        tracing::info!("the session log was closed because the link was lost");
    }

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
