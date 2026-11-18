//! The core thread: one owner of the loaded definition, one command loop.
//!
//! ```text
//! UI ──CommandRequest──▶ [commands queue] ──▶ core thread ──▶ [events queue] ──Event──▶ UI
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
use oxtuner_project::{DefinitionSource, Project};
use oxtuner_protocol::{BehaviorKind, LinkMode};
use oxtuner_tune::{IniPage, Tune, TuneDiff, TuneStatus, decode_page, patch_page};

use crate::contract::{
    AxisEdit, BurnReport, CellEdit, CommandRequest, ConflictResolution, ConnectionView,
    ControllerActionReport, DefinitionView, DialogView, Envelope, Event, Failure, FailureCode,
    ImportReport, JournalEntry, OpId, ProjectView, ReadReport, RevertReport, SaveReport,
    SignatureVerdict, TableView, WriteReport,
};
use crate::gate::GateEvaluator;
use crate::handle::{CoreHandle, EventStream, EventTx, SendFailure, SendGate, StopFlag};
use crate::history::{ChannelInfo, ChartHistory};
use crate::io_thread::{
    IoFailureKind, IoOp, IoOutcome, IoPlan, IoReply, IoThread, PageRead, PageWrite, RequestId,
};
use crate::journal::{self, Journal};
use crate::link::PortChoice;
use crate::menu::MenuNode;
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
    /// does not fit is dropped and counted instead of blocking.
    pub event_capacity: usize,
    /// Whether the global write switch starts on. Off unless the user
    /// enabled it in the settings and the app passed that choice in.
    pub writes_enabled: bool,
    /// Where a standalone definition's write journal and logs live. A project
    /// always uses its own `.oxtuner/` sidecar.
    pub journal_dir: Option<PathBuf>,
    /// Where parsed definitions are cached. `None` disables the cache, which is
    /// what tests and tools get unless they ask for it; the application passes
    /// its own cache directory.
    pub definition_cache_dir: Option<PathBuf>,
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
            definition_cache_dir: None,
            history_capacity: 6_000,
        }
    }
}

/// A command queue that cannot take the command right now.
#[non_exhaustive]
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
    ///
    /// The protocol cause is flattened to its display text because nothing
    /// downstream matches on it. Retry or reporting code that needs the
    /// concrete error would box the cause instead, a change left until a
    /// caller actually needs it.
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
    /// Commands that failed, so the shutdown report shows how much went wrong.
    ///
    /// A silent probe counts too: a scan that finds no ECU reports one miss
    /// per attempt, so a miss-heavy shutdown is an empty bench, not a broken
    /// core.
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
    /// and it had stopped reading.
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

/// Queues a [`CommandRequest::Shutdown`], waiting up to [`SHUTDOWN_WAIT`] for room.
///
/// Returns whether the core will see the command (or is already gone).
pub(crate) fn send_shutdown(
    commands: &MTx<mpsc::Array<Envelope>>,
    op: OpId,
) -> Result<(), CoreError> {
    match commands.send_timeout(
        Envelope {
            op,
            command: CommandRequest::Shutdown,
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
    /// The newest realtime frame, read without an event round trip.
    pub latest: LatestFrame,
    /// Wakes the UI when a newer frame is readable. Coalesced, never blocking.
    pub frames: FrameWatch,
    /// The bounded realtime history the charts read.
    pub history: ChartHistory,
    /// The session log's live counters, when one is running.
    pub log: LogStatus,
    /// The thread, which joins on shutdown.
    pub thread: CoreThread,
}

impl std::fmt::Debug for CoreParts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("CoreParts").finish_non_exhaustive()
    }
}

/// Owns the core thread and joins it on drop.
///
/// Dropping is a safe shutdown: it queues [`CommandRequest::Shutdown`], raises the stop
/// flag so a full event queue cannot hold the thread, and joins, so a forgotten
/// core is stopped and reaped and [`CoreThread::join`] returns its report. One
/// policy is explicit rather than pretty: a core that refuses shutdown is
/// wedged, and a wedged thread is abandoned — loudly, after [`SHUTDOWN_WAIT`] +
/// [`JOIN_WAIT`] — instead of hanging the caller forever. Nothing is ever
/// detached silently.
///
/// The trade-off is quit latency. Dropping waits for the command in flight to
/// finish, so a slow ECU conversation can hold the caller for up to that
/// conversation's own I/O deadline. The wait is deliberate: leaving the port
/// open while the process tears down is worse than a short pause.
pub struct CoreThread {
    join: Option<JoinHandle<CoreReport>>,
    commands: MTx<mpsc::Array<Envelope>>,
    stop: Arc<StopFlag>,
}

impl std::fmt::Debug for CoreThread {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoreThread")
            .field("finished", &self.join.is_none())
            .finish_non_exhaustive()
    }
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
        // queue. The core thread may be blocked handing an answer to
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
///
/// # Errors
///
/// Returns [`CoreError::Spawn`] when the thread cannot be created.
pub fn start(config: &CoreConfig) -> Result<CoreParts, CoreError> {
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
    let gate = Arc::new(SendGate::default());
    let core_gate = Arc::clone(&gate);
    let latest = LatestFrame::default();
    let history = ChartHistory::new(config.history_capacity);
    let log = LogStatus::new();
    let safety = Safety::new(config.writes_enabled);
    let journal_dir = config.journal_dir.clone();
    let definition_cache_dir = config.definition_cache_dir.clone();
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
                definition_cache_dir,
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
/// Waits at most one poll interval while idle, because the port list is sampled
/// here rather than on a thread of its own. With a live connection the nearer of
/// its timers and the poll sets the wake.
#[expect(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    reason = "the thread loop owns its channels and shared views for the life of the core"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the loop keeps the command, menu refresh and poll ordering in one place"
)]
fn run(
    commands: Rx<mpsc::Array<Envelope>>,
    events: EventTx,
    gate: Arc<SendGate>,
    latest: LatestFrame,
    history: ChartHistory,
    log: LogStatus,
    frames: FrameNotify,
    mut safety: Safety,
    journal_dir: Option<PathBuf>,
    definition_cache_dir: Option<PathBuf>,
) -> CoreReport {
    let started = Instant::now();
    let mut report = CoreReport::default();
    let mut session = Session::Empty;
    let mut live: Option<Live> = None;
    // The last menu tree the core published, for change suppression.
    let mut last_menu: Vec<MenuNode> = Vec::new();
    let mut menu_dirty = false;
    // Port presence is sampled on this loop, not on a thread of its own.
    let mut ports = ports::PortsWatch::new();

    // Every handle is gone (`Err`) also ends the loop, which is what makes a
    // dropped UI a shutdown rather than a leak.
    loop {
        let wait = match next_wait(live.as_ref()) {
            Some(wait) => wait.min(ports.remaining()),
            None => ports.remaining(),
        };

        let envelope = match commands.recv_timeout(wait) {
            Ok(envelope) => Some(envelope),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        if let Some(envelope) = envelope {
            let op_started = Instant::now();
            let outcome = handle(
                envelope,
                &mut session,
                &mut live,
                &mut safety,
                journal_dir.as_deref(),
                definition_cache_dir.as_deref(),
                &events,
                &latest,
                &history,
                &log,
                &mut report,
                &mut last_menu,
                &mut menu_dirty,
                &mut ports,
            );
            let took = op_started.elapsed();

            report.total_op_latency += took;
            report.max_op_latency = report.max_op_latency.max(took);

            if outcome == Outcome::Stop {
                break;
            }
            report.handled += 1;
        }

        if menu_dirty {
            refresh_menu(&events, &session, &latest, &mut last_menu);
            menu_dirty = false;
        }

        // Replies and timers are serviced after every command and every wait,
        // so a poll or a reconnect never depends on another command arriving.
        let connection_revision = live
            .as_ref()
            .map(|connection| connection.connection.revision);
        let had_frame = latest.get().is_some();
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
        let connection_changed = live
            .as_ref()
            .map(|connection| connection.connection.revision)
            != connection_revision;
        let first_frame = !had_frame && latest.get().is_some();

        // Sampled here rather than on its own thread: the list is published only
        // when it differs from the last one published, including the first
        // sample when it found ports, so a view that opens later renders the
        // last published list without asking first.
        if ports.due() {
            if let Some(updated) = ports.sample() {
                tracing::debug!(ports = updated.len(), "serial ports changed");
                send(&events, Event::PortsChanged { ports: updated });
            }

            ports.polled();
        }

        if menu_dirty || connection_changed || first_frame {
            refresh_menu(&events, &session, &latest, &mut last_menu);
            menu_dirty = false;
        }
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
    // explicitly: cancellation is never implicit.
    while let Ok(envelope) = commands.try_recv() {
        if matches!(envelope.command, CommandRequest::Shutdown) {
            continue;
        }
        report.rejected += 1;
        send(
            &events,
            Event::Rejected {
                op: envelope.op,
                reason: crate::contract::RejectReason::ShuttingDown,
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
    /// A `TunerStudio` project: definition, tune images and the file they came
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

    /// Everything unproven on the ECU side becomes unknown.
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

/// The text of a panic payload, which is `Box<dyn Any>` and not printable.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return message;
    }
    payload
        .downcast_ref::<String>()
        .map_or("unknown panic", String::as_str)
}

/// The default realtime poll rate.
///
/// 20 Hz is the default the settings ship with; the charts and the logger read
/// the same polled frames rather than running a second schedule.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The slowest and fastest accepted poll intervals.
///
/// These are the intervals for [`MAX_POLL_HZ`](crate::MAX_POLL_HZ) and
/// [`MIN_POLL_HZ`](crate::MIN_POLL_HZ); the test below keeps them in step.
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
    /// One `[ControllerCommands]` action on the wire.
    Command,
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
/// `IoOp::BurnPages`, which is what makes "no auto-burn" structural.
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

/// One controller command action in flight.
///
/// Kept beside the write transaction so the reply can close its journal record
/// with the same evidence path a write uses.
#[derive(Debug)]
struct CommandTxn {
    dialog: String,
    name: String,
    journal: Journal,
}

/// A serial connection a project should reopen next time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SavedConnection {
    port: String,
    baud: u32,
}

/// The transport and handshake state of one live session.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the connection tracks independent link flags"
)]
struct EcuConnection {
    thread: IoThread,
    replies: Rx<spsc::Array<IoReply>>,
    link_label: String,
    /// What to remember in the project's state once the handshake succeeds.
    ///
    /// Only a serial link names a connection a later session can reopen; a
    /// custom link belongs to the application and is never persisted.
    saved_connection: Option<SavedConnection>,
    custom_link: bool,
    definition_signature: String,
    behavior: String,
    framing: &'static str,
    pages: usize,
    realtime_block: Option<u16>,
    connected: bool,
    auto_poll: bool,
    probe: bool,
    signature: String,
    verdict: SignatureVerdict,
    revision: u64,
}

/// Conversations waiting for the wire.
struct RequestTable {
    pending: BTreeMap<u64, Pending>,
    next_id: RequestId,
}

/// The transactions that own the link.
struct LiveTransactions {
    write_tx: Option<WriteTxn>,
    command_tx: Option<CommandTxn>,
    burn_wait: Option<BurnWait>,
}

/// Decoded realtime state.
struct EcuTelemetry {
    decoder: RealtimeDecoder,
    sequence: u64,
    first_frame: Option<Instant>,
    last_frame_time: f64,
}

/// The running session log.
struct SessionLogState {
    logger: Option<Logger>,
    log_columns: Vec<String>,
}

/// The page image and the ECU image when no project owns one.
struct PageImageState {
    standalone: Option<Box<Tune>>,
    page_bytes: BTreeMap<u8, Vec<u8>>,
    pages_complete: bool,
}

/// Poll and reconnect timers.
struct LinkTimers {
    poll: Option<Poll>,
    reconnect: Option<Reconnect>,
}

/// A live ECU session, grouped by the lifecycle of its parts.
struct Live {
    connection: EcuConnection,
    requests: RequestTable,
    txns: LiveTransactions,
    telemetry: EcuTelemetry,
    logging: SessionLogState,
    images: PageImageState,
    timers: LinkTimers,
}

impl Live {
    fn new(
        thread: IoThread,
        replies: Rx<spsc::Array<IoReply>>,
        link: &PortChoice,
        definition: &Definition,
        behavior: BehaviorKind,
        framing: LinkMode,
        standalone: Option<Box<Tune>>,
    ) -> Self {
        let realtime_block = definition
            .comms
            .och_block_size
            .and_then(|size| u16::try_from(size).ok())
            .filter(|size| *size > 0);
        Self {
            connection: EcuConnection {
                thread,
                replies,
                link_label: link.label(),
                saved_connection: match link {
                    PortChoice::Serial { port, baud } => Some(SavedConnection {
                        port: port.clone(),
                        baud: *baud,
                    }),
                    PortChoice::Custom(_) => None,
                },
                custom_link: !link.is_serial(),
                definition_signature: definition.signature.clone(),
                behavior: behavior.as_dyn().id().to_owned(),
                framing: match framing {
                    LinkMode::Legacy => "legacy",
                    LinkMode::Envelope => "envelope",
                },
                pages: definition.pages.len(),
                realtime_block,
                connected: false,
                auto_poll: true,
                probe: false,
                signature: String::new(),
                verdict: SignatureVerdict::Mismatch,
                revision: 0,
            },
            requests: RequestTable {
                pending: BTreeMap::new(),
                next_id: RequestId::FIRST,
            },
            txns: LiveTransactions {
                write_tx: None,
                command_tx: None,
                burn_wait: None,
            },
            telemetry: EcuTelemetry {
                decoder: RealtimeDecoder::new(definition),
                sequence: 0,
                first_frame: None,
                last_frame_time: 0.0,
            },
            logging: SessionLogState {
                logger: None,
                log_columns: Vec::new(),
            },
            images: PageImageState {
                standalone,
                page_bytes: BTreeMap::new(),
                pages_complete: false,
            },
            timers: LinkTimers {
                poll: None,
                reconnect: None,
            },
        }
    }

    /// Queues one request and records what its answer will mean.
    fn request(&mut self, op: OpId, kind: PendingKind, io_op: IoOp) -> Result<(), CoreError> {
        let id = self.requests.next_id;
        self.connection.thread.submit(id.0, io_op)?;
        self.requests.next_id = self.requests.next_id.next();
        self.requests.pending.insert(id.0, Pending { op, kind });
        Ok(())
    }

    /// Records the transaction the next replies belong to.
    fn begin_transaction(&mut self, transaction: WriteTxn) {
        self.txns.write_tx = Some(transaction);
    }

    /// Forgets a transaction whose first request could not be queued.
    fn abort_transaction(&mut self, reason: &str) {
        let Some(transaction) = self.txns.write_tx.take() else {
            return;
        };
        if let Some(journal) = &transaction.journal {
            let entry = JournalEntry {
                at: journal::timestamp(),
                operation: transaction.kind.journal_name().to_owned(),
                phase: "result".to_owned(),
                port: self.connection.link_label.clone(),
                signature: self.connection.signature.clone(),
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
        self.requests
            .pending
            .values()
            .any(|pending| pending.kind == kind)
    }

    /// Starts (or changes) the poll schedule.
    fn start_polling(&mut self, interval_ms: u32) {
        let interval = Duration::from_millis(u64::from(interval_ms))
            .clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL);
        self.timers.poll = Some(Poll {
            interval,
            next_at: Instant::now(),
        });
    }

    /// Stops polling; the last frame stays readable.
    fn stop_polling(&mut self) {
        self.timers.poll = None;
    }

    /// Whether a poll is due.
    fn poll_due(&self) -> bool {
        self.timers
            .poll
            .as_ref()
            .is_some_and(|poll| poll.next_at <= Instant::now())
    }

    /// Moves the poll schedule on.
    fn polled(&mut self) {
        if let Some(poll) = &mut self.timers.poll {
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
        self.connection.realtime_block.is_some()
            && self.telemetry.decoder.has_channel(BURN_PENDING_CHANNEL)
    }

    /// Starts waiting for the firmware to write an accepted burn to flash.
    fn begin_burn_wait(&mut self) {
        let now = Instant::now();
        if self.connection.realtime_block.is_none() {
            // Nothing to sample: the read-back runs at once, as a burn without
            // a status bit always did.
            self.txns.burn_wait = None;
            return;
        }
        let observed = self.can_observe_burn();
        if !observed {
            tracing::debug!(
                "the definition has no {BURN_PENDING_CHANNEL} bit; settling for {} ms",
                BURN_SETTLE.as_millis()
            );
        }
        self.txns.burn_wait = Some(BurnWait {
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
        self.txns
            .burn_wait
            .is_some_and(|wait| wait.next_at <= Instant::now())
    }

    /// Whether a reconnect attempt is due.
    fn reconnect_due(&self) -> bool {
        self.timers
            .reconnect
            .as_ref()
            .is_some_and(|reconnect| reconnect.next_at <= Instant::now())
    }

    /// The summary to show for this session.
    fn summary(&self, writes_enabled: bool) -> ConnectionView {
        let read_only = self.connection.verdict == SignatureVerdict::Mismatch;
        ConnectionView {
            port: self.connection.link_label.clone(),
            custom: self.connection.custom_link,
            signature: self.connection.signature.clone(),
            definition_signature: self.connection.definition_signature.clone(),
            verdict: self.connection.verdict,
            read_only,
            read_only_reason: read_only.then(|| {
                format!(
                    "the ECU reports {:?}, the definition expects {:?}",
                    self.connection.signature, self.connection.definition_signature
                )
            }),
            writes_enabled,
            behavior: self.connection.behavior.clone(),
            framing: self.connection.framing,
            pages: self.connection.pages,
            realtime_block: self.connection.realtime_block,
        }
    }

    /// Re-evaluates the signature verdict after a handshake.
    fn set_signature(&mut self, signature: String) {
        self.connection.verdict = if signature.trim() == self.connection.definition_signature.trim()
        {
            SignatureVerdict::Match
        } else {
            SignatureVerdict::Mismatch
        };
        self.connection.signature = signature;
        self.connection.connected = true;
        self.connection.revision = self.connection.revision.wrapping_add(1);
        self.timers.reconnect = None;
        // A fresh handshake proves the link, not the page image: the app reads
        // every page next, and a write in between would act on stale RAM.
        self.images.pages_complete = false;
        self.images.page_bytes.clear();
    }
}

mod dispatch;
mod ecu;
mod log;
mod ports;
mod probe;
mod transfer;
mod tune;

use dispatch::{handle, refresh_menu, send};
use ecu::{next_wait, service_live, teardown};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poll_interval_limits_match_the_published_rate_range() {
        assert_eq!(
            u32::try_from(MIN_POLL_INTERVAL.as_millis()).unwrap_or(u32::MAX),
            1_000 / crate::MAX_POLL_HZ
        );
        assert_eq!(
            u32::try_from(MAX_POLL_INTERVAL.as_millis()).unwrap_or(u32::MAX),
            1_000 / crate::MIN_POLL_HZ
        );
    }
}
