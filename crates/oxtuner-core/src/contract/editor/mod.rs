//! Editor snapshots: dialogs, tables and curves.

pub(crate) mod curve;
pub(crate) mod dialog;
pub(crate) mod table;

pub use curve::CurveAxes;
pub use dialog::{
    DialogAction, DialogEmbed, DialogField, DialogFieldKind, DialogFieldTone, DialogHelpLink,
    DialogPreset, DialogPresetAssignment, DialogPresetOption, DialogSelectOption,
    DialogUnmodeledRow, DialogView, DialogWidget, DialogWidgetKind,
};
pub use table::{AxisView, TableView};
