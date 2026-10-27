//! The core thread: one owner of the loaded definition, one command loop.
//!
//! ```text
//! UI ──Command──▶ [commands queue] ──▶ core thread ──▶ [events queue] ──Event──▶ UI
//!                                       owns the session
//! ```
//!
//! What this thread owns today is the **session** — nothing, a standalone
//! definition, or an open project (definition + tune images) — and the lifecycle
//! around it. The transport, the realtime poller and the ECU-side images arrive
//! with M5/M7/M8 and land **here**, not in the UI — that is the point of the
//! crate (`docs/architecture/crates/oxtuner-core.md`). Nothing in this module is
//! a stub for them: the core is complete for what it claims to do.

use std::{
    io,
    sync::{Arc, atomic::AtomicU64},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossfire::{MRx, MTx, mpmc};
use oxtuner_ini::Definition;
use oxtuner_project::Project;

use crate::handle::{CoreHandle, EventStream, Gate};
use crate::message::{
    AxisEdit, CellEdit, Command, DefinitionSummary, DialogSummary, Envelope, Event, Failure,
    FailureCode, OpId, ProjectSummary, SaveSummary, TableSummary,
};

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
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => formatter.write_str("the core is busy; the command was refused"),
            Self::Stopped => formatter.write_str("the core has stopped"),
            Self::Spawn(error) => write!(formatter, "could not start the core thread: {error}"),
        }
    }
}

impl std::error::Error for CoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::QueueFull | Self::Stopped => None,
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

    let join = thread::Builder::new()
        .name("oxtuner-core".to_owned())
        .spawn(move || run(command_rx, event_tx, core_gate))
        .map_err(CoreError::Spawn)?;

    tracing::debug!("core thread started");

    Ok(CoreParts {
        handle: CoreHandle {
            commands: commands.clone(),
            next_op,
            gate,
        },
        events: EventStream { events },
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
) -> CoreReport {
    let started = Instant::now();
    let mut report = CoreReport::default();
    let mut session = Session::Empty;

    // Every handle is gone (`Err`) also ends the loop, which is what makes a
    // dropped UI a shutdown rather than a leak.
    while let Ok(envelope) = commands.recv() {
        let op_started = Instant::now();
        let outcome = handle(envelope, &mut session, &events, &mut report);
        let took = op_started.elapsed();

        report.total_op_latency += took;
        report.max_op_latency = report.max_op_latency.max(took);

        if outcome == Outcome::Stop {
            break;
        }
        report.handled += 1;
    }

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
    /// Owned for the commands that will read it (the protocol layer, M5); today
    /// only the load summary is derived from it.
    Definition(
        #[expect(
            dead_code,
            reason = "owned for the M5 protocol layer; only the summary is read today"
        )]
        Box<Definition>,
    ),
    /// A TunerStudio project: definition, tune images and the file they came
    /// from.
    ///
    /// Boxed: a `Project` is a few kilobytes of tune images, and the session
    /// lives on the core thread's stack for the whole run — it must not inflate
    /// the enum every command frame touches.
    Project(Box<Project>),
}

fn handle(
    envelope: Envelope,
    session: &mut Session,
    events: &MTx<mpmc::Array<Event>>,
    report: &mut CoreReport,
) -> Outcome {
    let Envelope { op, command } = envelope;

    match command {
        Command::Shutdown => {
            tracing::debug!(%op, "shutdown requested");
            Outcome::Stop
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
                    // dropped here, never handed back to a caller.
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
