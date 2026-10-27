//! The grid's state, free of GPUI types.
//!
//! Selection, edit parsing, copy/paste and the display flip are plain Rust so
//! they can be tested without a window (ADR-0013). The view in [`super::view`]
//! renders this state and turns its requested edits into events; the core stays
//! the only owner of tune values.
//!
//! Row coordinates are **definition rows**. When the Y axis is flipped the grid
//! draws them in reverse, and every cursor/pointer move is translated through
//! [`TableModel::data_row`] and [`TableModel::display_row`].

use std::collections::HashMap;
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
}

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
            // Start on the cell the user sees first: the top-left of the
            // displayed grid, which is the highest bin unless the axis is
            // flipped. Selecting data row 0 here would open the table scrolled
            // to its far end.
            let top = if self.flip_y {
                0
            } else {
                table.rows.saturating_sub(1)
            };
            self.selection = Selection::single(CellPos {
                row: top,
                column: 0,
            });
            self.editing = None;
            self.axis_editing = None;
            self.pending.clear();
            self.pending_axis.clear();
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
    pub fn reject_pending(&mut self) {
        self.pending.clear();
        self.pending_axis.clear();
        self.rebuild_labels();
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

    /// Appends a typed character while editing a header bin.
    pub fn push_axis_char(&mut self, character: char) {
        if let Some(editing) = self.axis_editing.as_mut()
            && is_edit_character(character)
        {
            editing.text.push(character);
            editing.invalid = false;
            self.action = None;
        }
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
        };
        if !value.is_finite() {
            return Err(EditError::NotANumber);
        }
        let key = (
            matches!(editing.target.axis, AxisKind::X),
            editing.target.index,
        );
        self.pending_axis.insert(key, value);
        let edit = AxisEdit {
            index: editing.target.index,
            value,
        };
        self.axis_editing = None;
        self.action = None;
        Ok(vec![edit])
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
        self.apply_pending(&edits);
        Ok(edits)
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

    let (operation, rest) = match text.as_bytes()[0] {
        b'+' => (EditOp::Add, &text[1..]),
        b'-' => (EditOp::Add, &text[1..]),
        b'*' => (EditOp::Multiply, &text[1..]),
        b'/' => (EditOp::Divide, &text[1..]),
        b'=' => (EditOp::Set, &text[1..]),
        _ => (EditOp::Set, text),
    };

    // A bare `+` or `-` nudges by one step: the tuner pressed the key and then
    // Enter, rather than typing a relative amount like `+2`.
    if text == "+" {
        return Ok(Some((EditOp::Add, 1.0)));
    }
    if text == "-" {
        return Ok(Some((EditOp::Add, -1.0)));
    }

    let number = if text.starts_with('-') {
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

fn is_edit_character(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, '.' | '-' | '+' | '*' | '/' | '=' | 'e' | 'E')
}

fn clamp_signed(current: usize, delta: isize, last: usize) -> usize {
    let next = current as isize + delta;
    next.clamp(0, last as isize) as usize
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
                digits: 0,
                values: (0..columns).map(|value| value as f64 * 500.0).collect(),
            },
            y_axis: oxtuner_core::AxisSummary {
                name: "mapBins".to_owned(),
                label: "MAP".to_owned(),
                units: "kPa".to_owned(),
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
        for character in "2500".chars() {
            model.push_axis_char(character);
        }
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
        for character in "600".chars() {
            model.push_axis_char(character);
        }
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
}
