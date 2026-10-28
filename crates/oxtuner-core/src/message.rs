//! The core's message vocabulary.
//!
//! These types are the whole UI↔core contract (`docs/architecture/ui-core/contract.md`).
//! Two rules shape them:
//!
//! - **Commands carry what the core needs, never a borrowed view of UI state.**
//!   A definition load moves bytes and a settings snapshot in; nothing is shared.
//! - **Every accepted command is answered exactly once while the UI is reading.**
//!   [`OpId`] is assigned by
//!   the sender, so the UI can correlate a reply without trusting arrival order;
//!   the core answers with [`Event::DefinitionLoaded`], [`Event::ProjectOpened`],
//!   [`Event::TuneSaved`], [`Event::TableRead`], [`Event::TableEdited`],
//!   [`Event::DefinitionUnloaded`], [`Event::Failed`] or [`Event::Rejected`] —
//!   never twice. After shutdown is accepted an event that does not fit may be
//!   dropped and counted instead of blocking the quit (ADR-0023).

use std::{collections::HashSet, fmt, path::PathBuf};

use oxtuner_datalog::LogSummary;
use oxtuner_ini::{Definition, DialogRow, RowKind, SettingsContext, Severity};
use oxtuner_project::Project;
use oxtuner_protocol::LinkMode;
use oxtuner_tune::{CellState, TuneStatus};
use serde::{Deserialize, Serialize};

use crate::menu::MenuNode;

/// Identifies one accepted command for its whole lifetime.
///
/// Ids come from the sending handle and are unique per process. A gap means a
/// command was **refused** by the queue (see [`crate::CoreError::QueueFull`])
/// and never entered the core — only accepted ids receive an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId(u64);

impl OpId {
    /// The first id a fresh core hands out.
    pub(crate) const FIRST: u64 = 1;

    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw counter value, for logs.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "op-{}", self.0)
    }
}

/// One table cell the UI asked to write, in user units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellEdit {
    /// Row, from the top of the definition's array.
    pub row: usize,
    /// Column, from the left of the definition's array.
    pub column: usize,
    /// The value the user typed, after the definition's conversion.
    pub value: f64,
}

/// Which axis of a table a bin edit targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisKind {
    /// The column bins (`xBins`).
    X,
    /// The row bins (`yBins`).
    Y,
}

impl AxisKind {
    /// A stable code for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
        }
    }
}

/// One axis bin the UI asked to write, in user units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisEdit {
    /// Bin index along the axis.
    pub index: usize,
    /// The value the user typed.
    pub value: f64,
}

/// A request from the UI.
#[derive(Debug)]
pub enum Command {
    /// Parse `bytes` and take ownership of the resulting definition.
    ///
    /// `settings` is the project's settings snapshot (`<settings>` in the
    /// `.msq`); `#if` branches and the protocol commands they rewrite are
    /// resolved with it (ADR-0006). A definition that replaces an already
    /// loaded one is allowed — the core drops the old one.
    LoadDefinition {
        /// What to call the source in logs and the UI (usually a file name).
        label: String,
        /// The definition file, as read from disk.
        bytes: Vec<u8>,
        /// Settings the `#if` branches resolve against.
        settings: SettingsContext,
    },
    /// Open a TunerStudio project folder in place (ADR-0007).
    ///
    /// The core reads the definition and the tune, owns both, and answers with
    /// [`Event::ProjectOpened`] — or [`Event::Failed`] when the folder is not a
    /// project or a file cannot be read. A failure leaves the previous session
    /// untouched. A successful open **replaces** it: the previous definition and
    /// tune are dropped in the core.
    ///
    /// The read happens on the core thread, never on the UI thread.
    OpenProject {
        /// The project folder (a directory containing `projectCfg/`).
        path: PathBuf,
    },
    /// Write the open project's changed values back to its tune file.
    ///
    /// Only variables that differ from the baseline are touched, so an unedited
    /// tune stays byte-for-byte identical. A read-only session refuses with
    /// [`FailureCode::TuneSaveFailed`] and the S1 reasons.
    SaveTune,
    /// Read one table from the open project, shaped for the grid.
    ///
    /// `name` is the table id, its 3-D map id, its cell variable, or a curve
    /// name (which reads as a one-row table). The reply is
    /// [`Event::TableRead`] or [`Event::Failed`] with
    /// [`FailureCode::TableUnavailable`].
    ReadTable {
        /// Table id as written in the definition, its map id, a curve name, or
        /// the cell variable the grid edits.
        name: String,
    },
    /// Read one dialog from the open project, shaped for the constant list.
    ///
    /// The reply is [`Event::DialogRead`] or [`Event::Failed`] with
    /// [`FailureCode::DialogUnavailable`].
    ReadDialog {
        /// Dialog name as written in the definition.
        name: String,
    },
    /// Apply one scalar edit to a dialog's variable, in user units.
    ///
    /// The edit is validated before anything is stored. The reply is
    /// [`Event::DialogEdited`] with the dialog after the edit, or
    /// [`Event::Failed`] with [`FailureCode::DialogEditRejected`].
    EditScalar {
        /// Dialog the edit was made from, for the echo.
        dialog: String,
        /// Variable to write.
        name: String,
        /// New value, in user units.
        value: f64,
    },
    /// Apply user-unit edits to one table's cells.
    ///
    /// Every edit is validated before any value is stored, so a rejected
    /// request leaves the tune untouched. The reply is [`Event::TableEdited`]
    /// with the table after the edit, or [`Event::Failed`] with
    /// [`FailureCode::TableEditRejected`].
    EditTable {
        /// The cell variable's name (the table's `zBins`).
        name: String,
        /// Cells to write, in user units.
        edits: Vec<CellEdit>,
    },
    /// Apply user-unit edits to one table's axis bins.
    ///
    /// `table` is the table id (or its 3-D map id, or a curve name — anything
    /// `TableSummary::from_project` resolves). Every edit is validated before
    /// any bin is stored. The reply is [`Event::TableEdited`] with the table
    /// after the edit, or [`Event::Failed`] with
    /// [`FailureCode::TableEditRejected`].
    EditAxis {
        /// Table id as written in the definition, its map id, or a curve name.
        table: String,
        /// Which axis the bins belong to.
        axis: AxisKind,
        /// Bins to write, in user units.
        edits: Vec<AxisEdit>,
    },
    /// Connect over a serial device or a link the application supplies, using
    /// the loaded definition.
    ///
    /// The core builds the transport and protocol, owns them on the I/O thread
    /// and answers with [`Event::Connected`] (or [`Event::Failed`] with
    /// [`FailureCode::ConnectFailed`]). The definition must be loaded; without
    /// one the core has no message set to talk.
    Connect {
        /// Where the bytes go.
        port: PortChoice,
        /// Framing override. `None` uses the definition's own
        /// `messageEnvelopeFormat`; `Some(Legacy)` is the fallback for a
        /// firmware whose framed path misbehaves.
        framing: Option<LinkMode>,
    },
    /// Read the signature from a port without opening a session.
    ///
    /// Only the definition's `queryCommand` is sent — no page read, no write.
    /// The reply is [`Event::ConnectionTested`] with the same
    /// [`ConnectionSummary`] a real connection would report, or a failure with
    /// [`FailureCode::TestFailed`]. A live connection must be closed first.
    TestConnection {
        /// Where the bytes go.
        port: PortChoice,
        /// Framing override, as for [`Self::Connect`].
        framing: Option<LinkMode>,
    },
    /// Close the connection and release the port.
    Disconnect,
    /// Read every declared page into the tune images.
    ///
    /// Only a complete read updates the ECU image; a failure leaves it
    /// untouched and reports which page failed.
    ReadAll,
    /// Take the ECU image the last read captured as the working tune.
    ///
    /// Without this a project's file values stay on screen after a read, so
    /// writing would replace the ECU's tune with the file's. The file baseline
    /// does not move, so the adopted values show as unsaved changes.
    AdoptEcuImage,
    /// Start periodic realtime polling at `interval_ms`.
    StartRealtime {
        /// Milliseconds between blocks. Clamped to 10–1000 by the core.
        interval_ms: u32,
    },
    /// Stop periodic realtime polling. The last frame stays readable.
    StopRealtime,
    /// Write the session trace to `path`.
    SaveTrace {
        /// Where to write the trace file.
        path: PathBuf,
    },
    /// Send the changed working values to ECU RAM.
    ///
    /// This is the only command that starts a RAM write. The core clamps the
    /// values, reads the pages it will patch, writes only the variables that
    /// changed, reads them back and verifies the result. It never burns.
    /// `pages` is an optional restriction; an empty list means every page with
    /// a change. S1, S2, S3, S6, S7 and S8 apply to this path.
    WriteToRam {
        /// INI page numbers, or empty for every changed page.
        pages: Vec<u8>,
    },
    /// Burn RAM content to flash.
    ///
    /// Only an explicit user command reaches this variant (S5: no auto-burn).
    /// The core snapshots the working image, burns the selected pages (or every
    /// burnable page), waits for the protocol's activation delay, reads the
    /// pages back and verifies them. The working image must already match ECU
    /// RAM: burning older RAM while edits are unwritten is refused.
    BurnToFlash {
        /// INI page numbers, or empty for every burnable page.
        pages: Vec<u8>,
    },
    /// Discard local edits and return to the file baseline.
    ///
    /// A snapshot of the working image is written first (S4).
    RevertTune,
    /// Import another `.msq` into the working image.
    ///
    /// A tune whose signature differs from the loaded definition needs
    /// `consented: true` (S9); without it the core refuses with
    /// [`FailureCode::ImportNeedsConsent`] and changes nothing.
    ImportTune {
        /// The `.msq` to import.
        path: PathBuf,
        /// Whether the user approved a different-signature import.
        consented: bool,
    },
    /// Restore one of the project's own snapshots.
    ///
    /// The current working image is snapshotted first, then the snapshot is
    /// imported. Snapshots carry the definition's signature, so no consent is
    /// needed for a first-party snapshot.
    RestoreSnapshot {
        /// A `.msq` under `.ox-tuner/snapshots/`.
        path: PathBuf,
    },
    /// List the project's snapshots, oldest first.
    ListSnapshots,
    /// Turn the global write path on or off (S8).
    ///
    /// Writes start disabled. Enabling is an explicit user choice; disabling is
    /// immediate and always allowed.
    SetWritesEnabled {
        /// Whether writes may run from now on.
        enabled: bool,
    },
    /// Clear a halted write path after the user reviewed a failed write (S3).
    AcknowledgeWriteFailure,
    /// Read the write journal for the open project.
    ReadJournal,
    /// Refresh the derived tune status (unsaved, not sent, not burned).
    RefreshTuneStatus,
    /// Start a CSV session log in `directory`.
    ///
    /// The core names the file, writes the definition's channel order and
    /// appends every realtime frame. Only one log runs at a time.
    StartLog {
        /// Where the session file is created.
        directory: PathBuf,
    },
    /// Stop the session log, flushing it before the reply.
    StopLog,
    /// List the serial ports the connect dialog can offer.
    ListPorts,
    /// Drop the loaded definition and tune.
    UnloadDefinition,
    /// Stop the core. No command is accepted after this one is read.
    Shutdown,
}

impl Command {
    /// A definition load against the default settings context.
    pub fn load_definition(label: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self::LoadDefinition {
            label: label.into(),
            bytes: bytes.into(),
            settings: SettingsContext::new(),
        }
    }

    /// A stable name for logs. Never contains user data.
    pub fn name(&self) -> &'static str {
        match self {
            Self::LoadDefinition { .. } => "load-definition",
            Self::OpenProject { .. } => "open-project",
            Self::SaveTune => "save-tune",
            Self::ReadTable { .. } => "read-table",
            Self::ReadDialog { .. } => "read-dialog",
            Self::EditScalar { .. } => "edit-scalar",
            Self::EditTable { .. } => "edit-table",
            Self::EditAxis { .. } => "edit-axis",
            Self::Connect { .. } => "connect",
            Self::TestConnection { .. } => "test-connection",
            Self::Disconnect => "disconnect",
            Self::ReadAll => "read-all",
            Self::AdoptEcuImage => "adopt-ecu-image",
            Self::StartRealtime { .. } => "start-realtime",
            Self::StopRealtime => "stop-realtime",
            Self::SaveTrace { .. } => "save-trace",
            Self::WriteToRam { .. } => "write-to-ram",
            Self::BurnToFlash { .. } => "burn-to-flash",
            Self::RevertTune => "revert-tune",
            Self::ImportTune { .. } => "import-tune",
            Self::RestoreSnapshot { .. } => "restore-snapshot",
            Self::ListSnapshots => "list-snapshots",
            Self::SetWritesEnabled { .. } => "set-writes-enabled",
            Self::AcknowledgeWriteFailure => "acknowledge-write-failure",
            Self::ReadJournal => "read-journal",
            Self::RefreshTuneStatus => "refresh-tune-status",
            Self::StartLog { .. } => "start-log",
            Self::StopLog => "stop-log",
            Self::ListPorts => "list-ports",
            Self::UnloadDefinition => "unload-definition",
            Self::Shutdown => "shutdown",
        }
    }
}

/// A reply from the core.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The definition is loaded and owned by the core.
    DefinitionLoaded {
        op: OpId,
        summary: DefinitionSummary,
    },
    /// A project is open and owned by the core: definition, tune images and the
    /// load gate's findings.
    ProjectOpened { op: OpId, summary: ProjectSummary },
    /// The open project's tune file was rewritten.
    TuneSaved { op: OpId, outcome: SaveSummary },
    /// A table was read from the open project.
    TableRead { op: OpId, table: TableSummary },
    /// A table edit was accepted; `table` is the state after it.
    TableEdited { op: OpId, table: TableSummary },
    /// A dialog was read from the open project.
    DialogRead { op: OpId, dialog: DialogSummary },
    /// A scalar edit was accepted; `dialog` is the state after it.
    DialogEdited { op: OpId, dialog: DialogSummary },
    /// The serial ports the connect dialog can offer.
    PortsListed { op: OpId, ports: Vec<PortSummary> },
    /// A [`Command::TestConnection`] read a signature.
    ///
    /// The summary describes what a real connection would be: the same verdict
    /// and read-only rule, without a session or a page read.
    ConnectionTested {
        op: OpId,
        summary: ConnectionSummary,
    },
    /// The ECU answered and the session is up.
    Connected {
        op: OpId,
        summary: ConnectionSummary,
    },
    /// The connection was closed on request.
    Disconnected { op: OpId },
    /// Every declared page was read into the tune images.
    PagesRead { op: OpId, summary: ReadAllSummary },
    /// The ECU image became the working tune, so the editor shows what the
    /// controller holds.
    EcuAdopted { op: OpId },
    /// Periodic polling started or stopped.
    RealtimeState { op: OpId, running: bool },
    /// The session trace was written.
    TraceSaved {
        op: OpId,
        path: PathBuf,
        frames: usize,
    },
    /// Changed values were written to ECU RAM and verified by a read-back.
    TuneWritten { op: OpId, summary: WriteSummary },
    /// RAM was burned to flash and read back.
    TuneBurned { op: OpId, summary: BurnSummary },
    /// Local edits were discarded, after a snapshot.
    TuneReverted { op: OpId, summary: RevertSummary },
    /// Another tune was imported into the working image.
    TuneImported { op: OpId, summary: ImportSummary },
    /// The project's snapshots, oldest first.
    SnapshotsListed { op: OpId, paths: Vec<PathBuf> },
    /// The derived write state after a read, edit, write, burn or import.
    TuneStatusRead { op: OpId, status: TuneStatus },
    /// The global write switch changed (S8).
    WriteAccessChanged {
        op: OpId,
        writes_enabled: bool,
        /// The live session's summary after the change, when one is open.
        connection: Option<ConnectionSummary>,
    },
    /// A halted write path was cleared after review.
    WriteFailureAcknowledged { op: OpId },
    /// The project's write journal, newest last.
    JournalRead {
        op: OpId,
        path: Option<PathBuf>,
        entries: Vec<JournalEntry>,
    },
    /// A session log started or stopped.
    LogState {
        op: OpId,
        running: bool,
        /// What a stopped log wrote.
        summary: Option<LogSummary>,
    },
    /// The link died; the core is either reconnecting or waiting for a command.
    ConnectionLost {
        /// Why, in text the UI can show.
        reason: String,
    },
    /// The link came back and the ECU answered again.
    Reconnected { summary: ConnectionSummary },
    /// The loaded definition (and tune, if a project was open) was dropped.
    DefinitionUnloaded { op: OpId },
    /// The command was handled but could not do its work.
    Failed { op: OpId, failure: Failure },
    /// The command was accepted, then never executed because the core stopped.
    Rejected { op: OpId, reason: RejectReason },
    /// The core thread has stopped. Always the last event.
    Stopped,
}

impl Event {
    /// The operation this event answers, if it answers one.
    pub fn op(&self) -> Option<OpId> {
        match self {
            Self::DefinitionLoaded { op, .. }
            | Self::ProjectOpened { op, .. }
            | Self::TuneSaved { op, .. }
            | Self::TableRead { op, .. }
            | Self::TableEdited { op, .. }
            | Self::DialogRead { op, .. }
            | Self::DialogEdited { op, .. }
            | Self::PortsListed { op, .. }
            | Self::ConnectionTested { op, .. }
            | Self::Connected { op, .. }
            | Self::Disconnected { op }
            | Self::PagesRead { op, .. }
            | Self::EcuAdopted { op }
            | Self::RealtimeState { op, .. }
            | Self::TraceSaved { op, .. }
            | Self::TuneWritten { op, .. }
            | Self::TuneBurned { op, .. }
            | Self::TuneReverted { op, .. }
            | Self::TuneImported { op, .. }
            | Self::SnapshotsListed { op, .. }
            | Self::TuneStatusRead { op, .. }
            | Self::WriteAccessChanged { op, .. }
            | Self::WriteFailureAcknowledged { op }
            | Self::JournalRead { op, .. }
            | Self::LogState { op, .. }
            | Self::DefinitionUnloaded { op }
            | Self::Failed { op, .. }
            | Self::Rejected { op, .. } => Some(*op),
            Self::ConnectionLost { .. } | Self::Reconnected { .. } => None,
            Self::Stopped => None,
        }
    }
}

/// Why a command was accepted and then not executed.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The core was stopping; the command never touched any resource.
    ShuttingDown,
}

impl RejectReason {
    /// A stable code for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShuttingDown => "shutting-down",
        }
    }
}

/// A handled command that could not do its work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// What kind of failure it was.
    pub code: FailureCode,
    /// The full message, including per-line diagnostics where the parser has
    /// them. Designed to be shown in the output panel as-is.
    pub message: String,
}

/// The failure kinds the core can report today.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCode {
    /// The bytes are not a definition this build can read (ADR-0006 rule 11:
    /// an unsupported `iniSpecVersion` fails loudly rather than half-parsing).
    DefinitionRejected,
    /// The folder is not an openable TunerStudio project, or a file in it could
    /// not be read or parsed.
    ProjectRejected,
    /// The tune file could not be written, or the session is read-only (S1).
    TuneSaveFailed,
    /// There is no open tune to save (no project, or only a definition).
    NothingToSave,
    /// The table is not in the definition, has no numeric cell array, or no
    /// project is open to read it from.
    TableUnavailable,
    /// A table edit was out of range, not finite, or the session is read-only.
    TableEditRejected,
    /// The dialog is not in the definition, or no project is open to read it
    /// from.
    DialogUnavailable,
    /// A scalar edit was not finite, not a scalar, or the session is read-only.
    DialogEditRejected,
    /// The port could not be opened, the definition has no message set, or a
    /// connection is already up.
    ConnectFailed,
    /// The signature test could not run (no definition, port busy, no answer).
    TestFailed,
    /// There is no connection to act on.
    NotConnected,
    /// A page read failed. The ECU image is left untouched.
    ReadFailed,
    /// The ECU image could not become the working tune: no project, no read
    /// yet, or the images do not share a shape.
    AdoptRefused,
    /// The session trace could not be written.
    TraceFailed,
    /// Realtime polling was refused (no connection, or the definition has no
    /// realtime block).
    RealtimeRejected,
    /// A write was refused before any byte was sent: writes are off (S8), the
    /// signature does not match (S1), the path is halted after a failed
    /// verification (S3), the session is not connected, or nothing changed.
    WriteRefused,
    /// A page write or its read-back verification failed. The ECU image is
    /// reloaded from the read-back and the write path halts until reviewed.
    WriteFailed,
    /// A burn was refused before any byte was sent (guardrails, no burnable
    /// page, or unwritten edits).
    BurnRefused,
    /// A burn failed, or its read-back did not match.
    BurnFailed,
    /// The tune could not be imported (unreadable, unparseable, wrong shape).
    ImportRejected,
    /// The imported tune has a different signature and the user has not
    /// approved the import (S9).
    ImportNeedsConsent,
    /// The snapshot before a risky operation could not be written.
    SnapshotFailed,
    /// The write journal could not be written, so the operation was refused.
    JournalFailed,
    /// The session log could not be started or stopped.
    LogFailed,
    /// The request was refused because an operation that must finish owns the
    /// link (a write or burn, including its EEPROM wait).
    Busy,
}

impl FailureCode {
    /// A stable code for logs and for the UI to switch on.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DefinitionRejected => "definition-rejected",
            Self::ProjectRejected => "project-rejected",
            Self::TuneSaveFailed => "tune-save-failed",
            Self::NothingToSave => "nothing-to-save",
            Self::TableUnavailable => "table-unavailable",
            Self::TableEditRejected => "table-edit-rejected",
            Self::DialogUnavailable => "dialog-unavailable",
            Self::DialogEditRejected => "dialog-edit-rejected",
            Self::ConnectFailed => "connect-failed",
            Self::TestFailed => "test-failed",
            Self::NotConnected => "not-connected",
            Self::ReadFailed => "read-failed",
            Self::AdoptRefused => "adopt-refused",
            Self::TraceFailed => "trace-failed",
            Self::RealtimeRejected => "realtime-rejected",
            Self::WriteRefused => "write-refused",
            Self::WriteFailed => "write-failed",
            Self::BurnRefused => "burn-refused",
            Self::BurnFailed => "burn-failed",
            Self::ImportRejected => "import-rejected",
            Self::ImportNeedsConsent => "import-needs-consent",
            Self::SnapshotFailed => "snapshot-failed",
            Self::JournalFailed => "journal-failed",
            Self::LogFailed => "log-failed",
            Self::Busy => "busy",
        }
    }
}

/// The load result, shaped for the UI rather than for the parser.
///
/// Deliberately small and `Clone`: the core keeps the full [`Definition`];
/// the UI only needs enough to name what it is looking at.
#[derive(Debug, Clone, PartialEq)]
pub struct DefinitionSummary {
    /// The label the load command carried.
    pub label: String,
    /// Firmware signature.
    pub signature: String,
    /// `iniSpecVersion`, when the file declares one.
    pub spec_version: Option<f64>,
    /// Calibration pages.
    pub pages: usize,
    /// Calibration constants.
    pub constants: usize,
    /// PC-side variables.
    pub pc_variables: usize,
    /// Realtime channels.
    pub channels: usize,
    /// 2-D tables.
    pub tables: usize,
    /// 1-D curves.
    pub curves: usize,
    /// Gauge templates.
    pub gauges: usize,
    /// Dialogs.
    pub dialogs: usize,
    /// The navigation tree the primary sidebar draws, in definition order.
    pub menu: Vec<MenuNode>,
    /// Share of entries that became model data, 0–100.
    pub coverage_percent: f64,
    /// Parse errors recorded while loading.
    pub errors: usize,
    /// Parse warnings recorded while loading.
    pub warnings: usize,
    /// `#if` flags active for this load, sorted.
    pub active_flags: Vec<String>,
}

impl DefinitionSummary {
    /// Builds the UI-facing summary of a parsed definition.
    pub fn new(label: impl Into<String>, definition: &Definition) -> Self {
        let severity = |wanted: Severity| {
            definition
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == wanted)
                .count()
        };

        Self {
            label: label.into(),
            signature: definition.signature.clone(),
            spec_version: definition.spec_version,
            pages: definition.pages.len(),
            constants: definition.constants.len(),
            pc_variables: definition.pc_variables.len(),
            channels: definition.channels.len(),
            tables: definition.tables.len(),
            curves: definition.curves.len(),
            gauges: definition.gauges.len(),
            dialogs: definition.dialogs.len(),
            menu: crate::menu::build(definition),
            coverage_percent: definition.coverage.percent(),
            errors: severity(Severity::Error),
            warnings: severity(Severity::Warning),
            active_flags: definition.active_flags.iter().cloned().collect(),
        }
    }

    /// Whether the definition parsed without errors.
    pub fn is_clean(&self) -> bool {
        self.errors == 0
    }
}

/// An open project, shaped for the UI rather than for the file layer.
///
/// Deliberately small and `Clone`: the core keeps the [`Project`] (definition,
/// tune images, `.msq` document); the UI only needs enough to name what is open,
/// explain read-only mode and show the load gate's findings.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSummary {
    /// The project folder, or the tune's folder in standalone mode.
    pub root: PathBuf,
    /// The tune file that was read (and will be written).
    pub tune_path: PathBuf,
    /// The definition file the tune was interpreted with.
    pub definition_path: PathBuf,
    /// Whether this is a project folder (as opposed to standalone).
    pub in_project: bool,
    /// The definition, summarised like a standalone load.
    pub definition: DefinitionSummary,
    /// Whether saving is refused (S1).
    pub read_only: bool,
    /// Why the tune opened read-only, for the editor and output panel that
    /// explain it.
    pub read_only_reasons: Vec<String>,
    /// Values copied from the tune into the model.
    pub loaded: usize,
    /// Values left at their definition default (absent from the tune).
    pub skipped: usize,
    /// Non-fatal findings, ready for the output panel.
    pub warnings: Vec<String>,
}

impl ProjectSummary {
    /// Builds the UI-facing summary of an open project.
    pub(crate) fn from_project(project: &Project) -> Self {
        let report = project.report();
        let label = project
            .definition_path()
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(
                || project.definition_path().display().to_string(),
                str::to_owned,
            );

        Self {
            root: project.root().to_owned(),
            tune_path: project.tune_path().to_owned(),
            definition_path: project.definition_path().to_owned(),
            in_project: project.is_in_project(),
            definition: DefinitionSummary::new(label, project.definition()),
            read_only: report.is_read_only(),
            read_only_reasons: report
                .read_only_reasons()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            loaded: report.loaded,
            skipped: report.skipped,
            warnings: report
                .warnings
                .iter()
                .map(|issue| issue.message.clone())
                .collect(),
        }
    }
}

/// What a successful [`Command::SaveTune`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSummary {
    /// The tune file that was rewritten.
    pub path: PathBuf,
    /// Number of values that changed and were written.
    pub cells: usize,
}

/// What a verified RAM write did.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteSummary {
    /// Pages written.
    pub pages: usize,
    /// Cells whose values changed and were sent.
    pub cells: usize,
    /// Variables those cells belong to.
    pub variables: usize,
    /// Whether the read-back matched the bytes written.
    pub verified: bool,
    /// The snapshot taken before the write, when one was needed.
    pub snapshot: Option<PathBuf>,
    /// The journal file the entries went to.
    pub journal: Option<PathBuf>,
}

/// What a verified burn did.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnSummary {
    /// Pages burned.
    pub pages: usize,
    /// Whether the read-back matched RAM after the burn.
    pub verified: bool,
    /// The snapshot taken before the burn.
    pub snapshot: Option<PathBuf>,
    /// The journal file the entries went to.
    pub journal: Option<PathBuf>,
}

/// What reverting to the file baseline did.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertSummary {
    /// Cells that were restored.
    pub cells: usize,
    /// The snapshot taken before the revert.
    pub snapshot: Option<PathBuf>,
}

/// What importing another tune did.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// The file that was imported.
    pub path: PathBuf,
    /// Cells whose working values changed.
    pub cells: usize,
    /// Variables those cells belong to.
    pub variables: usize,
    /// Whether the import was approved despite a signature difference (S9).
    pub consented: bool,
    /// The snapshot taken before the import.
    pub snapshot: Option<PathBuf>,
}

/// One line in the append-only write journal.
///
/// A write or burn writes a `begin` entry before touching the ECU and a `result`
/// entry when it finishes, so a crash leaves evidence of the attempt. Entries are
/// JSON lines in `.ox-tuner/write-journal.log`, capped by rotation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// UTC time, RFC 3339.
    pub at: String,
    /// `write`, `burn`, `revert`, `import` or `restore`.
    pub operation: String,
    /// `begin` or `result`.
    pub phase: String,
    /// The link the operation ran on (`simulator`, a device path).
    pub port: String,
    /// The ECU signature the session saw.
    pub signature: String,
    /// Page numbers, comma separated, or `all`.
    pub pages: String,
    /// Cells touched.
    pub cells: usize,
    /// Variables touched.
    pub variables: usize,
    /// `ok`, `failed`, `refused` or `pending`.
    pub result: String,
    /// The verification outcome or failure text.
    pub detail: String,
}

/// What a dialog row holds, for the UI to pick a presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogFieldKind {
    /// A scalar value from `field`, `displayOnlyField`, `slider` or a setting.
    Value,
    /// A boolean from `checkbox` or `radio`; the value is 0 or 1.
    Toggle,
    /// Text with no variable: a hint or a warning the definition shows.
    Note,
}

/// One row of a dialog, shaped for the constant list.
///
/// Values are in **user units**. The UI keeps no tune values of its own: it
/// renders this snapshot and asks for edits by variable.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogFieldSummary {
    /// Label from the row, unquoted.
    pub label: String,
    /// Tune variable the row writes, empty for a note.
    pub variable: String,
    /// User value, for scalar numbers.
    pub value: Option<f64>,
    /// Text, for string variables and notes.
    pub text: Option<String>,
    /// Units of the value.
    pub units: String,
    /// Decimal digits to display.
    pub digits: u8,
    /// Whether the row differs from the file baseline.
    pub changed: bool,
    /// Whether the definition marks the row display-only.
    pub display_only: bool,
    /// Whether the session may store an edit to this row.
    pub writable: bool,
    /// What the row holds, so a toggle is not drawn as a plain number.
    pub kind: DialogFieldKind,
    /// The nested panel title the row came from, empty at the dialog's top.
    pub section: String,
    /// Whether a note is a warning (`!` or `#` before the text).
    pub warning: bool,
}

/// One dialog, shaped for the constant list.
///
/// Panels are flattened into the field list: a dialog like `Engine Constants`
/// is a border layout of panels, and its constants live one or two levels down.
/// Tables and curves a panel embeds stay out; the menu opens them as grids.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogSummary {
    /// The definition's dialog name.
    pub id: String,
    /// Window title from the definition, or the dialog name.
    pub title: String,
    /// Rows in document order, panels flattened in place.
    pub fields: Vec<DialogFieldSummary>,
    /// Number of fields that differ from the file baseline.
    pub changed: usize,
    /// Whether the session may store edits.
    pub writable: bool,
    /// Built-in `std_*` panels this dialog embeds, which this build cannot draw.
    pub unavailable: Vec<String>,
}

/// One table, shaped for the grid.
///
/// Values are in **user units**, row-major from the definition's array. The UI
/// keeps no tune values of its own: it renders this snapshot and asks for edits
/// by cell.
#[derive(Debug, Clone, PartialEq)]
pub struct TableSummary {
    /// The definition's table id (the `[TableEditor]` name).
    pub id: String,
    /// The cell variable's name (the table's `zBins`).
    pub name: String,
    /// Window title from the definition, or the table id when it has none.
    pub title: String,
    /// Columns.
    pub cols: usize,
    /// Rows.
    pub rows: usize,
    /// Decimal digits to display.
    pub digits: u8,
    /// Units of the cell values.
    pub units: String,
    /// Cell values, row-major.
    pub cells: Vec<f64>,
    /// Per-cell write state (ADR-0010). `Unknown` until an ECU was read.
    pub states: Vec<CellState>,
    /// Per-cell "differs from the file baseline".
    pub changed: Vec<bool>,
    /// Column axis.
    pub x_axis: AxisSummary,
    /// Row axis.
    pub y_axis: AxisSummary,
    /// Lowest cell value, for the heatmap.
    pub minimum: f64,
    /// Highest cell value, for the heatmap.
    pub maximum: f64,
    /// Whether the session may store edits.
    pub writable: bool,
}

/// One table axis, as the grid draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisSummary {
    /// Variable name the values come from.
    pub name: String,
    /// Label to show, from `xyLabels`, otherwise the variable name.
    pub label: String,
    /// Units.
    pub units: String,
    /// Decimal digits to display.
    pub digits: u8,
    /// Axis values, in definition order.
    pub values: Vec<f64>,
}

impl TableSummary {
    /// Reads one table from an open project.
    ///
    /// A table that the definition declares but the tune cannot represent is an
    /// error, never an empty grid: a wrong shape would put values in the wrong
    /// cells.
    ///
    /// A 1-D curve resolves here too, as a one-row table: the curve's first
    /// `yBins` series becomes the cells and its `xBins` the column axis. The
    /// grid, its edits and its copy/paste then work unchanged; the 3-D view
    /// and the chart stay pending work.
    pub(crate) fn from_project(project: &Project, name: &str) -> Result<Self, String> {
        let definition = project.definition();
        if let Some(table) = definition
            .tables
            .get_ci(name)
            .or_else(|| {
                definition.tables.values().find(|table| {
                    table
                        .map_id
                        .as_deref()
                        .is_some_and(|map| map.eq_ignore_ascii_case(name))
                })
            })
            .or_else(|| {
                definition.tables.values().find(|table| {
                    table
                        .z_bins
                        .as_deref()
                        .is_some_and(|cells| cells.eq_ignore_ascii_case(name))
                })
            })
        {
            return Self::from_table(project, table);
        }

        if let Some(curve) = definition.curves.get_ci(name) {
            return Self::from_curve(project, curve);
        }

        // A curve's value array (its first `yBins`), so a cell edit echoed by
        // variable name still finds the curve it belongs to.
        if let Some(curve) = definition.curves.values().find(|curve| {
            curve
                .y_bins
                .first()
                .is_some_and(|bins| bins.array.eq_ignore_ascii_case(name))
        }) {
            return Self::from_curve(project, curve);
        }

        // A menu's `VE Table` is a dialog whose panel holds the map; opening it
        // by the dialog name must still reach the grid inside.
        if let Some(dialog) = definition.dialogs.get_ci(name)
            && let Some(embedded) =
                crate::menu::embedded_target(&dialog.name, definition, &mut HashSet::new())
        {
            return Self::from_project(project, &embedded.id);
        }

        Err(format!("the definition has no table named `{name}`"))
    }

    /// The axis variable a table's bins live in, if the definition declares it.
    pub(crate) fn axis_variable(
        project: &Project,
        name: &str,
        axis: AxisKind,
    ) -> Result<String, String> {
        let definition = project.definition();
        if let Some(table) = definition.tables.get_ci(name).or_else(|| {
            definition.tables.values().find(|table| {
                table
                    .map_id
                    .as_deref()
                    .is_some_and(|map| map.eq_ignore_ascii_case(name))
            })
        }) {
            let bins = match axis {
                AxisKind::X => table.x_bins.as_ref(),
                AxisKind::Y => table.y_bins.as_ref(),
            };
            return bins
                .map(|bins| bins.array.clone())
                .ok_or_else(|| format!("table `{}` has no {} axis bins", table.id, axis.as_str()));
        }

        if let Some(curve) = definition.curves.get_ci(name) {
            return match axis {
                AxisKind::X => curve.x_bins.as_ref().map(|bins| bins.array.clone()),
                AxisKind::Y => curve.y_bins.first().map(|bins| bins.array.clone()),
            }
            .ok_or_else(|| format!("curve `{}` has no {} axis bins", curve.name, axis.as_str()));
        }

        Err(format!("the definition has no table named `{name}`"))
    }

    fn from_table(project: &Project, table: &oxtuner_ini::Table) -> Result<Self, String> {
        let cells_name = table
            .z_bins
            .as_deref()
            .ok_or_else(|| format!("table `{}` has no `zBins` cell array", table.id))?;
        let spec = project
            .layout()
            .variable(cells_name)
            .ok_or_else(|| format!("the tune has no variable named `{cells_name}`"))?;
        let (cols, rows) = spec.shape.dimensions().ok_or_else(|| {
            format!("`{cells_name}` is not a 2-D array, so it cannot be shown as a table")
        })?;

        let tune = project.tune();
        let baseline = project.images().baseline();
        let mut cells = Vec::with_capacity(cols.saturating_mul(rows));
        let mut states = Vec::with_capacity(cols.saturating_mul(rows));
        let mut changed = Vec::with_capacity(cols.saturating_mul(rows));
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;

        for row in 0..rows {
            for column in 0..cols {
                let value = tune
                    .cell_user(cells_name, row, column)
                    .map_err(|error| error.to_string())?;
                minimum = minimum.min(value);
                maximum = maximum.max(value);
                cells.push(value);
                states.push(
                    project
                        .images()
                        .cell_state(cells_name, row, column)
                        .unwrap_or(CellState::Unknown),
                );
                let edited = matches!(
                    (
                        tune.cell_raw(cells_name, row, column),
                        baseline.cell_raw(cells_name, row, column)
                    ),
                    (Ok(current), Ok(saved)) if current != saved
                );
                changed.push(edited);
            }
        }

        let (minimum, maximum) = if cells.is_empty() {
            (0.0, 0.0)
        } else {
            (minimum, maximum)
        };

        let x_label = table.xy_labels.first().and_then(|text| text.literal());
        let y_label = table.xy_labels.get(1).and_then(|text| text.literal());
        let x_axis = axis_summary(project, table.x_bins.as_ref(), cols, x_label, "X");
        let y_axis = axis_summary(project, table.y_bins.as_ref(), rows, y_label, "Y");

        Ok(Self {
            id: table.id.clone(),
            name: cells_name.to_owned(),
            title: table
                .title
                .literal()
                .map_or_else(|| table.id.clone(), str::to_owned),
            cols,
            rows,
            digits: spec.digits,
            units: spec.units.clone(),
            cells,
            states,
            changed,
            x_axis,
            y_axis,
            minimum,
            maximum,
            writable: !project.is_read_only() && tune.is_writable(cells_name),
        })
    }

    /// A curve as a one-row table: the first `yBins` series is the row, the
    /// `xBins` are the column axis.
    fn from_curve(project: &Project, curve: &oxtuner_ini::Curve) -> Result<Self, String> {
        let bins = curve
            .y_bins
            .first()
            .ok_or_else(|| format!("curve `{}` has no `yBins` value array", curve.name))?;
        let cells_name = bins.array.as_str();
        let spec = project
            .layout()
            .variable(cells_name)
            .ok_or_else(|| format!("the tune has no variable named `{cells_name}`"))?;
        let (cols, rows) = spec.shape.dimensions().ok_or_else(|| {
            format!("`{cells_name}` is not an array, so it cannot be shown as a curve")
        })?;
        // A list is `cols == points, rows == 1`; anything else is flattened
        // into one row so the grid still shows every value.
        let points = cols.saturating_mul(rows).max(1);

        let tune = project.tune();
        let baseline = project.images().baseline();
        let mut cells = Vec::with_capacity(points);
        let mut states = Vec::with_capacity(points);
        let mut changed = Vec::with_capacity(points);
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;

        for index in 0..points {
            let value = tune
                .user_at(cells_name, index)
                .map_err(|error| error.to_string())?;
            minimum = minimum.min(value);
            maximum = maximum.max(value);
            cells.push(value);
            states.push(CellState::Unknown);
            let edited = matches!(
                (tune.raw_at(cells_name, index), baseline.raw_at(cells_name, index)),
                (Ok(current), Ok(saved)) if current != saved
            );
            changed.push(edited);
        }

        let (minimum, maximum) = if cells.is_empty() {
            (0.0, 0.0)
        } else {
            (minimum, maximum)
        };

        let x_label = curve.column_labels.first().and_then(|text| text.literal());
        let x_axis = axis_summary(project, curve.x_bins.as_ref(), points, x_label, "X");
        let y_label = curve
            .line_labels
            .first()
            .and_then(|text| text.literal())
            .or_else(|| curve.column_labels.get(1).and_then(|text| text.literal()));
        let y_axis = AxisSummary {
            name: cells_name.to_owned(),
            label: y_label.map_or_else(|| cells_name.to_owned(), str::to_owned),
            units: spec.units.clone(),
            digits: spec.digits,
            values: vec![minimum],
        };

        Ok(Self {
            id: curve.name.clone(),
            name: cells_name.to_owned(),
            title: curve
                .title
                .literal()
                .map_or_else(|| curve.name.clone(), str::to_owned),
            cols: points,
            rows: 1,
            digits: spec.digits,
            units: spec.units.clone(),
            cells,
            states,
            changed,
            x_axis,
            y_axis,
            minimum,
            maximum,
            writable: !project.is_read_only() && tune.is_writable(cells_name),
        })
    }

    /// The value at `(row, column)`, if the cell exists.
    pub fn cell(&self, row: usize, column: usize) -> Option<f64> {
        (row < self.rows && column < self.cols)
            .then(|| {
                self.cells
                    .get(row.saturating_mul(self.cols).saturating_add(column))
                    .copied()
            })
            .flatten()
    }

    /// Number of cells that differ from the file baseline.
    pub fn changed_cells(&self) -> usize {
        self.changed.iter().filter(|changed| **changed).count()
    }
}

fn axis_summary(
    project: &Project,
    axis: Option<&oxtuner_ini::Axis>,
    length: usize,
    label: Option<&str>,
    fallback: &str,
) -> AxisSummary {
    let Some(axis) = axis else {
        return AxisSummary {
            name: fallback.to_owned(),
            label: fallback.to_owned(),
            units: String::new(),
            digits: 0,
            values: (0..length).map(|index| index as f64).collect(),
        };
    };

    let spec = project.layout().variable(&axis.array);
    let values = (0..length)
        .map(|index| {
            project
                .tune()
                .user_at(&axis.array, index)
                .unwrap_or(index as f64)
        })
        .collect();

    AxisSummary {
        name: spec.map_or_else(|| axis.array.clone(), |spec| spec.name.clone()),
        label: label.map_or_else(
            || {
                if axis.array.is_empty() {
                    fallback.to_owned()
                } else {
                    axis.array.clone()
                }
            },
            str::to_owned,
        ),
        units: spec.map_or_else(String::new, |spec| spec.units.clone()),
        digits: spec.map_or(0, |spec| spec.digits),
        values,
    }
}

impl DialogSummary {
    /// Reads one dialog's rows from an open project, panels flattened.
    ///
    /// A dialog is a layout: `Engine Constants` is a border of panels, and the
    /// constants sit one or two levels down. Reading only its own rows would
    /// show an empty list, so panels are walked in place and their rows keep the
    /// nested title as a section. Rows that name no tune variable and carry one
    /// field are the definition's own text, kept as notes. A built-in `std_*`
    /// panel cannot be drawn from the definition and is reported by name.
    ///
    /// Tables and curves a panel embeds are grids, not rows: the menu opens
    /// them, and only the grid's snapshot carries their values.
    pub(crate) fn from_project(project: &Project, name: &str) -> Result<Self, String> {
        let definition = project.definition();
        let dialog = definition
            .dialogs
            .get_ci(name)
            .ok_or_else(|| format!("the definition has no dialog named `{name}`"))?;

        let mut fields = Vec::new();
        let mut unavailable = Vec::new();
        collect_rows(
            project,
            &dialog.name,
            "",
            &mut HashSet::new(),
            &mut fields,
            &mut unavailable,
        );

        let changed = fields.iter().filter(|field| field.changed).count();
        Ok(Self {
            id: dialog.name.clone(),
            title: dialog
                .title
                .clone()
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| dialog.name.clone()),
            fields,
            changed,
            writable: !project.is_read_only(),
            unavailable,
        })
    }
}

/// Collects a dialog's rows, walking its panels in place.
///
/// `visited` holds lowercased dialog names: definitions reference panels in
/// cycles, and a dialog that appears twice in one layout is drawn once.
fn collect_rows(
    project: &Project,
    name: &str,
    section: &str,
    visited: &mut HashSet<String>,
    fields: &mut Vec<DialogFieldSummary>,
    unavailable: &mut Vec<String>,
) {
    if !visited.insert(name.to_ascii_lowercase()) {
        return;
    }
    let definition = project.definition();
    let Some(dialog) = definition.dialogs.get_ci(name) else {
        return;
    };

    for row in &dialog.rows {
        match row.kind {
            RowKind::Panel => {
                let Some(target) = row
                    .fields
                    .first()
                    .map(|field| oxtuner_ini::fields::unquote(field).trim())
                else {
                    continue;
                };
                if target.is_empty() {
                    continue;
                }
                if let Some(nested) = definition.dialogs.get_ci(target) {
                    let title = nested
                        .title
                        .as_deref()
                        .map(str::trim)
                        .filter(|title| !title.is_empty());
                    collect_rows(
                        project,
                        &nested.name,
                        title.unwrap_or(section),
                        visited,
                        fields,
                        unavailable,
                    );
                } else if target.to_ascii_lowercase().starts_with("std_")
                    && !unavailable
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(target))
                {
                    unavailable.push(target.to_owned());
                }
            }
            RowKind::Field
            | RowKind::DisplayOnlyField
            | RowKind::Slider
            | RowKind::Checkbox
            | RowKind::Radio
            | RowKind::SettingSelector => {
                if let Some(field) = field_row(project, row, section) {
                    fields.push(field);
                }
            }
            _ => {}
        }
    }
}

/// One readable row of a dialog.
///
/// The variable is the first field after the label that names a tune constant;
/// a row with one field and no variable is text the definition shows (a hint or
/// a warning), not a value.
fn field_row(project: &Project, row: &DialogRow, section: &str) -> Option<DialogFieldSummary> {
    let label = row
        .fields
        .first()
        .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
        .unwrap_or_default();

    let tune = project.tune();
    let variable = row
        .fields
        .iter()
        .skip(1)
        .map(|field| oxtuner_ini::fields::unquote(field).trim())
        .find(|field| project.layout().variable(field).is_some())
        .or_else(|| {
            // A row that names no other variable may itself be one.
            let first = row
                .fields
                .first()
                .map(|field| oxtuner_ini::fields::unquote(field).trim())
                .unwrap_or_default();
            (!first.is_empty() && project.layout().variable(first).is_some()).then_some(first)
        });

    let Some(variable) = variable else {
        if row.fields.len() != 1 || label.is_empty() {
            return None;
        }
        let warning = matches!(label.chars().next(), Some('!' | '#'));
        let text = if warning {
            label.get(1..).map(str::trim).unwrap_or_default().to_owned()
        } else {
            label
        };
        return Some(DialogFieldSummary {
            label: text.clone(),
            variable: String::new(),
            value: None,
            text: Some(text),
            units: String::new(),
            digits: 0,
            changed: false,
            display_only: true,
            writable: false,
            kind: DialogFieldKind::Note,
            section: section.to_owned(),
            warning,
        });
    };

    let spec = tune.variable(variable).ok()?;
    let baseline = project.images().baseline();
    let (value, text) = if spec.shape.is_scalar() && spec.shape.is_numeric() {
        (tune.user_at(variable, 0).ok(), None)
    } else if !spec.shape.is_numeric() {
        (None, tune.text(variable).ok().map(str::to_owned))
    } else {
        (None, None)
    };
    let changed = if spec.shape.is_scalar() && spec.shape.is_numeric() {
        matches!(
            (tune.raw_at(variable, 0), baseline.raw_at(variable, 0)),
            (Ok(current), Ok(saved)) if current != saved
        )
    } else {
        false
    };
    let display_only = row.kind == RowKind::DisplayOnlyField;
    let writable =
        !display_only && !project.is_read_only() && tune.is_writable(variable) && value.is_some();
    let kind = match row.kind {
        RowKind::Checkbox | RowKind::Radio => DialogFieldKind::Toggle,
        _ => DialogFieldKind::Value,
    };

    Some(DialogFieldSummary {
        label: if label.is_empty() {
            variable.to_owned()
        } else {
            label
        },
        variable: variable.to_owned(),
        value,
        text,
        units: if spec.shape.is_numeric() {
            spec.units.clone()
        } else {
            String::new()
        },
        digits: spec.digits,
        changed,
        display_only,
        writable,
        kind,
        section: section.to_owned(),
        warning: false,
    })
}

/// One command plus the id it will be answered under.
///
/// Internal to the crate: senders wrap a [`Command`], receivers unwrap it. A
/// command never reaches the core without an id, so "answered exactly once
/// while reading" is a property of the type rather than a convention; after
/// shutdown starts a missing answer is counted as dropped.
#[derive(Debug)]
pub(crate) struct Envelope {
    pub(crate) op: OpId,
    pub(crate) command: Command,
}

/// Re-exported here because [`Command`] carries it.
///
/// The type lives in [`crate::link`], next to the [`crate::CustomLink`] seam
/// that lets the application supply a transport the core does not build.
pub use crate::link::PortChoice;

/// One serial port the connect dialog can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSummary {
    /// Device path.
    pub name: String,
    /// The label to show (the path itself: `serial2` reports no product
    /// description).
    pub label: String,
}

/// How the ECU's signature compared with the definition's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureVerdict {
    /// Exactly the definition's signature.
    Match,
    /// Different text: the session is read-only (S1).
    Mismatch,
}

impl SignatureVerdict {
    /// A stable code for logs and the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
        }
    }
}

/// What the core is connected to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionSummary {
    /// Device path, or the custom link's label.
    pub port: String,
    /// Whether the link came from the application rather than a serial port.
    pub custom: bool,
    /// The signature the ECU reported.
    pub signature: String,
    /// The signature the definition expects.
    pub definition_signature: String,
    /// Whether they match.
    pub verdict: SignatureVerdict,
    /// Whether this session refuses writes (S1).
    pub read_only: bool,
    /// Why, when it does.
    pub read_only_reason: Option<String>,
    /// Whether the global write switch is on (S8). Off by default.
    pub writes_enabled: bool,
    /// The `EcuBehavior` that claimed the definition.
    pub behavior: String,
    /// Framing in use: `legacy` or `envelope`.
    pub framing: &'static str,
    /// Pages the definition declares.
    pub pages: usize,
    /// The realtime block size, when the definition declares one.
    pub realtime_block: Option<u16>,
}

impl ConnectionSummary {
    /// The ECU and the definition disagree, or the definition cannot be
    /// confirmed; the session is read-only either way (S1).
    pub fn is_mismatched(&self) -> bool {
        self.verdict == SignatureVerdict::Mismatch
    }
}

/// What a full page read did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadAllSummary {
    /// Pages read.
    pub pages: usize,
    /// Bytes read from the ECU.
    pub bytes: usize,
    /// Constants taken from those bytes.
    pub constants: usize,
    /// Constants that could not be placed (an unresolved address or shape).
    pub skipped: usize,
    /// Whether the signature matched when the read started.
    pub signature_verified: bool,
}

impl ReadAllSummary {
    /// Whether every declared page and cell was read.
    pub fn is_complete(&self) -> bool {
        self.skipped == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn summary() -> DefinitionSummary {
        DefinitionSummary {
            label: "test.ini".to_owned(),
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
            menu: Vec::new(),
            coverage_percent: 97.95,
            errors: 0,
            warnings: 0,
            active_flags: vec!["mcu_stm32".to_owned()],
        }
    }

    #[test]
    fn an_op_id_renders_with_its_prefix() {
        assert_eq!(OpId::new(7).to_string(), "op-7");
        assert_eq!(OpId::new(7).get(), 7);
    }

    #[test]
    fn only_replies_carry_the_op_they_answer() {
        let op = OpId::new(3);

        assert_eq!(Event::DefinitionUnloaded { op }.op(), Some(op));
        assert_eq!(
            Event::DefinitionLoaded {
                op,
                summary: summary()
            }
            .op(),
            Some(op)
        );
        assert_eq!(
            Event::TuneSaved {
                op,
                outcome: SaveSummary {
                    path: PathBuf::from("CurrentTune.msq"),
                    cells: 0,
                },
            }
            .op(),
            Some(op)
        );
        assert_eq!(Event::Stopped.op(), None);
    }

    #[test]
    fn a_command_constructed_from_bytes_uses_the_default_settings() {
        let Command::LoadDefinition {
            label, settings, ..
        } = Command::load_definition("test.ini", vec![0x5B])
        else {
            panic!("wrong variant");
        };

        assert_eq!(label, "test.ini");
        assert!(settings.tested_flags().is_empty());
    }

    #[test]
    fn summaries_are_ordered_by_flag_for_a_stable_ui() {
        let summary = summary();

        let flags: HashSet<_> = summary.active_flags.iter().cloned().collect();
        assert_eq!(flags.len(), summary.active_flags.len());
        assert!(summary.is_clean());
    }
}
