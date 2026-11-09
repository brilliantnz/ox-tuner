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

mod editor;
mod edits;
mod events;
mod frontpage;
mod gates;
mod ops;
mod project;

pub use editor::{
    AxisSummary, CurveAxes, DialogCommand, DialogEmbed, DialogFieldKind, DialogFieldSummary,
    DialogFieldTone, DialogHelpLink, DialogPreset, DialogPresetAssignment, DialogPresetOption,
    DialogSelectOption, DialogSummary, DialogUnmodeledRow, DialogWidget, DialogWidgetKind,
    TableSummary,
};
pub(crate) use editor::{expression_slot, order_gates, panel_target};
pub use edits::{
    BurnSummary, ImportSummary, JournalEntry, RevertSummary, SaveSummary, WriteSummary,
};
pub use events::{Event, Failure, FailureCode};
pub use frontpage::{FrontPageGauge, GaugeSummary, IndicatorSummary};
pub use gates::GateState;
pub(crate) use ops::Envelope;
pub use ops::{AxisEdit, AxisKind, CellEdit, Command, OpId, RejectReason};
pub use project::{
    ConnectionSummary, DefinitionSummary, KeyActionSummary, PortSummary, ProjectSummary,
    ReadAllSummary, ReferenceTableIdentifier, ReferenceTableLimit, ReferenceTableSolution,
    ReferenceTableSummary, SignatureVerdict,
};
