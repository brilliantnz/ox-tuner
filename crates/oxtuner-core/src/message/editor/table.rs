//! One table, shaped for the grid the UI draws.

use std::collections::HashSet;

use oxtuner_project::Project;
use oxtuner_tune::CellState;

use super::super::AxisKind;
use super::curve::{CurveAxes, curve_axes};

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
pub struct AxisSummary {
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

impl AxisSummary {
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

    #[expect(
        clippy::float_cmp,
        reason = "a cell is changed only when its stored raw value differs exactly"
    )]
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
            cell_minimum: spec.minimum,
            cell_maximum: spec.maximum,
            writable: !project.is_read_only() && tune.is_writable(cells_name),
            power_cycle: project
                .definition()
                .extensions
                .requires_power_cycle(cells_name),
            curve: None,
        })
    }

    /// A curve as a one-row table: the first `yBins` series is the row, the
    /// `xBins` are the column axis.
    #[expect(
        clippy::float_cmp,
        reason = "a cell is changed only when its stored raw value differs exactly"
    )]
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
            states.push(
                project
                    .images()
                    .cell_state(cells_name, 0, index)
                    .unwrap_or(CellState::Unknown),
            );
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
            channel: None,
            digits: spec.digits,
            minimum: spec.minimum,
            maximum: spec.maximum,
            power_cycle: false,
            values: vec![minimum],
        };
        let axes = curve_axes(curve, &x_axis.values, &cells);

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
            cell_minimum: spec.minimum,
            cell_maximum: spec.maximum,
            writable: !project.is_read_only() && tune.is_writable(cells_name),
            power_cycle: project
                .definition()
                .extensions
                .requires_power_cycle(cells_name),
            curve: Some(axes),
        })
    }

    /// The value at `(row, column)`, if the cell exists.
    #[must_use]
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
    #[must_use]
    pub fn changed_cells(&self) -> usize {
        self.changed.iter().filter(|changed| **changed).count()
    }
}

/// A synthetic axis position for the element at `index`.
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "axis lengths stay far below 2^53, where f64 is exact"
)]
fn axis_position(index: usize) -> f64 {
    index as f64
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
            channel: None,
            digits: 0,
            minimum: None,
            maximum: None,
            power_cycle: false,
            values: (0..length).map(axis_position).collect(),
        };
    };

    let spec = project.layout().variable(&axis.array);
    let values = (0..length)
        .map(|index| {
            project
                .tune()
                .user_at(&axis.array, index)
                .unwrap_or_else(|_| axis_position(index))
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
        channel: axis.channel.clone(),
        digits: spec.map_or(0, |spec| spec.digits),
        minimum: spec.and_then(|spec| spec.minimum),
        maximum: spec.and_then(|spec| spec.maximum),
        power_cycle: project
            .definition()
            .extensions
            .requires_power_cycle(&axis.array),
        values,
    }
}
