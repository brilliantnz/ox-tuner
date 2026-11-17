//! Replies from the core and the failures they can carry.

use std::path::PathBuf;

use oxtuner_datalog::SessionLogReport;
use oxtuner_tune::TuneStatus;

use crate::menu::MenuNode;

use super::project::DefinitionPreview;
use super::{
    BurnReport, ConnectionView, ControllerActionReport, DefinitionView, DialogView, FirmwareProbe,
    ImportReport, JournalEntry, OpId, PortView, ProjectView, ReadReport, RejectReason,
    RevertReport, SaveReport, TableView, WriteReport,
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
        view: DefinitionView,
    },
    /// A definition was inspected for the new-project wizard.
    DefinitionInspected {
        /// The command this answers.
        op: OpId,
        /// The definition that was read.
        path: PathBuf,
        /// The signature and setting groups the wizard renders.
        preview: DefinitionPreview,
    },
    /// A project is open and owned by the core: definition, tune images and the
    /// load gate's findings.
    ProjectOpened {
        /// The command this answers.
        op: OpId,
        /// What the project contains.
        view: ProjectView,
    },
    /// The open project's tune file was rewritten.
    TuneSaved {
        /// The command this answers.
        op: OpId,
        /// What the save rewrote.
        outcome: SaveReport,
    },
    /// The project's panel layout was stored, or there was no project to
    /// store it for.
    ProjectUiSaved {
        /// The command this answers.
        op: OpId,
    },
    /// A table was read from the open project.
    TableRead {
        /// The command this answers.
        op: OpId,
        /// The table as the grid will show it.
        table: TableView,
    },
    /// A table edit was accepted; `table` is the state after it.
    TableEdited {
        /// The command this answers.
        op: OpId,
        /// The table after the edit.
        table: TableView,
    },
    /// A dialog was read from the open project.
    DialogRead {
        /// The command this answers.
        op: OpId,
        /// The dialog as the editor will show it.
        dialog: DialogView,
    },
    /// A scalar edit was accepted; `dialog` is the state after it.
    DialogEdited {
        /// The command this answers.
        op: OpId,
        /// The dialog after the edit.
        dialog: DialogView,
    },
    /// One `[ControllerCommands]` action was sent.
    ///
    /// The ECU side is stale afterwards, because the action bypassed memory
    /// synchronization; a page read restores the derived states.
    ControllerActionRan {
        /// The command this answers.
        op: OpId,
        /// What was sent.
        report: ControllerActionReport,
    },
    /// Conflict marks were resolved after a read.
    ConflictsResolved {
        /// The command this answers.
        op: OpId,
    },
    /// A tune or connection change altered one or more menu gates.
    ///
    /// Only sent when the evaluated tree actually differs from the last one
    /// the core published. The tree's gates already include every ancestor's.
    MenuChanged {
        /// The evaluated navigation tree.
        menu: Vec<MenuNode>,
    },
    /// The serial ports the core last sampled, on explicit request.
    ///
    /// The core also pushes this snapshot on its own whenever it changes (see
    /// [`Self::PortsChanged`]), so this command is the on-demand resync: the
    /// Refresh button, or a check right before a scan that must not miss a
    /// freshly plugged device.
    PortsListed {
        /// The command this answers.
        op: OpId,
        /// The available ports.
        ports: Vec<PortView>,
    },
    /// The set of serial ports changed while the core was idle.
    ///
    /// Sent whenever the sampled list differs from the last one published,
    /// including the first sample when it found ports: a view that opens later
    /// renders the last published list and never has to ask for the initial
    /// one. A steady machine produces no traffic. The same snapshot answers
    /// [`super::Command::ListPorts`].
    PortsChanged {
        /// The ports as they are now.
        ports: Vec<PortView>,
    },
    /// A [`super::Command::TestConnection`] read a signature.
    ///
    /// The summary describes what a real connection would be: the same verdict
    /// and read-only rule, without a session or a page read.
    ConnectionTested {
        /// The command this answers.
        op: OpId,
        /// What the test found.
        view: ConnectionView,
    },
    /// A [`super::Command::ProbeFirmware`] read a signature without a
    /// definition.
    ///
    /// There is no verdict here because there is nothing to compare against:
    /// the signature is the first question, and the registry answers the next
    /// one.
    FirmwareProbed {
        /// The command this answers.
        op: OpId,
        /// The port the answer came from.
        port: String,
        /// The baud rate that answered, when the port was a serial device.
        baud: Option<u32>,
        /// The family probe that answered.
        probe: FirmwareProbe,
        /// The signature text the ECU sent.
        signature: String,
    },
    /// The ECU answered and the session is up.
    Connected {
        /// The command this answers.
        op: OpId,
        /// The live session.
        view: ConnectionView,
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
        report: ReadReport,
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
        report: WriteReport,
    },
    /// RAM was burned to flash and read back.
    TuneBurned {
        /// The command this answers.
        op: OpId,
        /// What was burned.
        report: BurnReport,
    },
    /// Local edits were discarded, after a snapshot.
    TuneReverted {
        /// The command this answers.
        op: OpId,
        /// What was reverted.
        report: RevertReport,
    },
    /// Another tune was imported into the working image.
    TuneImported {
        /// The command this answers.
        op: OpId,
        /// What was imported.
        report: ImportReport,
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
        connection: Option<ConnectionView>,
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
        report: Option<SessionLogReport>,
    },
    /// The link died; the core is either reconnecting or waiting for a command.
    ConnectionLost {
        /// Why, in text the UI can show.
        reason: String,
    },
    /// The link came back and the ECU answered again.
    Reconnected {
        /// The live session.
        view: ConnectionView,
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
            | Self::DefinitionInspected { op, .. }
            | Self::FirmwareProbed { op, .. }
            | Self::ProjectOpened { op, .. }
            | Self::TuneSaved { op, .. }
            | Self::ProjectUiSaved { op }
            | Self::TableRead { op, .. }
            | Self::TableEdited { op, .. }
            | Self::DialogRead { op, .. }
            | Self::DialogEdited { op, .. }
            | Self::ControllerActionRan { op, .. }
            | Self::ConflictsResolved { op }
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
            | Self::PortsChanged { .. }
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
    ControllerActionRefused,
    /// A controller command was sent, but the wire failed before it finished.
    ControllerActionFailed,
    /// The port could not be opened, the definition has no message set, or a
    /// connection is already up.
    ConnectFailed,
    /// The signature test could not run (no definition, port busy, no answer).
    TestFailed,
    /// A firmware probe reached no signature on the port it tried.
    FirmwareNotDetected,
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
    /// A native file or folder dialog could not return a choice.
    PickerFailed,
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
            Self::ControllerActionRefused => "controller-command-refused",
            Self::ControllerActionFailed => "controller-command-failed",
            Self::ConnectFailed => "connect-failed",
            Self::TestFailed => "test-failed",
            Self::FirmwareNotDetected => "firmware-not-detected",
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
            Self::PickerFailed => "picker-failed",
            Self::SnapshotFailed => "snapshot-failed",
            Self::JournalFailed => "journal-failed",
            Self::LogFailed => "log-failed",
            Self::Busy => "busy",
        }
    }
}
