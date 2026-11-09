//! Replies from the core and the failures they can carry.

use std::path::PathBuf;

use oxtuner_datalog::LogSummary;
use oxtuner_tune::TuneStatus;

use crate::menu::MenuNode;

use super::{
    BurnSummary, ConnectionSummary, ControllerCommandSummary, DefinitionSummary, DialogSummary,
    ImportSummary, JournalEntry, OpId, PortSummary, ProjectSummary, ReadAllSummary, RejectReason,
    RevertSummary, SaveSummary, TableSummary, WriteSummary,
};

/// A reply from the core.
#[expect(
    clippy::exhaustive_enums,
    reason = "the UI↔core contract enumerates every reply the core can send"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The definition is loaded and owned by the core.
    DefinitionLoaded {
        /// The command this answers.
        op: OpId,
        /// What the definition contains.
        summary: DefinitionSummary,
    },
    /// A project is open and owned by the core: definition, tune images and the
    /// load gate's findings.
    ProjectOpened {
        /// The command this answers.
        op: OpId,
        /// What the project contains.
        summary: ProjectSummary,
    },
    /// The open project's tune file was rewritten.
    TuneSaved {
        /// The command this answers.
        op: OpId,
        /// What the save rewrote.
        outcome: SaveSummary,
    },
    /// A table was read from the open project.
    TableRead {
        /// The command this answers.
        op: OpId,
        /// The table as the grid will show it.
        table: TableSummary,
    },
    /// A table edit was accepted; `table` is the state after it.
    TableEdited {
        /// The command this answers.
        op: OpId,
        /// The table after the edit.
        table: TableSummary,
    },
    /// A dialog was read from the open project.
    DialogRead {
        /// The command this answers.
        op: OpId,
        /// The dialog as the editor will show it.
        dialog: DialogSummary,
    },
    /// A scalar edit was accepted; `dialog` is the state after it.
    DialogEdited {
        /// The command this answers.
        op: OpId,
        /// The dialog after the edit.
        dialog: DialogSummary,
    },
    /// One `[ControllerCommands]` action was sent.
    ///
    /// The ECU side is stale afterwards, because the action bypassed memory
    /// synchronization; a page read restores the derived states.
    ControllerCommandRan {
        /// The command this answers.
        op: OpId,
        /// What was sent.
        summary: ControllerCommandSummary,
    },
    /// A tune or connection change altered one or more menu gates.
    ///
    /// Only sent when the evaluated tree actually differs from the last one
    /// the core published. The tree's gates already include every ancestor's.
    MenuChanged {
        /// The evaluated navigation tree.
        menu: Vec<MenuNode>,
    },
    /// The serial ports the connect dialog can offer.
    PortsListed {
        /// The command this answers.
        op: OpId,
        /// The available ports.
        ports: Vec<PortSummary>,
    },
    /// A [`super::Command::TestConnection`] read a signature.
    ///
    /// The summary describes what a real connection would be: the same verdict
    /// and read-only rule, without a session or a page read.
    ConnectionTested {
        /// The command this answers.
        op: OpId,
        /// What the test found.
        summary: ConnectionSummary,
    },
    /// The ECU answered and the session is up.
    Connected {
        /// The command this answers.
        op: OpId,
        /// The live session.
        summary: ConnectionSummary,
    },
    /// The connection was closed on request.
    Disconnected {
        /// The command this answers.
        op: OpId,
    },
    /// Every declared page was read into the tune images.
    PagesRead {
        /// The command this answers.
        op: OpId,
        /// What the read covered.
        summary: ReadAllSummary,
    },
    /// The ECU image became the working tune, so the editor shows what the
    /// controller holds.
    EcuAdopted {
        /// The command this answers.
        op: OpId,
    },
    /// Periodic polling started or stopped.
    RealtimeState {
        /// The command this answers.
        op: OpId,
        /// Whether polling is running.
        running: bool,
    },
    /// The session trace was written.
    TraceSaved {
        /// The command this answers.
        op: OpId,
        /// Where the trace was written.
        path: PathBuf,
        /// Frames in the trace.
        frames: usize,
    },
    /// Changed values were written to ECU RAM and verified by a read-back.
    TuneWritten {
        /// The command this answers.
        op: OpId,
        /// What was written.
        summary: WriteSummary,
    },
    /// RAM was burned to flash and read back.
    TuneBurned {
        /// The command this answers.
        op: OpId,
        /// What was burned.
        summary: BurnSummary,
    },
    /// Local edits were discarded, after a snapshot.
    TuneReverted {
        /// The command this answers.
        op: OpId,
        /// What was reverted.
        summary: RevertSummary,
    },
    /// Another tune was imported into the working image.
    TuneImported {
        /// The command this answers.
        op: OpId,
        /// What was imported.
        summary: ImportSummary,
    },
    /// The project's snapshots, oldest first.
    SnapshotsListed {
        /// The command this answers.
        op: OpId,
        /// Snapshot files, oldest first.
        paths: Vec<PathBuf>,
    },
    /// The derived write state after a read, edit, write, burn or import.
    TuneStatusRead {
        /// The command this answers.
        op: OpId,
        /// The derived state.
        status: TuneStatus,
    },
    /// The global write switch changed.
    WriteAccessChanged {
        /// The command this answers.
        op: OpId,
        /// Whether writes are now enabled.
        writes_enabled: bool,
        /// The live session's summary after the change, when one is open.
        connection: Option<ConnectionSummary>,
    },
    /// A halted write path was cleared after review.
    WriteFailureAcknowledged {
        /// The command this answers.
        op: OpId,
    },
    /// The project's write journal, newest last.
    JournalRead {
        /// The command this answers.
        op: OpId,
        /// The journal file, when one exists.
        path: Option<PathBuf>,
        /// The journal entries, newest last.
        entries: Vec<JournalEntry>,
    },
    /// A session log started or stopped.
    LogState {
        /// The command this answers.
        op: OpId,
        /// Whether logging is running.
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
    Reconnected {
        /// The live session.
        summary: ConnectionSummary,
    },
    /// The loaded definition (and tune, if a project was open) was dropped.
    DefinitionUnloaded {
        /// The command this answers.
        op: OpId,
    },
    /// The command was handled but could not do its work.
    Failed {
        /// The command this answers.
        op: OpId,
        /// What went wrong.
        failure: Failure,
    },
    /// The command was accepted, then never executed because the core stopped.
    Rejected {
        /// The command this answers.
        op: OpId,
        /// Why it was not executed.
        reason: RejectReason,
    },
    /// The core thread has stopped. Always the last event.
    Stopped,
}

impl Event {
    /// The operation this event answers, if it answers one.
    #[must_use]
    pub fn op(&self) -> Option<OpId> {
        match self {
            Self::DefinitionLoaded { op, .. }
            | Self::ProjectOpened { op, .. }
            | Self::TuneSaved { op, .. }
            | Self::TableRead { op, .. }
            | Self::TableEdited { op, .. }
            | Self::DialogRead { op, .. }
            | Self::DialogEdited { op, .. }
            | Self::ControllerCommandRan { op, .. }
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
            Self::MenuChanged { .. }
            | Self::ConnectionLost { .. }
            | Self::Reconnected { .. }
            | Self::Stopped => None,
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
    /// The bytes are not a definition this build can read. An unsupported
    /// `iniSpecVersion` fails loudly rather than half-parsing.
    DefinitionRejected,
    /// The folder is not an openable `TunerStudio` project, or a file in it could
    /// not be read or parsed.
    ProjectRejected,
    /// The tune file could not be written, or the session is read-only.
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
    /// A controller command was refused before any byte was sent: the
    /// definition does not declare it, the session is not connected, writes are
    /// off, the signature does not match, the path is halted after a failed
    /// verification, or another operation owns the link.
    ControllerCommandRefused,
    /// A controller command was sent, but the wire failed before it finished.
    ControllerCommandFailed,
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
    /// A write was refused before any byte was sent: writes are off, the
    /// signature does not match, the path is halted after a failed
    /// verification, the session is not connected, or nothing changed.
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
    /// approved the import.
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
    #[must_use]
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
            Self::ControllerCommandRefused => "controller-command-refused",
            Self::ControllerCommandFailed => "controller-command-failed",
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
