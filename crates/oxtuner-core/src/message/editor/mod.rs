//! Editor snapshots: dialogs, tables and curves.

mod curve;
mod dialog;
mod std_panels;
mod table;

pub use curve::CurveAxes;
pub use dialog::{
    DialogCommand, DialogFieldKind, DialogFieldSummary, DialogFieldTone, DialogHelpLink,
    DialogPreset, DialogPresetAssignment, DialogPresetOption, DialogSelectOption, DialogSummary,
    DialogUnmodeledRow, DialogWidget, DialogWidgetKind,
};
pub(crate) use dialog::{expression_slot, order_gates, panel_target};
pub(crate) use std_panels::synthesize;
pub use table::{AxisSummary, TableSummary};
