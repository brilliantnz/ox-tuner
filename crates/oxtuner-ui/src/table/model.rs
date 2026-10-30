//! The table's state, free of GPUI types.
//!
//! Selection, edit parsing, copy/paste and the display flip are plain Rust so
//! they can be tested without a window (ADR-0013). The view in [`super::Table`]
//! renders this state and turns its requested edits into events; the core stays
//! the only owner of tune values.
//!
//! Row coordinates are **definition rows**. When the Y axis is flipped the grid
//! draws them in reverse, and every cursor/pointer move is translated through
//! [`TableModel::data_row`] and [`TableModel::display_row`].

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use oxtuner_core::{AxisEdit, AxisKind, CellEdit, CellState, TableSummary};

/// How many displayed rows PageUp/PageDown move.
pub const PAGE_ROWS: usize = 10;

/// One cell, in definition coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellPos {
    /// Row from the top of the definition's array.
    pub row: usize,
    /// Column from the left of the definition's array.
    pub column: usize,
}

/// A rectangular selection, inclusive on both corners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The corner that stays put while extending.
    pub anchor: CellPos,
    /// The moving corner.
    pub cursor: CellPos,
}

impl Selection {
    /// A selection of one cell.
    pub fn single(at: CellPos) -> Self {
        Self {
            anchor: at,
            cursor: at,
        }
    }

    /// `(top, left, bottom, right)`, normalized so the anchor may be after the
    /// cursor.
    pub fn bounds(self) -> (usize, usize, usize, usize) {
        (
            self.anchor.row.min(self.cursor.row),
            self.anchor.column.min(self.cursor.column),
            self.anchor.row.max(self.cursor.row),
            self.anchor.column.max(self.cursor.column),
        )
    }

    /// Whether `cell` is inside the rectangle.
    pub fn contains(self, cell: CellPos) -> bool {
        let (top, left, bottom, right) = self.bounds();
        (top..=bottom).contains(&cell.row) && (left..=right).contains(&cell.column)
    }

    /// Number of cells in the rectangle.
    pub fn cell_count(self) -> usize {
        let (top, left, bottom, right) = self.bounds();
        (bottom - top + 1) * (right - left + 1)
    }
}

/// The edit being typed into the cursor cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editing {
    /// Cell the text belongs to.
    pub cell: CellPos,
    /// Text as typed.
    pub text: String,
    /// Whether the last commit attempt could not parse the text.
    pub invalid: bool,
}

/// The edit being typed into an axis header bin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisTarget {
    /// Which axis the bin belongs to.
    pub axis: AxisKind,
    /// Bin index along the axis.
    pub index: usize,
}

/// The in-progress header edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AxisEditing {
    /// Bin the text belongs to.
    pub target: AxisTarget,
    /// Text as typed.
    pub text: String,
    /// Whether the last commit attempt could not parse the text.
    pub invalid: bool,
}

/// Why an edit could not be accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The text is empty.
    Empty,
    /// The text is not a finite number after its prefix.
    NotANumber,
    /// A `/` operation used zero.
    DivideByZero,
    /// No table is loaded.
    NoTable,
    /// The table is read-only.
    ReadOnly,
    /// The pasted block does not fit at the selection.
    OutOfBounds,
    /// A pasted block has rows of different widths.
    RaggedPaste,
    /// The selection is too small for the operation.
    SelectionTooSmall,
    /// The axis has too few bins to respace.
    AxisTooSmall,
    /// A percent sign was typed without a `+` or `-`.
    UnsupportedPercent,
}

/// What the last command did, for a localized status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridAction {
    /// The selection was copied.
    Copied {
        /// Number of cells.
        cells: usize,
    },
    /// A block was pasted.
    Pasted {
        /// Number of cells.
        cells: usize,
    },
    /// The clipboard did not fit or did not parse.
    PasteRejected,
    /// A selection was interpolated.
    Interpolated {
        /// Number of cells written.
        cells: usize,
    },
    /// A selection was smoothed.
    Smoothed {
        /// Number of cells written.
        cells: usize,
    },
    /// A selection was equalized.
    Equalized {
        /// Number of cells written.
        cells: usize,
    },
    /// A row or column was filled from its first line.
    Filled {
        /// Number of cells written.
        cells: usize,
    },
    /// A table edit was undone.
    Undone {
        /// Number of cells restored.
        cells: usize,
    },
    /// A table edit was redone.
    Redone {
        /// Number of cells rewritten.
        cells: usize,
    },
    /// An axis bin edit was undone.
    UndoneAxis {
        /// Number of bins restored.
        bins: usize,
    },
    /// An axis bin edit was redone.
    RedoneAxis {
        /// Number of bins rewritten.
        bins: usize,
    },
    /// The selection is too small for the requested operation.
    TooSmall,
    /// An axis was respaced onto even steps.
    RebinnedAxis {
        /// Number of bins that moved.
        bins: usize,
    },
}

/// A bulk operation over the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BulkOp {
    /// Draw a straight line across each selected row.
    InterpolateRows,
    /// Draw a straight line down each selected column.
    InterpolateColumns,
    /// Average each cell with its orthogonal neighbours inside the selection.
    Smooth,
    /// Replace every selected cell with the selection's mean.
    Equalize,
    /// Copy the top displayed row down through the selected rows.
    FillDown,
    /// Copy the leftmost selected column across the selected columns.
    FillRight,
}

/// How a typed number applies to the selected cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditOp {
    /// Replace with the typed value.
    Set,
    /// Add the typed value.
    Add,
    /// Multiply by the typed factor.
    Multiply,
    /// Divide by the typed divisor.
    Divide,
    /// Change by a percentage of each cell's current value.
    Percent,
}

/// One recorded command, as the values on each side of it.
#[derive(Clone)]
pub enum HistoryStep {
    /// Cell values.
    Cells(Vec<CellEdit>),
    /// Axis bins.
    Axis {
        /// Which axis the bins belong to.
        axis: AxisKind,
        /// The bins.
        edits: Vec<AxisEdit>,
    },
}

/// A recorded command: what to write to undo it and what to write to redo it.
struct HistoryEntry {
    before: HistoryStep,
    after: HistoryStep,
}

/// How many commands the grid remembers.
const HISTORY_LIMIT: usize = 100;

/// The table grid's model.
pub struct TableModel {
    table: Option<TableSummary>,
    selection: Selection,
    editing: Option<Editing>,
    axis_editing: Option<AxisEditing>,
    flip_y: bool,
    pending: HashMap<(usize, usize), f64>,
    pending_axis: HashMap<(bool, usize), f64>,
    action: Option<GridAction>,
    /// Commands that can be undone, oldest first.
    undo: Vec<HistoryEntry>,
    /// Commands that can be redone, most recently undone last.
    redo: Vec<HistoryEntry>,
    /// Formatted cell values, rebuilt when the snapshot or an optimistic value
    /// changes. Rendering 1 024 cells must not format 1 024 strings per frame.
    display: Vec<Arc<str>>,
    /// Accessible labels for the same cells.
    aria: Vec<Arc<str>>,
}

impl Default for TableModel {
    fn default() -> Self {
        Self::new()
    }
}

impl TableModel {
    /// An empty model with no table.
    pub fn new() -> Self {
        Self {
            table: None,
            selection: Selection::single(CellPos { row: 0, column: 0 }),
            editing: None,
            axis_editing: None,
            flip_y: false,
            pending: HashMap::new(),
            pending_axis: HashMap::new(),
            action: None,
            undo: Vec::new(),
            redo: Vec::new(),
            display: Vec::new(),
            aria: Vec::new(),
        }
    }

    /// Replaces the snapshot. A different table resets selection and edit state;
    /// the same table (an echo after an edit) keeps the cursor where it is and
    /// drops only the optimistic values the core has confirmed.
    pub fn set_table(&mut self, table: TableSummary) {
        let same_table = self.table.as_ref().map(|current| &current.id) == Some(&table.id);
        if !same_table {
            // Start at the top displayed cell. The default chart orientation
            // draws the highest bin at the top, so its data row is the last
            // one. Flipping the axis puts the lowest bin there instead.
            let top_row = if self.flip_y {
                0
            } else {
                table.rows.saturating_sub(1)
            };
            self.selection = Selection::single(CellPos {
                row: top_row,
                column: 0,
            });
            self.editing = None;
            self.axis_editing = None;
            self.pending.clear();
            self.pending_axis.clear();
            self.undo.clear();
            self.redo.clear();
        } else {
            self.pending.retain(|(row, column), pending| {
                table
                    .cell(*row, *column)
                    .is_none_or(|current| current != *pending)
            });
            self.pending_axis.retain(|(is_x, index), pending| {
                let axis = if *is_x { &table.x_axis } else { &table.y_axis };
                axis.values
                    .get(*index)
                    .is_none_or(|current| *current != *pending)
            });
        }
        self.table = Some(table);
        self.clamp();
        self.rebuild_labels();
    }

    /// The formatted value of one cell.
    pub fn display(&self, cell: CellPos) -> Option<Arc<str>> {
        self.index_of(cell)
            .and_then(|index| self.display.get(index).cloned())
    }

    /// The accessible label of one cell.
    pub fn aria(&self, cell: CellPos) -> Option<Arc<str>> {
        self.index_of(cell)
            .and_then(|index| self.aria.get(index).cloned())
    }

    /// Drops the snapshot, keeping the display preference.
    pub fn clear_table(&mut self) {
        self.table = None;
        self.pending.clear();
        self.pending_axis.clear();
        self.display.clear();
        self.aria.clear();
        self.editing = None;
        self.axis_editing = None;
        self.selection = Selection::single(CellPos { row: 0, column: 0 });
        self.action = None;
        self.undo.clear();
        self.redo.clear();
    }

    /// The rendered snapshot.
    pub fn table(&self) -> Option<&TableSummary> {
        self.table.as_ref()
    }

    /// Rows in the table.
    pub fn rows(&self) -> usize {
        self.table.as_ref().map_or(0, |table| table.rows)
    }

    /// Columns in the table.
    pub fn columns(&self) -> usize {
        self.table.as_ref().map_or(0, |table| table.cols)
    }

    /// The current selection.
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// The in-progress edit, when there is one.
    pub fn editing(&self) -> Option<&Editing> {
        self.editing.as_ref()
    }

    /// The in-progress header edit, when there is one.
    pub fn axis_editing(&self) -> Option<&AxisEditing> {
        self.axis_editing.as_ref()
    }

    /// Turns the displayed Y axis upside down without moving the selected data
    /// cell.
    pub fn set_flip_y(&mut self, flip: bool) {
        self.flip_y = flip;
        self.action = None;
    }

    /// Whether the Y axis is drawn downward.
    ///
    /// The default (`false`) is the chart orientation: the lowest bin at the
    /// bottom and the highest at the top. Flipped draws the lowest bin at the
    /// top, spreadsheet style.
    pub fn flip_y(&self) -> bool {
        self.flip_y
    }

    /// The last command's result, until the next input.
    pub fn action(&self) -> Option<GridAction> {
        self.action
    }

    /// Forgets the last command's result.
    pub fn clear_action(&mut self) {
        self.action = None;
    }

    /// Forgets optimistic values after the core rejected an edit.
    ///
    /// The rejected command is the newest history entry: the core answers one
    /// command at a time, so dropping it keeps undo from re-sending a write the
    /// tune never accepted.
    pub fn reject_pending(&mut self) {
        // The same failure can be observed more than once. Only a write that
        // still has an optimistic value has an entry to drop; a second call
        // must not eat an older, confirmed step.
        if self.pending.is_empty() && self.pending_axis.is_empty() {
            return;
        }
        self.undo.pop();
        self.redo.clear();
        self.pending.clear();
        self.pending_axis.clear();
        self.rebuild_labels();
    }

    /// Whether there is a command to undo.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether there is a command to redo.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Takes the newest command back, returning the values that restore it.
    pub fn undo(&mut self) -> Option<HistoryStep> {
        let entry = self.undo.pop()?;
        let step = entry.before.clone();
        self.apply_step(&step);
        self.action = Some(match &step {
            HistoryStep::Cells(edits) => GridAction::Undone { cells: edits.len() },
            HistoryStep::Axis { edits, .. } => GridAction::UndoneAxis { bins: edits.len() },
        });
        self.redo.push(entry);
        Some(step)
    }

    /// Re-applies the newest undone command, returning the values to write.
    pub fn redo(&mut self) -> Option<HistoryStep> {
        let entry = self.redo.pop()?;
        let step = entry.after.clone();
        self.apply_step(&step);
        self.action = Some(match &step {
            HistoryStep::Cells(edits) => GridAction::Redone { cells: edits.len() },
            HistoryStep::Axis { edits, .. } => GridAction::RedoneAxis { bins: edits.len() },
        });
        self.undo.push(entry);
        Some(step)
    }

    /// The displayed value, including an optimistic edit not yet echoed by the
    /// core.
    pub fn value(&self, cell: CellPos) -> f64 {
        if let Some(value) = self.pending.get(&(cell.row, cell.column)) {
            return *value;
        }
        self.table
            .as_ref()
            .and_then(|table| table.cell(cell.row, cell.column))
            .unwrap_or(0.0)
    }

    /// The core's write state for one cell.
    pub fn state(&self, cell: CellPos) -> CellState {
        let Some(table) = &self.table else {
            return CellState::Unknown;
        };
        table
            .states
            .get(
                cell.row
                    .saturating_mul(table.cols)
                    .saturating_add(cell.column),
            )
            .copied()
            .unwrap_or(CellState::Unknown)
    }

    /// Whether the cell differs from the file baseline.
    pub fn changed(&self, cell: CellPos) -> bool {
        let Some(table) = &self.table else {
            return false;
        };
        table
            .changed
            .get(
                cell.row
                    .saturating_mul(table.cols)
                    .saturating_add(cell.column),
            )
            .copied()
            .unwrap_or(false)
    }

    /// Whether edits are allowed.
    pub fn is_writable(&self) -> bool {
        self.table.as_ref().is_some_and(|table| table.writable)
    }

    /// Bins along one axis.
    pub fn axis_len(&self, axis: AxisKind) -> usize {
        self.table.as_ref().map_or(0, |table| match axis {
            AxisKind::X => table.x_axis.values.len(),
            AxisKind::Y => table.y_axis.values.len(),
        })
    }

    /// A bin value, including an optimistic edit not yet echoed by the core.
    pub fn axis_value(&self, axis: AxisKind, index: usize) -> f64 {
        let key = (matches!(axis, AxisKind::X), index);
        if let Some(value) = self.pending_axis.get(&key) {
            return *value;
        }
        self.table
            .as_ref()
            .and_then(|table| match axis {
                AxisKind::X => table.x_axis.values.get(index).copied(),
                AxisKind::Y => table.y_axis.values.get(index).copied(),
            })
            .unwrap_or(0.0)
    }

    /// The live output channel tracking one axis, from the table in use.
    ///
    /// A realtime follower resolves each channel to a value and then to a bin
    /// with [`TableModel::realtime_cell`]; the grid itself never names a
    /// channel.
    pub fn axis_channel(&self, axis: AxisKind) -> Option<&str> {
        self.table.as_ref().and_then(|table| match axis {
            AxisKind::X => table.x_axis.channel.as_deref(),
            AxisKind::Y => table.y_axis.channel.as_deref(),
        })
    }

    /// The column whose bin sits closest to a live X value, ties going down.
    pub fn nearest_column(&self, x_value: f64) -> Option<usize> {
        let table = self.table.as_ref()?;
        nearest_bin(&table.x_axis.values, x_value)
    }

    /// The definition row whose bin sits closest to a live Y value.
    pub fn nearest_row(&self, y_value: f64) -> Option<usize> {
        let table = self.table.as_ref()?;
        nearest_bin(&table.y_axis.values, y_value)
    }

    /// The cell a live `(x, y)` point sits on, for the realtime follower.
    pub fn realtime_cell(&self, x_value: f64, y_value: f64) -> Option<CellPos> {
        Some(CellPos {
            row: self.nearest_row(y_value)?,
            column: self.nearest_column(x_value)?,
        })
    }

    /// Whether an axis bin can be edited.
    pub fn is_axis_writable(&self, axis: AxisKind, index: usize) -> bool {
        if !self.is_writable() {
            return false;
        }
        index < self.axis_len(axis)
    }

    /// Whether the axis bins are strictly increasing.
    ///
    /// TunerStudio lets a bin hold any value, but a flat or falling bin makes
    /// the ECU interpolate backwards. The grid warns rather than refuses.
    pub fn axis_monotonic(&self, axis: AxisKind) -> bool {
        use std::cmp::Ordering;

        let len = self.axis_len(axis);
        if len < 2 {
            return true;
        }
        let mut previous = self.axis_value(axis, 0);
        for index in 1..len {
            let value = self.axis_value(axis, index);
            if !matches!(value.partial_cmp(&previous), Some(Ordering::Greater)) {
                return false;
            }
            previous = value;
        }
        true
    }

    /// Starts editing one header bin.
    pub fn begin_axis_edit(&mut self, axis: AxisKind, index: usize) -> bool {
        if !self.is_axis_writable(axis, index) {
            return false;
        }
        self.editing = None;
        let digits = self.table.as_ref().map_or(0, |table| match axis {
            AxisKind::X => table.x_axis.digits,
            AxisKind::Y => table.y_axis.digits,
        });
        self.axis_editing = Some(AxisEditing {
            target: AxisTarget { axis, index },
            text: format!("{:.*}", usize::from(digits), self.axis_value(axis, index)),
            invalid: false,
        });
        self.action = None;
        true
    }

    /// Replaces part (or all) of the text being typed, for IME and platform
    /// input. Characters that cannot start an edit are dropped.
    ///
    /// Ranges are UTF-16 units; the edit language is ASCII, so a unit is a
    /// byte and the clamp cannot split a character.
    pub fn replace_edit_text(&mut self, range: Option<Range<usize>>, text: &str) -> bool {
        let filtered: String = text
            .chars()
            .filter(|character| is_edit_character(*character))
            .collect();
        if let Some(editing) = self.editing.as_mut() {
            editing.text = replace_range(&editing.text, range, &filtered);
            editing.invalid = false;
            self.action = None;
            return true;
        }
        if let Some(editing) = self.axis_editing.as_mut() {
            editing.text = replace_range(&editing.text, range, &filtered);
            editing.invalid = false;
            self.action = None;
            return true;
        }
        false
    }

    /// The text being typed, cell or header bin.
    pub fn edit_text(&self) -> Option<&str> {
        self.editing
            .as_ref()
            .map(|editing| editing.text.as_str())
            .or_else(|| {
                self.axis_editing
                    .as_ref()
                    .map(|editing| editing.text.as_str())
            })
    }

    /// Removes the last typed character while editing a header bin.
    pub fn pop_axis_char(&mut self) {
        if let Some(editing) = self.axis_editing.as_mut() {
            editing.text.pop();
            editing.invalid = false;
            self.action = None;
        }
    }

    /// Marks the header edit as unparseable, without discarding its text.
    pub fn mark_axis_invalid(&mut self) {
        if let Some(editing) = self.axis_editing.as_mut() {
            editing.invalid = true;
        }
    }

    /// Leaves header edit mode without storing anything.
    pub fn cancel_axis_edit(&mut self) {
        self.axis_editing = None;
        self.action = None;
    }

    /// Parses and stages the header edit as one bin write.
    pub fn commit_axis_edit(&mut self) -> Result<Vec<AxisEdit>, EditError> {
        let Some(editing) = self.axis_editing.as_ref() else {
            return Ok(Vec::new());
        };
        let Some(operation) = parse_edit(&editing.text)? else {
            self.axis_editing = None;
            return Ok(Vec::new());
        };
        if !self.is_writable() {
            return Err(EditError::ReadOnly);
        }
        let (operation, amount) = operation;
        // A header bin is one value: relative prefixes nudge it from where it
        // is, a bare number replaces it.
        let current = self.axis_value(editing.target.axis, editing.target.index);
        let value = match operation {
            EditOp::Set => amount,
            EditOp::Add => current + amount,
            EditOp::Multiply => current * amount,
            EditOp::Divide => current / amount,
            EditOp::Percent => current * (1.0 + amount / 100.0),
        };
        if !value.is_finite() {
            return Err(EditError::NotANumber);
        }
        let axis = editing.target.axis;
        let index = editing.target.index;
        let key = (matches!(axis, AxisKind::X), index);
        self.record(
            HistoryStep::Axis {
                axis,
                edits: vec![AxisEdit {
                    index,
                    value: current,
                }],
            },
            HistoryStep::Axis {
                axis,
                edits: vec![AxisEdit { index, value }],
            },
        );
        self.pending_axis.insert(key, value);
        self.axis_editing = None;
        self.action = None;
        Ok(vec![AxisEdit { index, value }])
    }

    /// The data row drawn at `display_row`.
    pub fn data_row(&self, display_row: usize) -> usize {
        let rows = self.rows();
        if rows == 0 {
            return 0;
        }
        if self.flip_y {
            display_row
        } else {
            rows.saturating_sub(1).saturating_sub(display_row)
        }
    }

    /// The display row a data row is drawn at.
    pub fn display_row(&self, data_row: usize) -> usize {
        let rows = self.rows();
        if rows == 0 {
            return 0;
        }
        if self.flip_y {
            data_row
        } else {
            rows.saturating_sub(1).saturating_sub(data_row)
        }
    }

    /// Replaces the selection with one cell.
    pub fn select(&mut self, cell: CellPos) {
        self.selection = Selection::single(cell);
        self.action = None;
    }

    /// Starts a new drag selection at `cell`.
    pub fn begin_drag(&mut self, cell: CellPos) {
        self.editing = None;
        self.axis_editing = None;
        self.selection = Selection::single(cell);
        self.action = None;
    }

    /// Moves the moving corner of the selection to `cell`.
    pub fn extend_to(&mut self, cell: CellPos) {
        self.selection.cursor = cell;
        self.action = None;
    }

    /// Collapses a range selection to its cursor cell.
    pub fn collapse_selection(&mut self) {
        self.selection = Selection::single(self.selection.cursor);
        self.action = None;
    }

    /// Moves the cursor in **display** coordinates, extending the selection when
    /// `extend` is set.
    pub fn move_cursor(&mut self, display_row_delta: isize, column_delta: isize, extend: bool) {
        let rows = self.rows();
        let columns = self.columns();
        if rows == 0 || columns == 0 {
            return;
        }

        let current_display = self.display_row(self.selection.cursor.row);
        let next_display = clamp_signed(current_display, display_row_delta, rows - 1);
        let next_column = clamp_signed(self.selection.cursor.column, column_delta, columns - 1);
        let cell = CellPos {
            row: self.data_row(next_display),
            column: next_column,
        };

        self.action = None;
        if extend {
            self.selection.cursor = cell;
        } else {
            self.selection = Selection::single(cell);
        }
    }

    /// Moves to the first or last displayed row.
    pub fn move_to_edge(&mut self, first: bool, extend: bool) {
        let display = if first {
            0
        } else {
            self.rows().saturating_sub(1)
        };
        let cell = CellPos {
            row: self.data_row(display),
            column: self.selection.cursor.column,
        };
        self.action = None;
        if extend {
            self.selection.cursor = cell;
        } else {
            self.selection = Selection::single(cell);
        }
    }

    /// Moves one page of displayed rows.
    pub fn move_page(&mut self, forward: bool, extend: bool) {
        let delta = PAGE_ROWS as isize;
        self.move_cursor(if forward { delta } else { -delta }, 0, extend);
    }

    /// Moves one displayed cell forward or backward, as Tab does.
    pub fn tab(&mut self, forward: bool) {
        let rows = self.rows();
        let columns = self.columns();
        if rows == 0 || columns == 0 {
            return;
        }
        let display = self.display_row(self.selection.cursor.row);
        let index = display * columns + self.selection.cursor.column;
        let next = if forward {
            (index + 1).min(rows * columns - 1)
        } else {
            index.saturating_sub(1)
        };
        let cell = CellPos {
            row: self.data_row(next / columns),
            column: next % columns,
        };
        self.action = None;
        self.selection = Selection::single(cell);
    }

    /// Selects every cell.
    pub fn select_all(&mut self) {
        let rows = self.rows();
        let columns = self.columns();
        if rows == 0 || columns == 0 {
            return;
        }
        self.selection = Selection {
            anchor: CellPos { row: 0, column: 0 },
            cursor: CellPos {
                row: rows - 1,
                column: columns - 1,
            },
        };
        self.action = None;
    }

    /// Starts editing the cursor cell, seeded with `seed` or its current value.
    pub fn begin_edit(&mut self, seed: Option<char>) -> bool {
        if !self.is_writable() {
            return false;
        }
        let cell = self.selection.cursor;
        if self
            .table
            .as_ref()
            .and_then(|table| table.cell(cell.row, cell.column))
            .is_none()
        {
            return false;
        }
        let text = seed.map_or_else(
            || {
                self.display(cell)
                    .map_or_else(String::new, |text| text.to_string())
            },
            String::from,
        );
        self.editing = Some(Editing {
            cell,
            text,
            invalid: false,
        });
        self.axis_editing = None;
        self.action = None;
        true
    }

    /// Appends a typed character while editing.
    pub fn push_char(&mut self, character: char) {
        if let Some(editing) = self.editing.as_mut()
            && is_edit_character(character)
        {
            editing.text.push(character);
            editing.invalid = false;
            self.action = None;
        }
    }

    /// Removes the last typed character while editing.
    pub fn pop_char(&mut self) {
        if let Some(editing) = self.editing.as_mut() {
            editing.text.pop();
            editing.invalid = false;
            self.action = None;
        }
    }

    /// Marks the in-progress edit as unparseable, without discarding its text.
    pub fn mark_edit_invalid(&mut self) {
        if let Some(editing) = self.editing.as_mut() {
            editing.invalid = true;
        }
    }

    /// Leaves edit mode without storing anything.
    pub fn cancel_edit(&mut self) {
        self.editing = None;
        self.action = None;
    }

    /// Parses and applies the edit to every selected cell.
    ///
    /// `+`/`-` apply a relative change, `*` multiplies and `/` divides; a bare
    /// number replaces the value. `=` forces an absolute value, which is how a
    /// negative number is typed without meaning "minus this much".
    pub fn commit_edit(&mut self) -> Result<Vec<CellEdit>, EditError> {
        let Some(editing) = self.editing.as_ref() else {
            return Ok(Vec::new());
        };
        // Parsing happens before edit mode ends, so a typo leaves the text on
        // screen for correction instead of discarding it.
        let Some(operation) = parse_edit(&editing.text)? else {
            self.editing = None;
            return Ok(Vec::new());
        };
        if !self.is_writable() {
            return Err(EditError::ReadOnly);
        }
        let (operation, amount) = operation;
        let edits = self.edits_for_selection(operation, amount)?;
        self.editing = None;
        self.record_cells(&edits);
        self.apply_pending(&edits);
        Ok(edits)
    }

    /// Applies a bulk operation to the selection.
    ///
    /// Every operation reads the values as they are before the command and
    /// writes the whole result at once, so smoothing never sees its own
    /// intermediate values. Values are rounded to the table's digits.
    pub fn bulk_edit(&mut self, op: BulkOp) -> Result<Vec<CellEdit>, EditError> {
        if self.table.is_none() {
            return Err(EditError::NoTable);
        }
        if !self.is_writable() {
            return Err(EditError::ReadOnly);
        }
        if !self.bulk_available(op) {
            self.action = Some(GridAction::TooSmall);
            return Err(EditError::SelectionTooSmall);
        }

        let (top, left, bottom, right) = self.selection.bounds();
        let digits = self
            .table
            .as_ref()
            .map_or(0, |table| usize::from(table.digits));
        let mut edits = Vec::new();

        match op {
            BulkOp::InterpolateRows => {
                let span = (right - left) as f64;
                for row in top..=bottom {
                    let first = self.value(CellPos { row, column: left });
                    let last = self.value(CellPos { row, column: right });
                    for column in left + 1..right {
                        let fraction = (column - left) as f64 / span;
                        let value = first + (last - first) * fraction;
                        edits.push(CellEdit {
                            row,
                            column,
                            value: round_to_digits(value, digits),
                        });
                    }
                }
            }
            BulkOp::InterpolateColumns => {
                let span = (bottom - top) as f64;
                for column in left..=right {
                    let first = self.value(CellPos { row: top, column });
                    let last = self.value(CellPos {
                        row: bottom,
                        column,
                    });
                    for row in top + 1..bottom {
                        let fraction = (row - top) as f64 / span;
                        let value = first + (last - first) * fraction;
                        edits.push(CellEdit {
                            row,
                            column,
                            value: round_to_digits(value, digits),
                        });
                    }
                }
            }
            BulkOp::Smooth => {
                for row in top..=bottom {
                    for column in left..=right {
                        let mut total = self.value(CellPos { row, column }) * 2.0;
                        let mut count = 2.0;
                        for (row_step, column_step) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)]
                        {
                            let neighbour_row = row as isize + row_step;
                            let neighbour_column = column as isize + column_step;
                            if neighbour_row < top as isize
                                || neighbour_row > bottom as isize
                                || neighbour_column < left as isize
                                || neighbour_column > right as isize
                            {
                                continue;
                            }
                            total += self.value(CellPos {
                                row: neighbour_row as usize,
                                column: neighbour_column as usize,
                            });
                            count += 1.0;
                        }
                        edits.push(CellEdit {
                            row,
                            column,
                            value: round_to_digits(total / count, digits),
                        });
                    }
                }
            }
            BulkOp::Equalize => {
                let mut total = 0.0;
                for row in top..=bottom {
                    for column in left..=right {
                        total += self.value(CellPos { row, column });
                    }
                }
                let count = ((bottom - top + 1) * (right - left + 1)) as f64;
                let mean = round_to_digits(total / count, digits);
                for row in top..=bottom {
                    for column in left..=right {
                        edits.push(CellEdit {
                            row,
                            column,
                            value: mean,
                        });
                    }
                }
            }
            BulkOp::FillDown => {
                let mut displayed: Vec<usize> = (top..=bottom).collect();
                displayed.sort_by_key(|row| self.display_row(*row));
                let source = displayed[0];
                for row in displayed.into_iter().skip(1) {
                    for column in left..=right {
                        edits.push(CellEdit {
                            row,
                            column,
                            value: self.value(CellPos {
                                row: source,
                                column,
                            }),
                        });
                    }
                }
            }
            BulkOp::FillRight => {
                for row in top..=bottom {
                    let source = self.value(CellPos { row, column: left });
                    for column in left + 1..=right {
                        edits.push(CellEdit {
                            row,
                            column,
                            value: source,
                        });
                    }
                }
            }
        }

        if !edits.is_empty() {
            self.record_cells(&edits);
            self.apply_pending(&edits);
        }
        self.action = Some(match op {
            BulkOp::InterpolateRows | BulkOp::InterpolateColumns => {
                GridAction::Interpolated { cells: edits.len() }
            }
            BulkOp::Smooth => GridAction::Smoothed { cells: edits.len() },
            BulkOp::Equalize => GridAction::Equalized { cells: edits.len() },
            BulkOp::FillDown | BulkOp::FillRight => GridAction::Filled { cells: edits.len() },
        });
        Ok(edits)
    }

    /// Whether the selection has the shape a bulk operation needs.
    pub fn bulk_available(&self, op: BulkOp) -> bool {
        if self.table.is_none() || !self.is_writable() {
            return false;
        }
        let (top, left, bottom, right) = self.selection.bounds();
        let rows = bottom - top + 1;
        let columns = right - left + 1;
        match op {
            BulkOp::InterpolateRows => columns >= 3,
            BulkOp::InterpolateColumns => rows >= 3,
            BulkOp::Smooth => rows * columns >= 3,
            BulkOp::Equalize => rows * columns >= 1,
            BulkOp::FillDown => rows >= 2,
            BulkOp::FillRight => columns >= 2,
        }
    }

    /// Whether an axis has enough bins to respace.
    pub fn can_rebin(&self, axis: AxisKind) -> bool {
        self.table.is_some() && self.is_writable() && self.axis_len(axis) >= 3
    }

    /// Evenly spaces the interior bins of one axis between its endpoints.
    ///
    /// The endpoints keep their values, so the table still covers the same
    /// range; only the breakpoints between them move. This is how a hand-edited
    /// axis is straightened without touching the cells.
    pub fn rebin_axis(&mut self, axis: AxisKind) -> Result<Vec<AxisEdit>, EditError> {
        let Some(table) = &self.table else {
            return Err(EditError::NoTable);
        };
        if !self.is_writable() {
            return Err(EditError::ReadOnly);
        }
        let len = self.axis_len(axis);
        if len < 3 {
            self.action = Some(GridAction::TooSmall);
            return Err(EditError::AxisTooSmall);
        }
        let digits = usize::from(match axis {
            AxisKind::X => table.x_axis.digits,
            AxisKind::Y => table.y_axis.digits,
        });
        let first = self.axis_value(axis, 0);
        let last = self.axis_value(axis, len - 1);

        let mut before = Vec::new();
        let mut after = Vec::new();
        for index in 1..len - 1 {
            let current = self.axis_value(axis, index);
            let value = round_to_digits(
                first + (last - first) * index as f64 / (len - 1) as f64,
                digits,
            );
            if value != current {
                before.push(AxisEdit {
                    index,
                    value: current,
                });
                after.push(AxisEdit { index, value });
            }
        }

        if !after.is_empty() {
            self.record(
                HistoryStep::Axis {
                    axis,
                    edits: before,
                },
                HistoryStep::Axis {
                    axis,
                    edits: after.clone(),
                },
            );
            let key = matches!(axis, AxisKind::X);
            for edit in &after {
                self.pending_axis.insert((key, edit.index), edit.value);
            }
        }
        self.action = Some(GridAction::RebinnedAxis { bins: after.len() });
        Ok(after)
    }

    /// The selection as tab-separated values.
    /// Copies the selection as tab-separated values, in displayed order.
    pub fn copy_text(&mut self) -> Option<String> {
        let table = self.table.as_ref()?;
        let digits = usize::from(table.digits);
        let (top, left, bottom, right) = self.selection.bounds();
        // Copy in the order the cells are drawn: top display row first.
        let rows: Vec<usize> = if self.flip_y {
            (top..=bottom).collect()
        } else {
            (top..=bottom).rev().collect()
        };
        let text = rows
            .into_iter()
            .map(|row| {
                (left..=right)
                    .map(|column| format!("{:.*}", digits, self.value(CellPos { row, column })))
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let cells = self.selection.cell_count();
        self.action = Some(GridAction::Copied { cells });
        Some(text)
    }

    /// Parses a pasted block at the selection's top-left corner.
    pub fn paste_edits(&mut self, text: &str) -> Result<Vec<CellEdit>, EditError> {
        if !self.is_writable() {
            self.action = Some(GridAction::PasteRejected);
            return Err(EditError::ReadOnly);
        }
        let Some(table) = &self.table else {
            self.action = Some(GridAction::PasteRejected);
            return Err(EditError::NoTable);
        };

        let rows: Vec<Vec<&str>> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.trim_end_matches('\r').split('\t').collect())
            .collect();
        let Some(width) = rows.first().map(Vec::len) else {
            self.action = Some(GridAction::PasteRejected);
            return Err(EditError::Empty);
        };
        if rows.iter().any(|row| row.len() != width) {
            self.action = Some(GridAction::PasteRejected);
            return Err(EditError::RaggedPaste);
        }

        let (top, left, bottom, _) = self.selection.bounds();
        // Paste starts at the visually top-left cell. Unless the axis is
        // flipped, that is the highest data row of the selection, and each
        // pasted row steps down through the data.
        let anchor = if self.flip_y { top } else { bottom };
        let fits = if self.flip_y {
            anchor + rows.len() <= table.rows
        } else {
            rows.len() <= anchor + 1
        };
        if !fits || left + width > table.cols {
            self.action = Some(GridAction::PasteRejected);
            return Err(EditError::OutOfBounds);
        }

        let mut edits = Vec::with_capacity(rows.len() * width);
        for (row_index, row) in rows.iter().enumerate() {
            let data_row = if self.flip_y {
                anchor + row_index
            } else {
                anchor - row_index
            };
            for (column_index, raw) in row.iter().enumerate() {
                let Ok(value) = raw.trim().parse::<f64>() else {
                    self.action = Some(GridAction::PasteRejected);
                    return Err(EditError::NotANumber);
                };
                if !value.is_finite() {
                    self.action = Some(GridAction::PasteRejected);
                    return Err(EditError::NotANumber);
                }
                edits.push(CellEdit {
                    row: data_row,
                    column: left + column_index,
                    value,
                });
            }
        }

        self.record_cells(&edits);
        self.apply_pending(&edits);
        let end_row = if self.flip_y {
            anchor + rows.len() - 1
        } else {
            anchor - (rows.len() - 1)
        };
        self.selection = Selection {
            anchor: CellPos {
                row: anchor,
                column: left,
            },
            cursor: CellPos {
                row: end_row,
                column: left + width - 1,
            },
        };
        self.action = Some(GridAction::Pasted { cells: edits.len() });
        Ok(edits)
    }

    fn edits_for_selection(
        &self,
        operation: EditOp,
        amount: f64,
    ) -> Result<Vec<CellEdit>, EditError> {
        if self.table.is_none() {
            return Err(EditError::NoTable);
        }
        let (top, left, bottom, right) = self.selection.bounds();
        let mut edits = Vec::with_capacity(self.selection.cell_count());
        for row in top..=bottom {
            for column in left..=right {
                let cell = CellPos { row, column };
                let current = self.value(cell);
                let value = match operation {
                    EditOp::Set => amount,
                    EditOp::Add => current + amount,
                    EditOp::Multiply => current * amount,
                    EditOp::Divide => current / amount,
                    EditOp::Percent => current * (1.0 + amount / 100.0),
                };
                if !value.is_finite() {
                    return Err(EditError::NotANumber);
                }
                edits.push(CellEdit { row, column, value });
            }
        }
        Ok(edits)
    }

    fn apply_pending(&mut self, edits: &[CellEdit]) {
        for edit in edits {
            self.pending.insert((edit.row, edit.column), edit.value);
            self.update_label(edit.row, edit.column, edit.value);
        }
    }

    /// Records a cell command from the values it is about to overwrite.
    fn record_cells(&mut self, after: &[CellEdit]) {
        let before = after
            .iter()
            .map(|edit| CellEdit {
                row: edit.row,
                column: edit.column,
                value: self.value(CellPos {
                    row: edit.row,
                    column: edit.column,
                }),
            })
            .collect();
        self.record(
            HistoryStep::Cells(before),
            HistoryStep::Cells(after.to_vec()),
        );
    }

    fn record(&mut self, before: HistoryStep, after: HistoryStep) {
        self.undo.push(HistoryEntry { before, after });
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Shows a history step optimistically and selects what it touched.
    fn apply_step(&mut self, step: &HistoryStep) {
        match step {
            HistoryStep::Cells(edits) => {
                self.apply_pending(edits);
                let Some(first) = edits.first() else {
                    return;
                };
                let mut top = first.row;
                let mut left = first.column;
                let mut bottom = first.row;
                let mut right = first.column;
                for edit in edits.iter().skip(1) {
                    top = top.min(edit.row);
                    left = left.min(edit.column);
                    bottom = bottom.max(edit.row);
                    right = right.max(edit.column);
                }
                self.selection = Selection {
                    anchor: CellPos {
                        row: top,
                        column: left,
                    },
                    cursor: CellPos {
                        row: bottom,
                        column: right,
                    },
                };
            }
            HistoryStep::Axis { axis, edits } => {
                let key = matches!(axis, AxisKind::X);
                for edit in edits {
                    self.pending_axis.insert((key, edit.index), edit.value);
                }
            }
        }
    }

    fn index_of(&self, cell: CellPos) -> Option<usize> {
        let table = self.table.as_ref()?;
        (cell.row < table.rows && cell.column < table.cols)
            .then(|| cell.row * table.cols + cell.column)
    }

    fn rebuild_labels(&mut self) {
        let Some(table) = &self.table else {
            self.display.clear();
            self.aria.clear();
            return;
        };

        let digits = usize::from(table.digits);
        let mut display = Vec::with_capacity(table.cells.len());
        let mut aria = Vec::with_capacity(table.cells.len());
        for (index, value) in table.cells.iter().enumerate() {
            let text = Arc::from(format!("{value:.digits$}"));
            let row = index / table.cols;
            let column = index % table.cols;
            let label = Arc::from(format!(
                "{text} {}, row {}, column {}",
                table.units,
                row + 1,
                column + 1
            ));
            display.push(text);
            aria.push(label);
        }
        self.display = display;
        self.aria = aria;

        let pending: Vec<(usize, usize, f64)> = self
            .pending
            .iter()
            .map(|(&(row, column), &value)| (row, column, value))
            .collect();
        for (row, column, value) in pending {
            self.update_label(row, column, value);
        }
    }

    fn update_label(&mut self, row: usize, column: usize, value: f64) {
        let Some(table) = &self.table else {
            return;
        };
        let Some(index) =
            (row < table.rows && column < table.cols).then(|| row * table.cols + column)
        else {
            return;
        };
        let text = Arc::from(format!(
            "{value:.digits$}",
            digits = usize::from(table.digits)
        ));
        let label = Arc::from(format!(
            "{text} {}, row {}, column {}",
            table.units,
            row + 1,
            column + 1
        ));
        if let Some(slot) = self.display.get_mut(index) {
            *slot = text;
        }
        if let Some(slot) = self.aria.get_mut(index) {
            *slot = label;
        }
    }

    fn clamp(&mut self) {
        let rows = self.rows();
        let columns = self.columns();
        if rows == 0 || columns == 0 {
            self.selection = Selection::single(CellPos { row: 0, column: 0 });
            self.editing = None;
            return;
        }
        let clamp_cell = |cell: CellPos| CellPos {
            row: cell.row.min(rows - 1),
            column: cell.column.min(columns - 1),
        };
        self.selection = Selection {
            anchor: clamp_cell(self.selection.anchor),
            cursor: clamp_cell(self.selection.cursor),
        };
        if self
            .editing
            .as_ref()
            .is_some_and(|editing| editing.cell.row >= rows || editing.cell.column >= columns)
        {
            self.editing = None;
        }
        if self.axis_editing.as_ref().is_some_and(|editing| {
            let len = match editing.target.axis {
                AxisKind::X => columns,
                AxisKind::Y => rows,
            };
            editing.target.index >= len
        }) {
            self.axis_editing = None;
        }
    }
}

/// Parses one edit expression into its operation and number.
///
/// Returns `Ok(None)` for an empty text, which means "nothing to do".
pub fn parse_edit(text: &str) -> Result<Option<(EditOp, f64)>, EditError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }

    // A trailing `%` makes a relative change a percentage of the current
    // value: `+5%` raises each cell by five percent. Only `+` and `-` take it,
    // because "set to 5% of itself" and "multiply by 5%" would be readings of
    // the same text.
    let percent = text.ends_with('%');
    let body = if percent {
        text[..text.len() - 1].trim_end()
    } else {
        text
    };
    if body.is_empty() {
        return Err(EditError::NotANumber);
    }

    let (mut operation, rest) = match body.as_bytes()[0] {
        b'+' => (EditOp::Add, &body[1..]),
        b'-' => (EditOp::Add, &body[1..]),
        b'*' => (EditOp::Multiply, &body[1..]),
        b'/' => (EditOp::Divide, &body[1..]),
        b'=' => (EditOp::Set, &body[1..]),
        _ => (EditOp::Set, body),
    };
    if percent {
        if operation != EditOp::Add {
            return Err(EditError::UnsupportedPercent);
        }
        operation = EditOp::Percent;
    }

    // A bare `+` or `-` nudges by one step: the tuner pressed the key and then
    // Enter, rather than typing a relative amount like `+2`.
    if !percent && body == "+" {
        return Ok(Some((EditOp::Add, 1.0)));
    }
    if !percent && body == "-" {
        return Ok(Some((EditOp::Add, -1.0)));
    }

    let number = if body.starts_with('-') {
        format!("-{rest}")
    } else {
        rest.to_owned()
    };
    let Ok(value) = number.trim().parse::<f64>() else {
        return Err(EditError::NotANumber);
    };
    if !value.is_finite() {
        return Err(EditError::NotANumber);
    }
    if operation == EditOp::Divide && value == 0.0 {
        return Err(EditError::DivideByZero);
    }

    Ok(Some((operation, value)))
}

/// Replaces a UTF-16 range in an ASCII edit string.
fn replace_range(current: &str, range: Option<Range<usize>>, text: &str) -> String {
    let mut next = current.to_owned();
    match range {
        Some(range) => {
            let start = range.start.min(next.len());
            let end = range.end.min(next.len()).max(start);
            next.replace_range(start..end, text);
        }
        None => next.push_str(text),
    }
    next
}

/// Rounds to the digits the table shows, so a fraction does not hide in a
/// cell the tuner only sees as a whole number.
fn round_to_digits(value: f64, digits: usize) -> f64 {
    let factor = 10f64.powi(digits as i32);
    (value * factor).round() / factor
}

fn is_edit_character(character: char) -> bool {
    character.is_ascii_digit()
        || matches!(
            character,
            '.' | '-' | '+' | '*' | '/' | '=' | '%' | 'e' | 'E'
        )
}

fn clamp_signed(current: usize, delta: isize, last: usize) -> usize {
    let next = current as isize + delta;
    next.clamp(0, last as isize) as usize
}

/// The bin closest to a live value, ties going to the lower index. A follower
/// outside the table still lands on the nearest edge bin.
fn nearest_bin(bins: &[f64], value: f64) -> Option<usize> {
    bins.iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (**a - value)
                .abs()
                .partial_cmp(&(**b - value).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: usize, columns: usize) -> TableSummary {
        let cells = (0..rows * columns).map(|value| value as f64).collect();
        TableSummary {
            id: "veTable".to_owned(),
            name: "veTable".to_owned(),
            title: "VE Table".to_owned(),
            cols: columns,
            rows,
            digits: 0,
            units: "%".to_owned(),
            cells,
            states: vec![CellState::Unknown; rows * columns],
            changed: vec![false; rows * columns],
            x_axis: oxtuner_core::AxisSummary {
                name: "rpmBins".to_owned(),
                label: "RPM".to_owned(),
                units: "rpm".to_owned(),
                channel: Some("rpm".to_owned()),
                digits: 0,
                values: (0..columns).map(|value| value as f64 * 500.0).collect(),
            },
            y_axis: oxtuner_core::AxisSummary {
                name: "mapBins".to_owned(),
                label: "MAP".to_owned(),
                units: "kPa".to_owned(),
                channel: Some("map".to_owned()),
                digits: 0,
                values: (0..rows).map(|value| value as f64 * 10.0).collect(),
            },
            minimum: 0.0,
            maximum: (rows * columns) as f64,
            writable: true,
        }
    }

    #[test]
    fn selection_bounds_are_normalized_and_counted() {
        let selection = Selection {
            anchor: CellPos { row: 3, column: 2 },
            cursor: CellPos { row: 1, column: 4 },
        };
        assert_eq!(selection.bounds(), (1, 2, 3, 4));
        assert_eq!(selection.cell_count(), 9);
        assert!(selection.contains(CellPos { row: 2, column: 3 }));
    }

    #[test]
    fn the_axes_name_their_live_channels() {
        let mut model = TableModel::new();
        assert_eq!(model.axis_channel(AxisKind::X), None);
        model.set_table(table(4, 2));
        assert_eq!(model.axis_channel(AxisKind::X), Some("rpm"));
        assert_eq!(model.axis_channel(AxisKind::Y), Some("map"));
    }

    #[test]
    fn a_new_table_starts_at_the_visible_top_left_on_both_axes() {
        let mut model = TableModel::new();
        model.set_table(table(4, 2));

        assert_eq!(model.selection().cursor, CellPos { row: 3, column: 0 });

        model.set_flip_y(true);
        model.clear_table();
        model.set_table(table(4, 8));

        assert_eq!(model.selection().cursor, CellPos { row: 0, column: 0 });
    }

    #[test]
    fn a_live_point_lands_on_its_nearest_cell() {
        let mut model = TableModel::new();
        // Columns step 500 (0, 500, 1000, ...), rows step 10 (0, 10, 20, 30).
        model.set_table(table(4, 8));
        assert_eq!(
            model.realtime_cell(750.0, 22.0),
            Some(CellPos { row: 2, column: 1 }),
            "750 ties 500 and 1000, so it lands on the lower bin; 22 rounds to 20"
        );
        assert_eq!(
            model.realtime_cell(9_999.0, -5.0),
            Some(CellPos { row: 0, column: 7 }),
            "a point outside the table holds the nearest edge bins"
        );
        model.clear_table();
        assert_eq!(model.realtime_cell(750.0, 22.0), None);
    }

    #[test]
    fn the_default_axis_draws_the_highest_row_on_top() {
        let mut model = TableModel::new();
        model.set_table(table(4, 2));
        assert!(!model.flip_y(), "the chart orientation is the default");
        assert_eq!(model.display_row(0), 3);
        // Start from the lowest bin, which is drawn last.
        model.select(CellPos { row: 0, column: 0 });

        model.move_cursor(-1, 0, false);
        assert_eq!(
            model.selection().cursor,
            CellPos { row: 1, column: 0 },
            "moving up reaches the next higher data row"
        );

        model.move_to_edge(true, false);
        assert_eq!(
            model.selection().cursor.row,
            3,
            "Home is the top display row, the highest bin"
        );
        model.move_to_edge(false, false);
        assert_eq!(
            model.selection().cursor.row,
            0,
            "End is the bottom display row, the lowest bin"
        );
    }

    #[test]
    fn a_flipped_axis_puts_the_lowest_row_on_top() {
        let mut model = TableModel::new();
        model.set_flip_y(true);
        model.set_table(table(4, 2));

        assert_eq!(model.display_row(0), 0);
        model.move_cursor(1, 0, false);
        assert_eq!(model.selection().cursor.row, 1);
    }

    #[test]
    fn tab_moves_in_display_order_and_stops_at_the_edges() {
        let mut model = TableModel::new();
        model.set_flip_y(true);
        model.set_table(table(2, 2));
        model.tab(true);
        assert_eq!(model.selection().cursor, CellPos { row: 0, column: 1 });
        model.tab(true);
        assert_eq!(model.selection().cursor, CellPos { row: 1, column: 0 });
        model.tab(false);
        assert_eq!(model.selection().cursor, CellPos { row: 0, column: 1 });
    }

    #[test]
    fn relative_and_factor_edits_parse() {
        assert_eq!(parse_edit("5"), Ok(Some((EditOp::Set, 5.0))));
        assert_eq!(parse_edit("= -5"), Ok(Some((EditOp::Set, -5.0))));
        assert_eq!(parse_edit("+2"), Ok(Some((EditOp::Add, 2.0))));
        assert_eq!(parse_edit("-2"), Ok(Some((EditOp::Add, -2.0))));
        assert_eq!(parse_edit("*1.1"), Ok(Some((EditOp::Multiply, 1.1))));
        assert_eq!(parse_edit("/2"), Ok(Some((EditOp::Divide, 2.0))));
        assert_eq!(parse_edit(" "), Ok(None));
        assert_eq!(parse_edit("x"), Err(EditError::NotANumber));
        assert_eq!(parse_edit("/0"), Err(EditError::DivideByZero));
    }

    #[test]
    fn committing_applies_the_edit_to_the_whole_selection() {
        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        model.selection = Selection {
            anchor: CellPos { row: 0, column: 0 },
            cursor: CellPos { row: 1, column: 1 },
        };
        model.begin_edit(Some('5'));
        let edits = model.commit_edit().expect("a valid edit");
        assert_eq!(edits.len(), 4);
        assert_eq!(model.value(CellPos { row: 1, column: 1 }), 5.0);
    }

    #[test]
    fn a_negative_absolute_value_needs_the_equals_prefix() {
        let mut model = TableModel::new();
        model.set_table(table(1, 1));
        model.begin_edit(Some('='));
        model.push_char('-');
        model.push_char('7');
        model.commit_edit().expect("a valid edit");
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), -7.0);
    }

    #[test]
    fn copy_and_paste_round_trip_a_block() {
        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        model.set_flip_y(true);
        model.selection = Selection {
            anchor: CellPos { row: 0, column: 0 },
            cursor: CellPos { row: 1, column: 1 },
        };
        let text = model.copy_text().expect("a table");
        assert_eq!(text, "0\t1\n2\t3");

        let edits = model.paste_edits("9\t8\n7\t6").expect("the block fits");
        assert_eq!(edits.len(), 4);
        assert_eq!(model.value(CellPos { row: 1, column: 1 }), 6.0);
        assert_eq!(model.action(), Some(GridAction::Pasted { cells: 4 }));
    }

    #[test]
    fn copy_and_paste_follow_the_display_when_flipped() {
        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        assert!(!model.flip_y());
        model.select_all();
        assert_eq!(
            model.copy_text().expect("a table"),
            "2\t3\n0\t1",
            "the top copied row is the top displayed row"
        );

        model.select(CellPos { row: 1, column: 0 });
        let edits = model
            .paste_edits("9\t8\n7\t6")
            .expect("the block fits from the top displayed row");
        assert!(edits.contains(&CellEdit {
            row: 1,
            column: 0,
            value: 9.0,
        }));
        assert!(edits.contains(&CellEdit {
            row: 0,
            column: 0,
            value: 7.0,
        }));
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), 7.0);
    }

    #[test]
    fn a_paste_that_does_not_fit_is_rejected_whole() {
        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        model.select(CellPos { row: 1, column: 1 });
        assert_eq!(model.paste_edits("1\t2\n3\t4"), Err(EditError::OutOfBounds));
        assert_eq!(model.value(CellPos { row: 1, column: 1 }), 3.0);
    }

    #[test]
    fn a_read_only_table_refuses_edits() {
        let mut summary = table(1, 1);
        summary.writable = false;
        let mut model = TableModel::new();
        model.set_table(summary);
        assert!(!model.begin_edit(Some('1')));
        assert_eq!(model.paste_edits("1"), Err(EditError::ReadOnly));
    }

    #[test]
    fn replacing_the_table_resets_the_selection_but_an_echo_does_not() {
        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        model.select(CellPos { row: 1, column: 1 });

        let mut echo = table(2, 2);
        model.set_table(echo.clone());
        assert_eq!(model.selection().cursor, CellPos { row: 1, column: 1 });

        echo.id = "other".to_owned();
        model.set_table(echo);
        assert_eq!(
            model.selection().cursor,
            CellPos { row: 1, column: 0 },
            "a new table starts at its top displayed cell, not the old cursor"
        );
    }

    #[test]
    fn a_new_table_starts_at_the_visible_top_left() {
        let mut model = TableModel::new();
        model.set_table(table(4, 2));
        assert_eq!(
            model.selection().cursor,
            CellPos { row: 3, column: 0 },
            "the default orientation starts at the highest bin"
        );

        let mut flipped = TableModel::new();
        flipped.set_flip_y(true);
        flipped.set_table(table(4, 2));
        assert_eq!(
            flipped.selection().cursor,
            CellPos { row: 0, column: 0 },
            "a flipped axis starts at the lowest bin"
        );
    }

    #[test]
    fn an_echo_only_clears_the_values_it_confirms() {
        let mut model = TableModel::new();
        model.set_table(table(1, 1));
        model.begin_edit(Some('5'));
        model.commit_edit().expect("a valid edit");
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), 5.0);

        model.set_table(table(1, 1));
        assert_eq!(
            model.value(CellPos { row: 0, column: 0 }),
            5.0,
            "an older snapshot does not undo a newer optimistic edit"
        );

        let mut confirmed = table(1, 1);
        confirmed.cells[0] = 5.0;
        model.set_table(confirmed);
        assert!(model.pending.is_empty());
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), 5.0);
    }

    #[test]
    fn a_bare_plus_or_minus_nudges_by_one() {
        assert_eq!(parse_edit("+"), Ok(Some((EditOp::Add, 1.0))));
        assert_eq!(parse_edit("-"), Ok(Some((EditOp::Add, -1.0))));
        assert_eq!(parse_edit("+2"), Ok(Some((EditOp::Add, 2.0))));
    }

    #[test]
    fn a_header_bin_edit_stages_an_optimistic_value() {
        use oxtuner_core::AxisKind;

        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        assert!(model.begin_axis_edit(AxisKind::X, 1));
        // Clear the seeded text and type the new bin.
        while model
            .axis_editing()
            .is_some_and(|editing| !editing.text.is_empty())
        {
            model.pop_axis_char();
        }
        model.replace_edit_text(None, "2500");
        let edits = model.commit_axis_edit().expect("a valid bin");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].index, 1);
        assert_eq!(edits[0].value, 2500.0);
        assert_eq!(model.axis_value(AxisKind::X, 1), 2500.0);
    }

    #[test]
    fn a_falling_bin_reads_as_not_monotonic() {
        use oxtuner_core::AxisKind;

        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        assert!(model.axis_monotonic(AxisKind::X));
        // Bins are [0, 500]: raising the first past the second breaks the order.
        assert!(model.begin_axis_edit(AxisKind::X, 0));
        while model
            .axis_editing()
            .is_some_and(|editing| !editing.text.is_empty())
        {
            model.pop_axis_char();
        }
        model.replace_edit_text(None, "600");
        model.commit_axis_edit().expect("a valid bin");
        assert!(
            !model.axis_monotonic(AxisKind::X),
            "600 followed by 500 must read as out of order"
        );
    }

    #[test]
    fn a_read_only_table_refuses_header_edits() {
        use oxtuner_core::AxisKind;

        let mut summary = table(1, 1);
        summary.writable = false;
        let mut model = TableModel::new();
        model.set_table(summary);
        assert!(!model.begin_axis_edit(AxisKind::X, 0));
    }

    fn table_with(rows: usize, columns: usize, cells: &[f64], digits: u8) -> TableSummary {
        let mut summary = table(rows, columns);
        summary.cells = cells.to_vec();
        summary.digits = digits;
        summary
    }

    #[test]
    fn a_percent_edit_changes_each_cell_relative_to_itself() {
        assert_eq!(parse_edit("+5%"), Ok(Some((EditOp::Percent, 5.0))));
        assert_eq!(parse_edit("-10%"), Ok(Some((EditOp::Percent, -10.0))));
        assert_eq!(parse_edit("+5 %"), Ok(Some((EditOp::Percent, 5.0))));
        assert_eq!(parse_edit("5%"), Err(EditError::UnsupportedPercent));
        assert_eq!(parse_edit("*5%"), Err(EditError::UnsupportedPercent));
        assert_eq!(parse_edit("/5%"), Err(EditError::UnsupportedPercent));
        assert_eq!(parse_edit("=5%"), Err(EditError::UnsupportedPercent));
        assert_eq!(parse_edit("%"), Err(EditError::NotANumber));

        let mut model = TableModel::new();
        model.set_table(table_with(1, 2, &[10.0, 20.0], 2));
        model.select_all();
        model.begin_edit(Some('+'));
        model.push_char('5');
        model.push_char('%');
        model.commit_edit().expect("a valid percent edit");
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), 10.5);
        assert_eq!(model.value(CellPos { row: 0, column: 1 }), 21.0);
    }

    #[test]
    fn interpolation_draws_a_line_between_the_selected_ends() {
        let mut model = TableModel::new();
        model.set_table(table_with(1, 3, &[0.0, 42.0, 100.0], 0));
        model.select_all();
        let edits = model
            .bulk_edit(BulkOp::InterpolateRows)
            .expect("three columns");
        assert_eq!(
            edits,
            vec![CellEdit {
                row: 0,
                column: 1,
                value: 50.0,
            }]
        );
        assert_eq!(model.value(CellPos { row: 0, column: 1 }), 50.0);
        assert_eq!(model.action(), Some(GridAction::Interpolated { cells: 1 }));

        let mut column = TableModel::new();
        column.set_table(table_with(3, 1, &[0.0, 42.0, 100.0], 0));
        column.select_all();
        let edits = column
            .bulk_edit(BulkOp::InterpolateColumns)
            .expect("three rows");
        assert_eq!(
            edits,
            vec![CellEdit {
                row: 1,
                column: 0,
                value: 50.0,
            }]
        );
        assert_eq!(column.value(CellPos { row: 1, column: 0 }), 50.0);
    }

    #[test]
    fn smoothing_averages_with_orthogonal_neighbours_once() {
        let mut cells = vec![0.0; 9];
        cells[4] = 9.0;
        let mut model = TableModel::new();
        model.set_table(table_with(3, 3, &cells, 0));
        model.select_all();
        let edits = model.bulk_edit(BulkOp::Smooth).expect("nine cells");
        assert_eq!(edits.len(), 9);
        assert_eq!(
            model.value(CellPos { row: 1, column: 1 }),
            3.0,
            "the centre averages itself twice with four zero neighbours"
        );
        assert_eq!(
            model.value(CellPos { row: 0, column: 1 }),
            2.0,
            "an edge uses the original centre, not its smoothed value"
        );
        assert_eq!(model.action(), Some(GridAction::Smoothed { cells: 9 }));
    }

    #[test]
    fn equalizing_sets_every_selected_cell_to_the_mean() {
        let mut model = TableModel::new();
        model.set_table(table_with(2, 2, &[0.0, 2.0, 4.0, 6.0], 0));
        model.select_all();
        let edits = model.bulk_edit(BulkOp::Equalize).expect("four cells");
        assert_eq!(edits.len(), 4);
        for edit in &edits {
            assert_eq!(edit.value, 3.0);
        }
        assert_eq!(model.action(), Some(GridAction::Equalized { cells: 4 }));
    }

    #[test]
    fn filling_down_uses_the_top_displayed_row() {
        // Default orientation: the highest data row is drawn on top, so row 1
        // is the source and row 0 the copy.
        let mut model = TableModel::new();
        model.set_table(table_with(2, 2, &[1.0, 2.0, 3.0, 4.0], 0));
        model.select_all();
        let edits = model.bulk_edit(BulkOp::FillDown).expect("two rows");
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|edit| edit.row == 0));
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), 3.0);
        assert_eq!(model.value(CellPos { row: 0, column: 1 }), 4.0);
        assert_eq!(
            model.value(CellPos { row: 1, column: 0 }),
            3.0,
            "the source row keeps its own values"
        );

        // Flipped: the lowest data row is on top and becomes the source.
        let mut flipped = TableModel::new();
        flipped.set_flip_y(true);
        flipped.set_table(table_with(2, 2, &[1.0, 2.0, 3.0, 4.0], 0));
        flipped.select_all();
        flipped.bulk_edit(BulkOp::FillDown).expect("two rows");
        assert_eq!(flipped.value(CellPos { row: 0, column: 0 }), 1.0);
        assert_eq!(flipped.value(CellPos { row: 1, column: 0 }), 1.0);
        assert_eq!(flipped.value(CellPos { row: 1, column: 1 }), 2.0);
    }

    #[test]
    fn filling_right_copies_the_leftmost_selected_column() {
        let mut model = TableModel::new();
        model.set_table(table_with(2, 2, &[1.0, 2.0, 3.0, 4.0], 0));
        model.select_all();
        let edits = model.bulk_edit(BulkOp::FillRight).expect("two columns");
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(|edit| edit.column == 1));
        assert_eq!(model.value(CellPos { row: 0, column: 1 }), 1.0);
        assert_eq!(model.value(CellPos { row: 1, column: 1 }), 3.0);
    }

    #[test]
    fn a_bulk_operation_needs_the_shape_it_reads() {
        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        model.select_all();
        assert!(!model.bulk_available(BulkOp::InterpolateRows));
        assert!(!model.bulk_available(BulkOp::InterpolateColumns));
        assert!(model.bulk_available(BulkOp::Smooth));
        assert!(model.bulk_available(BulkOp::Equalize));
        assert!(model.bulk_available(BulkOp::FillDown));
        assert!(model.bulk_available(BulkOp::FillRight));

        model.select(CellPos { row: 0, column: 0 });
        assert!(!model.bulk_available(BulkOp::Smooth));
        assert!(!model.bulk_available(BulkOp::FillDown));
        assert!(!model.bulk_available(BulkOp::FillRight));
        assert_eq!(
            model.bulk_edit(BulkOp::Smooth),
            Err(EditError::SelectionTooSmall)
        );
        assert_eq!(model.action(), Some(GridAction::TooSmall));
    }

    #[test]
    fn a_committed_edit_can_be_undone_and_redone() {
        let mut model = TableModel::new();
        model.set_table(table_with(1, 1, &[10.0], 0));
        model.select(CellPos { row: 0, column: 0 });
        model.begin_edit(Some('5'));
        model.commit_edit().expect("a valid edit");
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), 5.0);
        assert!(model.can_undo());
        assert!(!model.can_redo());

        let step = model.undo().expect("a command to undo");
        match step {
            HistoryStep::Cells(edits) => assert_eq!(
                edits,
                vec![CellEdit {
                    row: 0,
                    column: 0,
                    value: 10.0,
                }]
            ),
            HistoryStep::Axis { .. } => panic!("a cell edit must undo as cells"),
        }
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), 10.0);
        assert_eq!(model.action(), Some(GridAction::Undone { cells: 1 }));
        assert!(!model.can_undo());
        assert!(model.can_redo());

        let step = model.redo().expect("a command to redo");
        match step {
            HistoryStep::Cells(edits) => assert_eq!(
                edits,
                vec![CellEdit {
                    row: 0,
                    column: 0,
                    value: 5.0,
                }]
            ),
            HistoryStep::Axis { .. } => panic!("a cell edit must redo as cells"),
        }
        assert_eq!(model.value(CellPos { row: 0, column: 0 }), 5.0);
        assert_eq!(model.action(), Some(GridAction::Redone { cells: 1 }));
    }

    #[test]
    fn a_header_bin_edit_is_undoable_too() {
        use oxtuner_core::AxisKind;

        let mut model = TableModel::new();
        model.set_table(table(1, 2));
        assert!(model.begin_axis_edit(AxisKind::X, 1));
        while model
            .axis_editing()
            .is_some_and(|editing| !editing.text.is_empty())
        {
            model.pop_axis_char();
        }
        model.replace_edit_text(None, "2500");
        model.commit_axis_edit().expect("a valid bin");
        assert_eq!(model.axis_value(AxisKind::X, 1), 2500.0);

        let step = model.undo().expect("a command to undo");
        match step {
            HistoryStep::Axis { axis, edits } => {
                assert_eq!(axis, AxisKind::X);
                assert_eq!(
                    edits,
                    vec![oxtuner_core::AxisEdit {
                        index: 1,
                        value: 500.0,
                    }]
                );
            }
            HistoryStep::Cells(_) => panic!("a bin edit must undo as an axis step"),
        }
        assert_eq!(model.axis_value(AxisKind::X, 1), 500.0);
        assert_eq!(model.action(), Some(GridAction::UndoneAxis { bins: 1 }));

        model.redo().expect("a command to redo");
        assert_eq!(model.axis_value(AxisKind::X, 1), 2500.0);
        assert_eq!(model.action(), Some(GridAction::RedoneAxis { bins: 1 }));
    }

    #[test]
    fn a_rejected_command_leaves_the_history() {
        let mut model = TableModel::new();
        model.set_table(table_with(1, 1, &[10.0], 0));
        model.begin_edit(Some('5'));
        model.commit_edit().expect("a valid edit");
        assert!(model.can_undo());

        model.reject_pending();
        assert_eq!(
            model.value(CellPos { row: 0, column: 0 }),
            10.0,
            "the optimistic value is dropped"
        );
        assert!(
            !model.can_undo(),
            "a write the core refused must not be undoable"
        );
        assert!(!model.can_redo());

        // The same failure may be observed again. With an older, confirmed
        // step in the stack, a second report must not eat it.
        model.set_table(table_with(1, 1, &[10.0], 0));
        model.begin_edit(Some('3'));
        model.commit_edit().expect("a valid edit");
        model.set_table(table_with(1, 1, &[3.0], 0));
        model.begin_edit(Some('7'));
        model.commit_edit().expect("a valid edit");
        model.reject_pending();
        assert!(model.can_undo(), "the confirmed step is still undoable");
        model.reject_pending();
        assert!(
            model.can_undo(),
            "a repeated rejection has nothing left to drop"
        );
    }

    #[test]
    fn selecting_another_table_forgets_the_history() {
        let mut model = TableModel::new();
        model.set_table(table_with(1, 1, &[10.0], 0));
        model.begin_edit(Some('5'));
        model.commit_edit().expect("a valid edit");
        assert!(model.can_undo());

        let mut other = table_with(1, 1, &[10.0], 0);
        other.id = "other".to_owned();
        model.set_table(other);
        assert!(!model.can_undo());
        assert!(!model.can_redo());
    }

    #[test]
    fn a_bulk_command_undoes_as_one_step() {
        let mut model = TableModel::new();
        model.set_table(table_with(1, 3, &[0.0, 42.0, 100.0], 0));
        model.select_all();
        model
            .bulk_edit(BulkOp::InterpolateRows)
            .expect("three columns");
        assert_eq!(model.value(CellPos { row: 0, column: 1 }), 50.0);

        model.undo().expect("one step for the whole line");
        assert_eq!(model.value(CellPos { row: 0, column: 1 }), 42.0);
        assert!(!model.can_undo());
    }

    #[test]
    fn re_binning_spaces_an_axis_evenly_and_undoes() {
        use oxtuner_core::AxisKind;

        let mut summary = table(2, 4);
        summary.x_axis.values = vec![0.0, 100.0, 400.0, 1000.0];
        let mut model = TableModel::new();
        model.set_table(summary);
        assert!(model.can_rebin(AxisKind::X));

        let edits = model.rebin_axis(AxisKind::X).expect("four bins");
        assert_eq!(
            edits,
            vec![
                oxtuner_core::AxisEdit {
                    index: 1,
                    value: 333.0,
                },
                oxtuner_core::AxisEdit {
                    index: 2,
                    value: 667.0,
                },
            ]
        );
        assert_eq!(model.axis_value(AxisKind::X, 1), 333.0);
        assert_eq!(model.axis_value(AxisKind::X, 2), 667.0);
        assert_eq!(model.action(), Some(GridAction::RebinnedAxis { bins: 2 }));

        model.undo().expect("one step for the axis");
        assert_eq!(model.axis_value(AxisKind::X, 1), 100.0);
        assert_eq!(model.axis_value(AxisKind::X, 2), 400.0);
    }

    #[test]
    fn a_two_bin_axis_cannot_be_rebinned() {
        use oxtuner_core::AxisKind;

        let mut model = TableModel::new();
        model.set_table(table(2, 2));
        assert!(!model.can_rebin(AxisKind::X));
        assert_eq!(model.rebin_axis(AxisKind::X), Err(EditError::AxisTooSmall));
        assert_eq!(model.action(), Some(GridAction::TooSmall));
    }

    #[test]
    fn platform_text_replaces_the_typed_range() {
        let mut model = TableModel::new();
        model.set_table(table_with(1, 1, &[0.0], 0));
        model.begin_edit(Some('5'));
        assert!(model.replace_edit_text(None, "12"));
        assert_eq!(model.edit_text(), Some("512"));
        assert!(model.replace_edit_text(Some(1..3), "4"));
        assert_eq!(model.edit_text(), Some("54"));
        assert!(model.replace_edit_text(None, "a"));
        assert_eq!(
            model.edit_text(),
            Some("54"),
            "characters outside the edit language are dropped"
        );
        model.cancel_edit();
        assert_eq!(model.edit_text(), None);
    }
}
