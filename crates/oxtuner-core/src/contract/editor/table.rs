//! One table, shaped for the grid the UI draws.

use oxtuner_tune::CellState;

use super::curve::CurveAxes;

/// One table, shaped for the grid.
///
/// Values are in **user units**, row-major from the definition's array. The UI
/// keeps no tune values of its own: it renders this snapshot and asks for edits
/// by cell.
#[derive(Debug, Clone, PartialEq)]
pub struct TableView {
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
    /// Per-cell write state. `Unknown` until an ECU was read.
    pub states: Vec<CellState>,
    /// Per-cell "differs from the file baseline".
    pub changed: Vec<bool>,
    /// Column axis.
    pub x_axis: AxisView,
    /// Row axis.
    pub y_axis: AxisView,
    /// Lowest cell value, for the heatmap.
    pub minimum: f64,
    /// Highest cell value, for the heatmap.
    pub maximum: f64,
    /// Lowest user value the cell variable accepts, when the definition
    /// states one. An edit is clamped to this limit before it is shown.
    pub cell_minimum: Option<f64>,
    /// Highest user value the cell variable accepts, when stated.
    pub cell_maximum: Option<f64>,
    /// Whether the session may store edits.
    pub writable: bool,
    /// Whether a change to this table's cells needs an ECU power cycle.
    pub power_cycle: bool,
    /// The definition's axis frame when this snapshot is a curve.
    pub curve: Option<CurveAxes>,
}

/// One table axis, as the grid draws it.
///
/// Labels and units come from the table in use: the definition's `xyLabels`
/// override, otherwise the bins array's own label and units. Nothing in the
/// grid hardcodes them.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisView {
    /// Variable name the values come from.
    pub name: String,
    /// Label to show, from `xyLabels`, otherwise the variable name.
    pub label: String,
    /// Units.
    pub units: String,
    /// Live output channel tracking this axis, from the INI axis `channel`.
    /// The realtime follower resolves it to a value and then to a bin; `None`
    /// when the definition declares no channel.
    pub channel: Option<String>,
    /// Decimal digits to display.
    pub digits: u8,
    /// Lowest user value the bins variable accepts, when the definition states
    /// one. A bin edit is clamped to this limit rather than shown as accepted.
    pub minimum: Option<f64>,
    /// Highest user value the bins variable accepts, when stated.
    pub maximum: Option<f64>,
    /// Whether a change to this axis needs an ECU power cycle.
    pub power_cycle: bool,
    /// Axis values, in definition order.
    pub values: Vec<f64>,
}

impl AxisView {
    /// `label (units)`, or the bare label when unitless. Every caption of this
    /// axis renders through here, so the text always matches the table.
    #[must_use]
    pub fn caption(&self) -> String {
        if self.units.is_empty() {
            self.label.clone()
        } else {
            format!("{} ({})", self.label, self.units)
        }
    }

    /// ` units`, or nothing when unitless, for bin value labels.
    #[must_use]
    pub fn units_suffix(&self) -> String {
        if self.units.is_empty() {
            String::new()
        } else {
            format!(" {}", self.units)
        }
    }
}
