//! The core's message vocabulary.
//!
//! These types are the whole UI↔core contract.
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
//!   dropped and counted instead of blocking the quit.

pub(crate) mod editor;
mod edits;
mod events;
pub(crate) mod frontpage;
mod gates;
mod ops;
pub(crate) mod project;

pub use editor::{
    AxisView, CurveAxes, DialogAction, DialogEmbed, DialogField, DialogFieldKind, DialogFieldTone,
    DialogHelpLink, DialogPreset, DialogPresetAssignment, DialogPresetOption, DialogSelectOption,
    DialogUnmodeledRow, DialogView, DialogWidget, DialogWidgetKind, TableView,
};
pub use edits::{
    BurnReport, ControllerActionReport, ImportReport, JournalEntry, RevertReport, SaveReport,
    WriteReport,
};
pub use events::{Event, Failure, FailureCode};
pub use frontpage::{FrontPageGauge, GaugeTemplate, IndicatorSpec};
pub use gates::GateState;
pub use ops::{
    AxisEdit, AxisKind, CellEdit, ConflictResolution, CoreCommand, DefinitionCommand, EcuCommand,
    JournalCommand, LifecycleCommand, LogCommand, MAX_POLL_HZ, MIN_POLL_HZ, OpId, ProjectCommand,
    RejectReason, SafetyCommand, TuneCommand,
};
pub(crate) use ops::{CommandRequest, Envelope};
pub use project::{
    ConflictReport, ConnectionView, DefinitionView, KeyActionInfo, PortView, ProjectConnection,
    ProjectView, ReadReport, ReferenceTableIdentifier, ReferenceTableLimit, ReferenceTableSolution,
    ReferenceTableView, SignatureVerdict,
};
