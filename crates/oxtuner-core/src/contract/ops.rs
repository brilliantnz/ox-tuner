//! Commands the UI sends and the ids that track their replies.

use std::{fmt, path::PathBuf};

use oxtuner_ini::SettingsContext;
use oxtuner_protocol::LinkMode;

use crate::link::PortChoice;

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
    #[must_use]
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
#[expect(
    clippy::exhaustive_enums,
    reason = "a table has exactly an X and a Y axis"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisKind {
    /// The column bins (`xBins`).
    X,
    /// The row bins (`yBins`).
    Y,
}

impl AxisKind {
    /// A stable code for logs.
    #[must_use]
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
#[expect(
    clippy::exhaustive_enums,
    reason = "the UI and the core compile together, so the command set is closed"
)]
#[derive(Debug)]
pub enum Command {
    /// Parse `bytes` and take ownership of the resulting definition.
    ///
    /// `settings` is the project's settings snapshot (`<settings>` in the
    /// `.msq`); `#if` branches and the protocol commands they rewrite are
    /// resolved with it. A definition that replaces an already
    /// loaded one is allowed — the core drops the old one.
    LoadDefinition {
        /// What to call the source in logs and the UI (usually a file name).
        label: String,
        /// The definition file, as read from disk.
        bytes: Vec<u8>,
        /// Settings the `#if` branches resolve against.
        settings: SettingsContext,
    },
    /// Open a `TunerStudio` project folder in place.
    ///
    /// The core reads the definition and the tune, owns both, and answers with
    /// [`super::Event::ProjectOpened`] — or [`super::Event::Failed`] when the folder is not a
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
    /// [`super::FailureCode::TuneSaveFailed`] and the read-only reasons.
    SaveTune,
    /// Read one table from the open project, shaped for the grid.
    ///
    /// `name` is the table id, its 3-D map id, its cell variable, or a curve
    /// name (which reads as a one-row table). The reply is
    /// [`super::Event::TableRead`] or [`super::Event::Failed`] with
    /// [`super::FailureCode::TableUnavailable`].
    ReadTable {
        /// Table id as written in the definition, its map id, a curve name, or
        /// the cell variable the grid edits.
        name: String,
    },
    /// Read one dialog from the open project, shaped for the constant list.
    ///
    /// The reply is [`super::Event::DialogRead`] or [`super::Event::Failed`] with
    /// [`super::FailureCode::DialogUnavailable`].
    ReadDialog {
        /// Dialog name as written in the definition.
        name: String,
    },
    /// Apply one scalar edit to a dialog's variable, in user units.
    ///
    /// The edit is validated before anything is stored. The reply is
    /// [`super::Event::DialogEdited`] with the dialog after the edit, or
    /// [`super::Event::Failed`] with [`super::FailureCode::DialogEditRejected`].
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
    /// request leaves the tune untouched. The reply is [`super::Event::TableEdited`]
    /// with the table after the edit, or [`super::Event::Failed`] with
    /// [`super::FailureCode::TableEditRejected`].
    EditTable {
        /// The cell variable's name (the table's `zBins`).
        name: String,
        /// Cells to write, in user units.
        edits: Vec<CellEdit>,
    },
    /// Run one `[ControllerCommands]` action from a dialog button.
    ///
    /// A controller command is a raw ECU action: it bypasses memory
    /// synchronization, so the working image cannot assume the ECU still holds
    /// what the last read saw. The core therefore requires the same gate as a
    /// write and marks the ECU side stale after the command succeeds. Only an
    /// explicit click sends it; nothing runs on open, close or edit.
    ///
    /// The reply is [`super::Event::ControllerActionRan`] or
    /// [`super::Event::Failed`] with
    /// [`super::FailureCode::ControllerActionRefused`] or
    /// [`super::FailureCode::ControllerActionFailed`].
    RunControllerCommand {
        /// Dialog the button was pressed in, for the echo.
        dialog: String,
        /// `[ControllerCommands]` name.
        command: String,
    },
    /// Apply user-unit edits to one table's axis bins.
    ///
    /// `table` is the table id (or its 3-D map id, or a curve name — anything
    /// `TableView::from_project` resolves). Every edit is validated before
    /// any bin is stored. The reply is [`super::Event::TableEdited`] with the table
    /// after the edit, or [`super::Event::Failed`] with
    /// [`super::FailureCode::TableEditRejected`].
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
    /// and answers with [`super::Event::Connected`] (or [`super::Event::Failed`] with
    /// [`super::FailureCode::ConnectFailed`]). The definition must be loaded; without
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
    /// The reply is [`super::Event::ConnectionTested`] with the same
    /// [`super::ConnectionView`] a real connection would report, or a failure with
    /// [`super::FailureCode::TestFailed`]. A live connection must be closed first.
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
    /// a change. The write gate, clamping and read-back verification apply to
    /// this path.
    WriteToRam {
        /// INI page numbers, or empty for every changed page.
        pages: Vec<u8>,
    },
    /// Burn RAM content to flash.
    ///
    /// Only an explicit user command reaches this variant; nothing burns
    /// automatically.
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
    /// A snapshot of the working image is written first.
    RevertTune,
    /// Discard the local edits one table, curve or dialog owns.
    ///
    /// `name` is resolved like [`Self::ReadTable`]: a table id, a 3-D map id, a
    /// curve name, or a dialog name (whose embedded table goes with it). Every
    /// other target keeps its edits. A snapshot of the working image is
    /// written first. The reply is [`super::Event::TableEdited`] and/or
    /// [`super::Event::DialogEdited`] with the target after the revert, plus
    /// [`super::Event::TuneStatusRead`].
    RevertTarget {
        /// Table id, curve name or dialog name.
        name: String,
    },
    /// Import another `.msq` into the working image.
    ///
    /// A tune whose signature differs from the loaded definition needs
    /// `consented: true`; without it the core refuses with
    /// [`super::FailureCode::ImportNeedsConsent`] and changes nothing.
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
    /// Turn the global write path on or off.
    ///
    /// Writes start disabled. Enabling is an explicit user choice; disabling is
    /// immediate and always allowed.
    SetWritesEnabled {
        /// Whether writes may run from now on.
        enabled: bool,
    },
    /// Clear a halted write path after the user reviewed a failed write.
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
    #[must_use]
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
            Self::RunControllerCommand { .. } => "run-controller-command",
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
            Self::RevertTarget { .. } => "revert-target",
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

/// Why a command was accepted and then not executed.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The core was stopping; the command never touched any resource.
    ShuttingDown,
}

impl RejectReason {
    /// A stable code for logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShuttingDown => "shutting-down",
        }
    }
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
