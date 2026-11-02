//! The headless core, as the app sees it.
//!
//! `oxtuner-core` owns the work; this module owns the *relationship*: it starts
//! the core once per process, keeps the handle and the thread, and folds
//! incoming events into a small global the views can render.
//!
//! ```text
//! File ▸ Open Project…  ──send──▶  core thread  ──event──▶  CoreService
//! File ▸ Open ECU Definition…                                    │
//! File ▸ Save Tune…                                              │
//!                                                    Output panel ┘
//! ```
//!
//! The event loop runs on GPUI's foreground executor, awaiting the crossfire
//! async receiver: no polling tick, no second runtime, and no `Send` bound on
//! the task (ADR-0005). The `gpui*` → core direction is a plain channel send,
//! so nothing core-side knows a UI exists.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::{App, Global, PathPromptOptions};
use oxtuner_core::{
    AxisEdit, AxisKind, BurnSummary, CellEdit, ChartHistory, ConnectionSummary, CoreConfig,
    CoreHandle, CoreParts, CoreThread, DefinitionSummary, DialogSummary, Event, EventStream,
    Failure, FailureCode, FrameWatch, ImportSummary, JournalEntry, LatestFrame, LinkMode,
    LogSnapshot, LogStatus, LogSummary, PortChoice, PortSummary, ProjectSummary, ReadAllSummary,
    RealtimeFrame, RevertSummary, SaveSummary, TableSummary, TuneStatus, WriteSummary,
};
use rust_i18n::t;

/// What the views show about the core. Small and `Clone`: rendering reads it
/// every frame.
#[derive(Debug, Clone, Default)]
pub(crate) struct CoreStatus {
    /// A definition loaded on its own (no tune file).
    pub(crate) definition: Option<DefinitionSummary>,
    /// The project the core currently owns, when one is open.
    pub(crate) project: Option<ProjectSummary>,
    /// The last successful save, shown until the next open or failure.
    pub(crate) last_save: Option<SaveSummary>,
    /// The table the UI last read, kept until another table replaces it.
    pub(crate) table: Option<TableSummary>,
    /// The dialog the UI last read, kept until another dialog replaces it.
    pub(crate) dialog: Option<DialogSummary>,
    /// The last command failure, kept until a command succeeds.
    pub(crate) failure: Option<Failure>,
    /// Bumped whenever an edit or a save may have moved tune values, so a view
    /// can tell that copies it kept are no longer trustworthy.
    pub(crate) tune_revision: u64,
    /// The ECU session, when one is up.
    pub(crate) connection: Option<ConnectionSummary>,
    /// Why the link died, until a reconnect succeeds.
    pub(crate) link_lost: Option<String>,
    /// The last complete page read.
    pub(crate) pages_read: Option<ReadAllSummary>,
    /// Whether realtime polling is running.
    pub(crate) realtime: bool,
    /// The last saved session trace.
    pub(crate) last_trace: Option<TraceSaved>,
    /// The serial ports the last scan found.
    pub(crate) ports: Vec<PortSummary>,
    /// Whether a port scan is running.
    pub(crate) scanning: bool,
    /// The port a signature test is running against.
    pub(crate) testing: Option<String>,
    /// The last signature test.
    pub(crate) test: Option<TestResult>,
    /// The connection request that is still being answered.
    pub(crate) pending_connect: Option<PortChoice>,
    /// Whether the global write switch is on (S8).
    pub(crate) writes_enabled: bool,
    /// Why the write path is halted after a failed verification, until the user
    /// acknowledges it.
    pub(crate) write_halted: Option<String>,
    /// The derived write state after the last read, edit or write.
    pub(crate) tune_status: Option<TuneStatus>,
    /// The last verified RAM write.
    pub(crate) last_write: Option<WriteSummary>,
    /// The last verified burn.
    pub(crate) last_burn: Option<BurnSummary>,
    /// The last import or snapshot restore.
    pub(crate) last_import: Option<ImportSummary>,
    /// The last revert.
    pub(crate) last_revert: Option<RevertSummary>,
    /// The project's write journal, after `ReadJournal`.
    pub(crate) journal: Option<JournalView>,
    /// The project's snapshots, after `ListSnapshots`.
    pub(crate) snapshots: Vec<PathBuf>,
    /// A tune path waiting for the user's consent to import it (S9).
    pub(crate) import_consent: Option<PathBuf>,
    /// The last stopped session log, for the output panel.
    pub(crate) last_log: Option<LogSnapshot>,
    /// Moves when a definition or project is installed, so views can rebuild
    /// their channel lists.
    pub(crate) definition_revision: u64,
}

/// The journal file and its newest entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalView {
    /// The file, when the session has one.
    pub(crate) path: Option<PathBuf>,
    /// Newest last.
    pub(crate) entries: Vec<JournalEntry>,
}

/// The outcome of a signature test.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TestResult {
    /// The port the test ran against.
    pub(crate) port: String,
    /// The summary, or the failure message.
    pub(crate) outcome: Result<ConnectionSummary, String>,
}

/// A saved session trace, for the output panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraceSaved {
    /// Where it was written.
    pub(crate) path: PathBuf,
    /// Frames recorded.
    pub(crate) frames: usize,
}

impl CoreStatus {
    /// The definition to name, whether it came from a project or on its own.
    pub(crate) fn definition(&self) -> Option<&DefinitionSummary> {
        self.project
            .as_ref()
            .map(|project| &project.definition)
            .or(self.definition.as_ref())
    }
}

/// The core handle, its thread, and the last status.
///
/// One global rather than three: they are only meaningful together, and a view
/// must never render a summary from a different core than the one running.
pub(crate) struct CoreService {
    /// `None` when the core could not be started: the app still runs, and every
    /// command reports that instead of panicking.
    handle: Option<CoreHandle>,
    thread: Option<CoreThread>,
    status: CoreStatus,
    /// The freshest realtime frame (ADR-0014).
    latest: LatestFrame,
    /// The bounded realtime history the chart reads (ADR-0014/0015).
    history: ChartHistory,
    /// The live session log counters.
    log_status: LogStatus,
    /// The tune path waiting for the user's import consent (S9).
    pending_import: Option<PathBuf>,
    /// `--sim`: connect to the simulator as soon as a definition is loaded.
    auto_sim: bool,
}

impl Global for CoreService {}

impl CoreService {
    /// The status to render. Callers run after [`start`], which installs the
    /// global before the first window is created.
    pub(crate) fn status(cx: &App) -> &CoreStatus {
        &cx.global::<Self>().status
    }

    /// A handle for sending commands, or `None` when the core failed to start.
    ///
    /// The app keeps running without a core (see [`start`]); a command sent
    /// then must report that, not panic.
    pub(crate) fn handle(cx: &App) -> Option<CoreHandle> {
        cx.global::<Self>().handle.clone()
    }

    /// The newest realtime frame, for the live-values view.
    pub(crate) fn latest(cx: &App) -> Option<Arc<RealtimeFrame>> {
        cx.global::<Self>().latest.get()
    }

    /// The bounded realtime history the chart reads on its render tick.
    pub(crate) fn history(cx: &App) -> ChartHistory {
        cx.global::<Self>().history.clone()
    }

    /// The live session log counters.
    pub(crate) fn log(cx: &App) -> LogSnapshot {
        cx.global::<Self>().log_status.snapshot()
    }
}

/// Starts the core and the task that carries its events to the views.
///
/// The global is installed either way: views read it every frame, and a missing
/// core is a status to render, not a reason to panic. When the core does not
/// start, its handle is `None` and every command reports a failure.
pub fn start(cx: &mut App, auto_sim: bool) {
    let writes_enabled = crate::settings::preferences::AppSettings::writes_enabled(cx);
    let journal_dir = crate::platform::paths::AppPaths::discover()
        .config_dir
        .join("write-journal.log");
    let config = CoreConfig {
        writes_enabled,
        journal_dir: Some(journal_dir),
        ..CoreConfig::default()
    };
    let service = match oxtuner_core::start(config) {
        Ok(CoreParts {
            handle,
            events,
            latest,
            frames,
            history,
            log,
            thread,
        }) => {
            spawn_event_pump(events, frames, cx);
            CoreService {
                handle: Some(handle),
                thread: Some(thread),
                status: CoreStatus {
                    writes_enabled,
                    ..CoreStatus::default()
                },
                latest,
                history,
                log_status: log,
                pending_import: None,
                auto_sim,
            }
        }
        Err(error) => {
            // Without a core the app still runs: the commands report that the
            // core is not running, and nothing claims to be loaded.
            tracing::error!(%error, "the core did not start");
            CoreService {
                handle: None,
                thread: None,
                status: CoreStatus::default(),
                latest: LatestFrame::default(),
                history: ChartHistory::default(),
                log_status: LogStatus::new(),
                pending_import: None,
                auto_sim,
            }
        }
    };

    cx.set_global(service);
}

fn spawn_event_pump(events: EventStream, frames: FrameWatch, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Some(event) = events.recv().await {
            let stopped = matches!(event, Event::Stopped);
            cx.update(|cx| apply(event, cx));
            if stopped {
                break;
            }
        }
        tracing::debug!("the core event pump finished");
    })
    .detach();

    // Realtime wakes are separate from the event queue and coalesced, so a
    // frame can never queue behind a command answer or block the core
    // (ADR-0021). Repainting reads LatestFrame and the chart history.
    cx.spawn(async move |cx| {
        while frames.changed().await {
            cx.update(|cx| cx.refresh_windows());
        }
        tracing::debug!("the realtime wake watch finished");
    })
    .detach();
}

/// Folds one event into the status and repaints.
fn apply(event: Event, cx: &mut App) {
    // The port list is stored for the dialog, which stays open while it scans:
    // closing and reopening would lose the user's selection and scroll.
    if let Event::PortsListed { ports, .. } = event {
        let service = cx.global_mut::<CoreService>();
        service.status.ports = ports;
        service.status.scanning = false;
        cx.refresh_windows();
        return;
    }

    let (update, remember) = match event {
        Event::DefinitionLoaded { summary, .. } => {
            tracing::info!(signature = %summary.signature, "definition loaded");
            (Update::Loaded(summary), None)
        }
        Event::ProjectOpened { summary, .. } => {
            tracing::info!(
                root = %summary.root.display(),
                signature = %summary.definition.signature,
                read_only = summary.read_only,
                "project opened"
            );
            // Remember the folder only after the core opened it: the recent
            // list must not gain paths that turned out not to be projects.
            let root = summary.root.clone();
            (Update::Opened(summary), Some(root))
        }
        Event::TuneSaved { outcome, .. } => {
            tracing::info!(
                path = %outcome.path.display(),
                cells = outcome.cells,
                "tune saved"
            );
            (Update::Saved(outcome), None)
        }
        Event::TableRead { table, .. } => (
            Update::Table {
                table: Box::new(table),
                edited: false,
            },
            None,
        ),
        Event::TableEdited { table, .. } => (
            Update::Table {
                table: Box::new(table),
                edited: true,
            },
            None,
        ),
        Event::DialogRead { dialog, .. } => (
            Update::Dialog {
                dialog,
                edited: false,
            },
            None,
        ),
        Event::DialogEdited { dialog, .. } => (
            Update::Dialog {
                dialog,
                edited: true,
            },
            None,
        ),
        Event::ConnectionTested { summary, .. } => (Update::Tested(summary), None),
        Event::Connected { summary, .. } => (Update::Connected(summary), None),
        Event::Reconnected { summary } => (Update::Reconnected(summary), None),
        Event::ConnectionLost { reason } => (Update::LinkLost(reason), None),
        Event::Disconnected { .. } => (Update::Disconnected, None),
        Event::PagesRead { summary, .. } => (Update::PagesRead(summary), None),
        Event::EcuAdopted { .. } => (Update::EcuAdopted, None),
        Event::RealtimeState { running, .. } => (Update::Realtime(running), None),
        Event::TraceSaved { path, frames, .. } => (
            Update::Trace {
                saved: TraceSaved { path, frames },
            },
            None,
        ),
        Event::TuneWritten { summary, .. } => (Update::Written(summary), None),
        Event::TuneBurned { summary, .. } => (Update::Burned(summary), None),
        Event::TuneImported { summary, .. } => (Update::Imported(summary), None),
        Event::TuneReverted { summary, .. } => (Update::Reverted(summary), None),
        Event::TuneStatusRead { status, .. } => (Update::TuneStatus(status), None),
        Event::WriteAccessChanged {
            writes_enabled,
            connection,
            ..
        } => (
            Update::WriteAccess {
                enabled: writes_enabled,
                connection,
            },
            None,
        ),
        Event::WriteFailureAcknowledged { .. } => (Update::Acknowledged, None),
        Event::JournalRead { path, entries, .. } => {
            (Update::Journal(JournalView { path, entries }), None)
        }
        Event::SnapshotsListed { paths, .. } => (Update::Snapshots(paths), None),
        Event::LogState {
            running, summary, ..
        } => (Update::Log { running, summary }, None),
        Event::DefinitionUnloaded { .. } => (Update::Unloaded, None),
        Event::Failed { failure, .. } => {
            tracing::warn!(code = failure.code.as_str(), "the core reported a failure");
            (Update::Failed(failure), None)
        }
        Event::PortsListed { .. } => (Update::Nothing, None),
        Event::Rejected { .. } | Event::Stopped => (Update::Nothing, None),
    };

    let auto_connect = matches!(&update, Update::Loaded(_) | Update::Opened(_))
        && cx.global::<CoreService>().auto_sim
        && cx.global::<CoreService>().status.connection.is_none();
    let import_consent = if matches!(
        &update,
        Update::Failed(failure) if failure.code == FailureCode::ImportNeedsConsent
    ) {
        cx.global::<CoreService>().pending_import.clone()
    } else {
        None
    };
    let refresh_status = match &update {
        Update::Opened(_)
        | Update::PagesRead(_)
        | Update::Written(_)
        | Update::Burned(_)
        | Update::Imported(_)
        | Update::Reverted(_)
        | Update::Saved(_) => true,
        Update::Table { edited, .. } | Update::Dialog { edited, .. } => *edited,
        Update::Failed(failure) => matches!(
            failure.code,
            FailureCode::WriteFailed | FailureCode::BurnFailed | FailureCode::ImportNeedsConsent
        ),
        _ => false,
    };
    let read_pages = matches!(&update, Update::Connected(_) | Update::Reconnected(_));
    let poll = matches!(&update, Update::PagesRead(_));
    // Remember the port that worked, so the next dialog opens on it.
    let persist_port = match &update {
        Update::Connected(connection) if !connection.custom => {
            let baud = cx
                .global::<CoreService>()
                .status
                .pending_connect
                .as_ref()
                .and_then(|pending| match pending {
                    PortChoice::Serial { baud, .. } => Some(*baud),
                    PortChoice::Custom(_) => None,
                });
            Some((connection.port.clone(), baud))
        }
        _ => None,
    };

    if !matches!(&update, Update::Nothing) {
        cx.global_mut::<CoreService>().status.apply(update);
        if let Some(path) = import_consent.clone() {
            cx.global_mut::<CoreService>().status.import_consent = Some(path);
        }
        if let Some(root) = remember {
            crate::project::remember(&root, cx);
        }
        // Only the main layout shows the core status; the Settings window does not.
        cx.refresh_windows();
    }

    if let Some((port, baud)) = persist_port {
        crate::settings::preferences::AppSettings::update(cx, |settings| {
            settings.connection.last_port = Some(port);
            settings.connection.baud_override = baud;
        });
    }
    if auto_connect {
        connect_to_simulator(cx);
    }
    if read_pages {
        // The session is read-only until proven otherwise; pulling the tune in
        // is the first thing it does, and it is also what fills the grid's
        // write-state after a reconnect.
        read_all(cx);
    }
    if poll {
        // The connection settings hold the user's poll rate; the core clamps it.
        let hz = cx
            .global::<crate::settings::preferences::AppSettings>()
            .settings()
            .connection
            .poll_rate_hz
            .clamp(1, 100);
        start_realtime(1000 / hz, cx);
    }
    if refresh_status {
        refresh_tune_status(cx);
    }
    if import_consent.is_some() {
        cx.defer(|cx| {
            if let Some(window) = crate::app::window::main(cx) {
                window
                    .update(cx, |_, window, cx| {
                        crate::layout::write_dialogs::show_import_consent(window, cx);
                    })
                    .ok();
            }
        });
    }
}

enum Update {
    Loaded(DefinitionSummary),
    Tested(ConnectionSummary),
    Opened(ProjectSummary),
    Saved(SaveSummary),
    Table {
        table: Box<TableSummary>,
        edited: bool,
    },
    Dialog {
        dialog: DialogSummary,
        edited: bool,
    },
    Connected(ConnectionSummary),
    Reconnected(ConnectionSummary),
    LinkLost(String),
    Disconnected,
    PagesRead(ReadAllSummary),
    EcuAdopted,
    Realtime(bool),
    Trace {
        saved: TraceSaved,
    },
    Written(WriteSummary),
    Burned(BurnSummary),
    Imported(ImportSummary),
    Reverted(RevertSummary),
    TuneStatus(TuneStatus),
    WriteAccess {
        enabled: bool,
        connection: Option<ConnectionSummary>,
    },
    Acknowledged,
    Journal(JournalView),
    Snapshots(Vec<PathBuf>),
    Log {
        running: bool,
        summary: Option<LogSummary>,
    },
    Unloaded,
    Failed(Failure),
    Nothing,
}

impl CoreStatus {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Loaded(summary) => {
                self.definition = Some(summary);
                self.project = None;
                self.last_save = None;
                self.table = None;
                self.dialog = None;
                self.failure = None;
                self.tune_status = None;
                self.definition_revision = self.definition_revision.wrapping_add(1);
            }
            Update::Opened(project) => {
                self.project = Some(project);
                self.definition = None;
                self.last_save = None;
                self.table = None;
                self.dialog = None;
                self.failure = None;
                self.tune_status = None;
                self.journal = None;
                self.snapshots.clear();
                self.definition_revision = self.definition_revision.wrapping_add(1);
            }
            Update::Saved(outcome) => {
                self.last_save = Some(outcome);
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Table { table, edited } => {
                self.table = Some(*table);
                // A successful table operation also proves the earlier failure
                // is stale.
                self.failure = None;
                if edited {
                    self.tune_revision = self.tune_revision.wrapping_add(1);
                }
            }
            Update::Dialog { dialog, edited } => {
                self.dialog = Some(dialog);
                self.failure = None;
                if edited {
                    self.tune_revision = self.tune_revision.wrapping_add(1);
                }
            }
            Update::Tested(summary) => {
                let port = summary.port.clone();
                self.test = Some(TestResult {
                    port,
                    outcome: Ok(summary),
                });
                self.testing = None;
                self.failure = None;
            }
            Update::Connected(connection) => {
                self.connection = Some(connection);
                self.link_lost = None;
                self.pages_read = None;
                self.realtime = false;
                self.failure = None;
                self.pending_connect = None;
                self.testing = None;
                self.test = None;
            }
            Update::Reconnected(connection) => {
                self.connection = Some(connection);
                self.link_lost = None;
                self.pages_read = None;
                self.realtime = false;
                self.failure = None;
            }
            Update::LinkLost(reason) => {
                self.link_lost = Some(reason);
                self.realtime = false;
                self.pending_connect = None;
            }
            Update::Disconnected => {
                self.connection = None;
                self.link_lost = None;
                self.pending_connect = None;
                // The last read still describes what the tune image holds.
                self.realtime = false;
            }
            Update::PagesRead(summary) => {
                self.pages_read = Some(summary);
                self.failure = None;
                // The ECU image moved: a table kept on screen can no longer
                // trust the write-state marks it drew from the old one.
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::EcuAdopted => {
                self.failure = None;
                // The working tune changed under every open table and dialog.
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Realtime(running) => self.realtime = running,
            Update::Trace { saved } => {
                self.last_trace = Some(saved);
                self.failure = None;
            }
            Update::Written(summary) => {
                self.last_write = Some(summary);
                self.write_halted = None;
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Burned(summary) => {
                self.last_burn = Some(summary);
                self.write_halted = None;
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Imported(summary) => {
                self.last_import = Some(summary);
                self.import_consent = None;
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Reverted(summary) => {
                self.last_revert = Some(summary);
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::TuneStatus(status) => self.tune_status = Some(status),
            Update::WriteAccess {
                enabled,
                connection,
            } => {
                self.writes_enabled = enabled;
                if let Some(connection) = connection {
                    self.connection = Some(connection);
                }
                self.failure = None;
            }
            Update::Acknowledged => self.write_halted = None,
            Update::Journal(view) => self.journal = Some(view),
            Update::Snapshots(paths) => self.snapshots = paths,
            Update::Log { running, summary } => {
                if let Some(summary) = summary {
                    self.last_log = Some(LogSnapshot::new(
                        running,
                        Some(summary.path),
                        summary.frames,
                        summary.dropped,
                        summary.bytes,
                    ));
                }
                if running {
                    self.last_log = Some(LogSnapshot::new(
                        true,
                        self.last_log.as_ref().and_then(|log| log.path.clone()),
                        0,
                        0,
                        0,
                    ));
                }
            }
            Update::Unloaded => {
                let writes_enabled = self.writes_enabled;
                *self = Self {
                    writes_enabled,
                    ..Self::default()
                };
            }
            Update::Failed(failure) => {
                // A table or dialog that could not be read must not stay on
                // screen as if it were still the selection; an edit failure
                // keeps it, since the user needs to see what was changing.
                if failure.code == FailureCode::TableUnavailable {
                    self.table = None;
                }
                if failure.code == FailureCode::DialogUnavailable {
                    self.dialog = None;
                }
                // A failed scan or test must not leave the dialog stuck on
                // "working". The message stays for the dialog to show.
                if failure.code == FailureCode::ConnectFailed {
                    self.scanning = false;
                }
                if failure.code == FailureCode::TestFailed {
                    let port = self.testing.take().unwrap_or_default();
                    self.test = Some(TestResult {
                        port,
                        outcome: Err(failure.message.clone()),
                    });
                }
                if failure.code == FailureCode::ConnectFailed {
                    self.pending_connect = None;
                }
                if matches!(
                    failure.code,
                    FailureCode::WriteFailed | FailureCode::BurnFailed
                ) {
                    self.write_halted = Some(failure.message.clone());
                    // The core reloaded the ECU image from the read-back and
                    // marked the changed cells; a table on screen must refetch
                    // its states instead of showing the pre-write snapshot.
                    self.tune_revision = self.tune_revision.wrapping_add(1);
                }
                self.failure = Some(failure);
            }
            Update::Nothing => {}
        }
    }
}

/// Asks the core to open a project folder.
///
/// The read and parse happen on the core thread; the reply is
/// [`Event::ProjectOpened`] or [`Event::Failed`]. A refused command (a full
/// queue, or a stopped core) produces no event, so the failure is shown here.
pub(crate) fn open_project(path: &Path, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ProjectRejected) else {
        return;
    };
    if let Err(error) = handle.open_project(path) {
        tracing::warn!(%error, path = %path.display(), "the core refused the project");
        report_failure(cx, FailureCode::ProjectRejected, error.to_string());
    }
}

/// Asks the core to write the open project's tune file.
pub(crate) fn save_tune(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TuneSaveFailed) else {
        return;
    };
    if let Err(error) = handle.save_tune() {
        tracing::warn!(%error, "the core refused the save");
        report_failure(cx, FailureCode::TuneSaveFailed, error.to_string());
    }
}

/// Starts a connection to the simulator with the default configuration.
pub(crate) fn connect_to_simulator(cx: &mut App) {
    connect(crate::link::simulator(), None, cx);
}

/// Asks for the port list, then opens the connect dialog when it arrives.
pub(crate) fn request_connect(cx: &mut App) {
    // The dialog opens at once and stays open; its port list fills in when the
    // scan answers, so a refresh never costs the user their selection.
    cx.defer(|cx| {
        if let Some(window) = crate::app::window::main(cx) {
            window
                .update(cx, |_, window, cx| {
                    crate::layout::connect_dialog::show(window, cx);
                })
                .ok();
        }
    });
    refresh_ports(cx);
}

/// Rescans the serial ports without touching the dialog.
pub(crate) fn refresh_ports(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ConnectFailed) else {
        return;
    };
    cx.global_mut::<CoreService>().status.scanning = true;
    if let Err(error) = handle.list_ports() {
        tracing::warn!(%error, "the core refused the port scan");
        cx.global_mut::<CoreService>().status.scanning = false;
        report_failure(cx, FailureCode::ConnectFailed, error.to_string());
    }
}

/// Reads a port's signature without opening a session.
pub(crate) fn test_connection(port: PortChoice, framing: Option<LinkMode>, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TestFailed) else {
        return;
    };
    let label = port.label();
    cx.global_mut::<CoreService>().status.testing = Some(label);
    if let Err(error) = handle.test_connection(port, framing) {
        tracing::warn!(%error, "the core refused the connection test");
        cx.global_mut::<CoreService>().status.testing = None;
        report_failure(cx, FailureCode::TestFailed, error.to_string());
    }
}

/// Opens a connection.
pub(crate) fn connect(port: PortChoice, framing: Option<LinkMode>, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ConnectFailed) else {
        return;
    };
    cx.global_mut::<CoreService>().status.pending_connect = Some(port.clone());
    if let Err(error) = handle.connect(port, framing) {
        tracing::warn!(%error, "the core refused the connection");
        cx.global_mut::<CoreService>().status.pending_connect = None;
        report_failure(cx, FailureCode::ConnectFailed, error.to_string());
    }
}

/// Starts periodic realtime polling at the configured rate.
pub(crate) fn start_realtime(interval_ms: u32, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::RealtimeRejected) else {
        return;
    };
    if let Err(error) = handle.start_realtime(interval_ms) {
        tracing::warn!(%error, "the core refused the realtime start");
        report_failure(cx, FailureCode::RealtimeRejected, error.to_string());
    }
}

/// Closes the connection.
pub(crate) fn disconnect(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::NotConnected) else {
        return;
    };
    if let Err(error) = handle.disconnect() {
        tracing::warn!(%error, "the core refused the disconnect");
        report_failure(cx, FailureCode::NotConnected, error.to_string());
    }
}

/// Reads every page into the tune images.
pub(crate) fn read_all(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ReadFailed) else {
        return;
    };
    if let Err(error) = handle.read_all() {
        tracing::warn!(%error, "the core refused the page read");
        report_failure(cx, FailureCode::ReadFailed, error.to_string());
    }
}

/// Makes the tune read from the ECU the working image.
///
/// After a read the project file's values stay on screen, so a write would
/// send the file rather than the controller's tune. This is the explicit way to
/// edit what the ECU actually holds.
pub(crate) fn adopt_ecu_tune(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::AdoptRefused) else {
        return;
    };
    if let Err(error) = handle.adopt_ecu_image() {
        tracing::warn!(%error, "the core refused to take the ECU image");
        report_failure(cx, FailureCode::AdoptRefused, error.to_string());
    }
}

/// Saves the session trace to the app's trace directory.
pub(crate) fn save_trace(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TraceFailed) else {
        return;
    };
    let path = crate::platform::paths::AppPaths::discover().next_trace_path();
    if let Err(error) = handle.save_trace(path) {
        tracing::warn!(%error, "the core refused the trace save");
        report_failure(cx, FailureCode::TraceFailed, error.to_string());
    }
}

/// Asks the core to sync the tune values that differ from ECU RAM.
pub(crate) fn write_to_ram(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::WriteRefused) else {
        return;
    };
    if let Err(error) = handle.write_to_ram(Vec::new()) {
        tracing::warn!(%error, "the core refused the write");
        report_failure(cx, FailureCode::WriteRefused, error.to_string());
    }
}

/// Asks the core to burn RAM to flash.
pub(crate) fn burn_to_flash(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::BurnRefused) else {
        return;
    };
    if let Err(error) = handle.burn_to_flash(Vec::new()) {
        tracing::warn!(%error, "the core refused the burn");
        report_failure(cx, FailureCode::BurnRefused, error.to_string());
    }
}

/// Discards local edits after the core snapshots the working image.
pub(crate) fn revert_tune(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::WriteRefused) else {
        return;
    };
    if let Err(error) = handle.revert_tune() {
        tracing::warn!(%error, "the core refused the revert");
        report_failure(cx, FailureCode::WriteRefused, error.to_string());
    }
}

/// Turns the global write switch on or off and persists the choice.
pub(crate) fn set_writes_enabled(enabled: bool, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::WriteRefused) else {
        return;
    };
    if let Err(error) = handle.set_writes_enabled(enabled) {
        tracing::warn!(%error, "the core refused the write switch");
        report_failure(cx, FailureCode::WriteRefused, error.to_string());
        return;
    }
    crate::settings::preferences::AppSettings::set_writes_enabled(cx, enabled);
}

/// Clears a halted write path after the user reviewed the failure.
pub(crate) fn acknowledge_write_failure(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::WriteFailed) else {
        return;
    };
    if let Err(error) = handle.acknowledge_write_failure() {
        tracing::warn!(%error, "the core refused the acknowledgement");
        report_failure(cx, FailureCode::WriteFailed, error.to_string());
    }
}

/// Asks the core to import a `.msq` into the working image.
pub(crate) fn import_tune(path: &Path, consented: bool, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ImportRejected) else {
        return;
    };
    cx.global_mut::<CoreService>().pending_import = Some(path.to_owned());
    if let Err(error) = handle.import_tune(path, consented) {
        tracing::warn!(%error, path = %path.display(), "the core refused the import");
        cx.global_mut::<CoreService>().pending_import = None;
        report_failure(cx, FailureCode::ImportRejected, error.to_string());
    }
}

/// Restores one of the project's snapshots.
pub(crate) fn restore_snapshot(path: &Path, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ImportRejected) else {
        return;
    };
    if let Err(error) = handle.restore_snapshot(path) {
        tracing::warn!(%error, path = %path.display(), "the core refused the snapshot restore");
        report_failure(cx, FailureCode::ImportRejected, error.to_string());
    }
}

/// Lists the open project's snapshots.
pub(crate) fn list_snapshots(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::SnapshotFailed) else {
        return;
    };
    if let Err(error) = handle.list_snapshots() {
        tracing::warn!(%error, "the core refused the snapshot list");
        report_failure(cx, FailureCode::SnapshotFailed, error.to_string());
    }
}

/// Reads the open project's write journal.
pub(crate) fn read_journal(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::JournalFailed) else {
        return;
    };
    if let Err(error) = handle.read_journal() {
        tracing::warn!(%error, "the core refused the journal read");
        report_failure(cx, FailureCode::JournalFailed, error.to_string());
    }
}

/// Refreshes the derived tune status (unsaved, not sent, not burned).
pub(crate) fn refresh_tune_status(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::NothingToSave) else {
        return;
    };
    if let Err(error) = handle.refresh_tune_status() {
        tracing::debug!(%error, "the core refused the status refresh");
    }
}

/// Starts a session log beside the project, or in the app's log folder.
pub(crate) fn start_log(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::LogFailed) else {
        return;
    };
    let directory = CoreService::status(cx)
        .project
        .as_ref()
        .map(|project| project.root.join(".ox-tuner").join("logs"))
        .unwrap_or_else(|| crate::platform::paths::AppPaths::discover().logs_dir);
    if let Err(error) = handle.start_log(directory) {
        tracing::warn!(%error, "the core refused the session log");
        report_failure(cx, FailureCode::LogFailed, error.to_string());
    }
}

/// Stops the session log, flushing it.
pub(crate) fn stop_log(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::LogFailed) else {
        return;
    };
    if let Err(error) = handle.stop_log() {
        tracing::warn!(%error, "the core refused to stop the session log");
        report_failure(cx, FailureCode::LogFailed, error.to_string());
    }
}

/// Asks the OS for a `.msq` and imports it.
pub(crate) fn choose_import(cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("app.import.title").to_string().into()),
    });

    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| import_tune(&path, false, cx));
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the import picker failed"),
        Err(error) => tracing::warn!(%error, "the import picker channel closed"),
    })
    .detach();
}

/// Asks the core for one table snapshot.
pub(crate) fn read_table(name: &str, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TableUnavailable) else {
        return;
    };
    if let Err(error) = handle.read_table(name) {
        tracing::warn!(%error, table = name, "the core refused the table read");
        report_failure(cx, FailureCode::TableUnavailable, error.to_string());
    }
}

/// Asks the core to apply a dialog scalar edit.
pub(crate) fn edit_scalar(dialog: &str, name: &str, value: f64, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::DialogEditRejected) else {
        return;
    };
    if let Err(error) = handle.edit_scalar(dialog, name, value) {
        tracing::warn!(%error, dialog, variable = name, "the core refused the scalar edit");
        report_failure(cx, FailureCode::DialogEditRejected, error.to_string());
    }
}

/// Asks the core for one dialog snapshot.
pub(crate) fn read_dialog(name: &str, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::DialogUnavailable) else {
        return;
    };
    if let Err(error) = handle.read_dialog(name) {
        tracing::warn!(%error, dialog = name, "the core refused the dialog read");
        report_failure(cx, FailureCode::DialogUnavailable, error.to_string());
    }
}

/// Asks the core to apply table cell edits.
pub(crate) fn edit_table(name: &str, edits: Vec<CellEdit>, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TableEditRejected) else {
        return;
    };
    if let Err(error) = handle.edit_table(name, edits) {
        tracing::warn!(%error, table = name, "the core refused the table edit");
        report_failure(cx, FailureCode::TableEditRejected, error.to_string());
    }
}

/// Asks the core to apply table axis bin edits.
pub(crate) fn edit_axis(table: &str, axis: AxisKind, edits: Vec<AxisEdit>, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TableEditRejected) else {
        return;
    };
    if let Err(error) = handle.edit_axis(table, axis, edits) {
        tracing::warn!(%error, table, axis = axis.as_str(), "the core refused the axis edit");
        report_failure(cx, FailureCode::TableEditRejected, error.to_string());
    }
}

/// Closes the open project (and any standalone definition).
pub(crate) fn close_project(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TableUnavailable) else {
        return;
    };
    if let Err(error) = handle.unload_definition() {
        tracing::warn!(%error, "the core refused to close the project");
        report_failure(cx, FailureCode::TableUnavailable, error.to_string());
    }
}

/// The core handle, or a recorded failure when the core never started.
fn handle_or_report(cx: &mut App, code: FailureCode) -> Option<CoreHandle> {
    match CoreService::handle(cx) {
        Some(handle) => Some(handle),
        None => {
            tracing::error!("the core is not running");
            report_failure(cx, code, "the core is not running".to_owned());
            None
        }
    }
}

/// Shows a failure that never reached the core.
///
/// The global is installed by [`start`] before the first window exists, so this
/// normally has somewhere to put the message; the guard is for the boot order,
/// where a failure can only be logged.
pub(crate) fn report_failure(cx: &mut App, code: FailureCode, message: String) {
    if !cx.has_global::<CoreService>() {
        tracing::error!(code = code.as_str(), %message, "failure before the core was installed");
        return;
    }

    cx.global_mut::<CoreService>().status.failure = Some(Failure { code, message });
    cx.refresh_windows();
}

/// Asks the OS for an ECU definition file, reads it, and hands it to the core.
pub(crate) fn choose_definition(cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("app.menu.open_definition").to_string().into()),
    });

    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| load(path, cx));
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the definition picker failed"),
        Err(error) => tracing::warn!(%error, "the definition picker channel closed"),
    })
    .detach();
}

/// Reads `path` and loads it in the core.
///
/// The read happens here, on the UI thread, because it is user-initiated and one
/// shot; the parsing it feeds is already off this thread.
fn load(path: std::path::PathBuf, cx: &mut App) {
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_owned);

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read the definition");
            report_failure(cx, FailureCode::DefinitionRejected, error.to_string());
            return;
        }
    };

    let Some(handle) = handle_or_report(cx, FailureCode::DefinitionRejected) else {
        return;
    };
    if let Err(error) = handle.load_definition(label, bytes) {
        tracing::warn!(%error, "the core refused the definition");
        report_failure(cx, FailureCode::DefinitionRejected, error.to_string());
    }
}

/// Tells the core to stop and returns the future that resolves once it is gone.
///
/// Every blocking step runs on the background pool: the foreground executor has
/// to keep running the event pump until [`Event::Stopped`], or the core's last
/// sends block on a full events queue and the join would never return. The
/// returned future is awaited by the quit hook (`on_app_quit`), so quitting is
/// deferred until the core's thread is joined — the core is gone before the
/// app's own state starts being torn down.
pub(crate) fn request_shutdown(cx: &mut App) -> impl Future<Output = ()> + use<> {
    let core = {
        let service = cx.global_mut::<CoreService>();
        (service.handle.clone(), service.thread.take())
    };

    let stopped = cx.background_executor().spawn(async move {
        let (handle, thread) = core;
        // Bounded: a shutdown that cannot be queued is reported, never waited
        // on forever.
        if let Some(handle) = handle
            && let Err(error) = handle.shutdown()
        {
            tracing::debug!(%error, "the core was already stopping");
        }
        if let Some(thread) = thread {
            let report = thread.join();
            tracing::debug!(
                handled = report.handled,
                rejected = report.rejected,
                "the core stopped"
            );
        }
    });

    async move {
        stopped.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> DefinitionSummary {
        DefinitionSummary {
            label: "speeduino.ini".to_owned(),
            signature: "speeduino 202501".to_owned(),
            spec_version: Some(3.78),
            pages: 15,
            constants: 817,
            pc_variables: 50,
            channels: 229,
            tables: 21,
            curves: 34,
            gauges: 90,
            dialogs: 239,
            indicators: Vec::new(),
            menu: Vec::new(),
            coverage_percent: 97.95,
            errors: 0,
            warnings: 0,
            active_flags: Vec::new(),
        }
    }

    fn project() -> ProjectSummary {
        ProjectSummary {
            root: std::path::PathBuf::from("/projects/roadster"),
            tune_path: std::path::PathBuf::from("/projects/roadster/CurrentTune.msq"),
            definition_path: std::path::PathBuf::from(
                "/projects/roadster/projectCfg/mainController.ini",
            ),
            in_project: true,
            definition: summary(),
            read_only: false,
            read_only_reasons: Vec::new(),
            loaded: 817,
            skipped: 0,
            warnings: Vec::new(),
        }
    }

    fn failure(code: FailureCode) -> Failure {
        Failure {
            code,
            message: "boom".to_owned(),
        }
    }

    fn dialog() -> DialogSummary {
        DialogSummary {
            id: "engine".to_owned(),
            title: "Engine".to_owned(),
            fields: Vec::new(),
            changed: 0,
            writable: true,
            unavailable: Vec::new(),
        }
    }

    #[test]
    fn only_an_edit_or_a_save_moves_the_tune_revision() {
        let mut status = CoreStatus::default();

        status.apply(Update::Dialog {
            dialog: dialog(),
            edited: false,
        });
        assert_eq!(status.tune_revision, 0, "a read is not a change");

        status.apply(Update::Dialog {
            dialog: dialog(),
            edited: true,
        });
        assert_eq!(status.tune_revision, 1);

        status.apply(Update::Saved(SaveSummary {
            path: "/projects/roadster/CurrentTune.msq".into(),
            cells: 3,
        }));
        assert_eq!(status.tune_revision, 2);
    }

    #[test]
    fn a_load_replaces_any_previous_failure() {
        let mut status = CoreStatus {
            failure: Some(failure(FailureCode::DefinitionRejected)),
            ..CoreStatus::default()
        };

        status.apply(Update::Loaded(summary()));

        assert!(status.definition.is_some());
        assert_eq!(status.failure, None);
    }

    #[test]
    fn a_project_and_a_definition_are_mutually_exclusive() {
        let mut status = CoreStatus::default();
        status.apply(Update::Loaded(summary()));

        status.apply(Update::Opened(project()));

        assert!(status.project.is_some());
        assert!(
            status.definition.is_none(),
            "the project owns the definition now"
        );
        assert_eq!(status.definition().map(|summary| summary.pages), Some(15));
    }

    #[test]
    fn a_save_is_shown_until_the_next_failure() {
        let mut status = CoreStatus::default();
        status.apply(Update::Opened(project()));

        status.apply(Update::Saved(SaveSummary {
            path: std::path::PathBuf::from("/projects/roadster/CurrentTune.msq"),
            cells: 3,
        }));

        assert_eq!(status.last_save.as_ref().map(|save| save.cells), Some(3));
        assert_eq!(status.failure, None);

        status.apply(Update::Failed(failure(FailureCode::TuneSaveFailed)));

        assert_eq!(
            status.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::TuneSaveFailed)
        );
    }

    #[test]
    fn a_failure_keeps_the_project_that_is_still_open() {
        let mut status = CoreStatus::default();
        status.apply(Update::Opened(project()));

        status.apply(Update::Failed(failure(FailureCode::ProjectRejected)));

        assert!(status.project.is_some(), "the old project is still owned");
        assert_eq!(
            status.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::ProjectRejected)
        );
    }

    fn connection() -> ConnectionSummary {
        ConnectionSummary {
            port: "/dev/cu.usbmodem21201".to_owned(),
            custom: false,
            signature: "speeduino 202501".to_owned(),
            definition_signature: "speeduino 202501".to_owned(),
            verdict: oxtuner_core::SignatureVerdict::Match,
            read_only: false,
            read_only_reason: None,
            writes_enabled: true,
            behavior: "speeduino".to_owned(),
            framing: "envelope",
            pages: 15,
            realtime_block: Some(130),
        }
    }

    #[test]
    fn a_connection_replaces_the_previous_one_and_clears_a_loss() {
        let mut status = CoreStatus {
            link_lost: Some("old".to_owned()),
            ..CoreStatus::default()
        };

        status.apply(Update::Connected(connection()));

        assert!(status.connection.is_some());
        assert_eq!(status.link_lost, None);
        assert_eq!(status.failure, None);
    }

    #[test]
    fn a_lost_link_keeps_the_summary_but_stops_polling() {
        let mut status = CoreStatus::default();
        status.apply(Update::Connected(connection()));
        status.apply(Update::Realtime(true));

        status.apply(Update::LinkLost("unplugged".to_owned()));

        assert!(status.connection.is_some(), "the session is still known");
        assert_eq!(status.link_lost.as_deref(), Some("unplugged"));
        assert!(!status.realtime);
    }

    #[test]
    fn a_page_read_moves_the_tune_revision_but_a_disconnect_keeps_the_read() {
        let mut status = CoreStatus::default();
        status.apply(Update::Connected(connection()));

        status.apply(Update::PagesRead(ReadAllSummary {
            pages: 15,
            bytes: 3408,
            constants: 817,
            skipped: 0,
            signature_verified: true,
        }));
        assert_eq!(status.tune_revision, 1);
        assert_eq!(status.pages_read.as_ref().map(|read| read.pages), Some(15));

        status.apply(Update::Disconnected);
        assert!(status.connection.is_none());
        assert!(status.pages_read.is_some(), "the read still happened");
    }

    #[test]
    fn a_failed_write_halts_the_path_until_it_is_acknowledged() {
        let mut status = CoreStatus::default();

        status.apply(Update::Failed(failure(FailureCode::WriteFailed)));
        assert!(status.write_halted.is_some());

        status.apply(Update::Acknowledged);
        assert!(status.write_halted.is_none());
    }

    #[test]
    fn the_write_switch_is_tracked_independently_of_the_connection() {
        let mut status = CoreStatus::default();
        assert!(!status.writes_enabled);

        status.apply(Update::WriteAccess {
            enabled: true,
            connection: None,
        });

        assert!(status.writes_enabled);
    }

    #[test]
    fn unloading_keeps_the_write_switch() {
        let mut status = CoreStatus::default();
        status.apply(Update::WriteAccess {
            enabled: true,
            connection: None,
        });

        status.apply(Update::Unloaded);

        assert!(
            status.writes_enabled,
            "the global switch is not project state"
        );
    }

    #[test]
    fn a_bad_import_is_not_halted() {
        let mut status = CoreStatus::default();

        status.apply(Update::Failed(failure(FailureCode::ImportRejected)));

        assert!(status.write_halted.is_none());
    }

    #[test]
    fn unloading_clears_every_field() {
        let mut status = CoreStatus::default();
        status.apply(Update::Opened(project()));
        status.apply(Update::Failed(failure(FailureCode::ProjectRejected)));

        status.apply(Update::Unloaded);

        assert!(status.project.is_none());
        assert!(status.definition.is_none());
        assert!(status.last_save.is_none());
        assert!(status.failure.is_none());
    }
}
