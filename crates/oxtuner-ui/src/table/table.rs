//! The reusable tune table.
//!
//! One component serves every value table: the tune editor today, and any
//! future surface that shows a `TableView`. It owns only view state —
//! selection, cursor, the in-progress edit, the scroll position — and reads
//! values from [`TableModel`], a plain-Rust state that is tested without a
//! window (ADR-0013, ADR-0020).
//!
//! The table never writes tune values. A committed edit becomes a
//! [`TableEvent::EditRequested`], the owner sends it to the core, and the
//! echoed [`oxtuner_core::TableView`] clears the optimistic value. That
//! keeps the core the only owner of the tune while the cell still changes in
//! the same frame the key was pressed.
//!
//! Headers are frozen: the body scrolls under fixed header bands shifted by
//! the body's scroll offset. The column band follows the low end of the Y
//! axis: at the bottom by default and at the top when the axis is flipped.
//! Each axis names itself in a caption band beside its headers, and the
//! selection is painted over the cells as a spreadsheet range: one tint and
//! one outline, never a border that moves the values. A single selected cell
//! also tints the two bins it sits between, so the value can be read against
//! both axes; a range leaves the bins plain unless the owner asks for the
//! wider wash with [`Table::set_header_wash_for_ranges`].
//! There is no scrollbar; the body scrolls with the wheel, drag-select and
//! the keyboard.

#![expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::unreadable_literal,
    reason = "GPUI geometry uses f32 while the tune model stores f64 and element indexes"
)]

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, button::Button, button::ButtonVariants};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementId, ElementInputHandler,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, Role, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, UTF16Selection, Window,
    canvas, div, point, px, rgb,
};

use oxtuner_core::{CellEdit, TableView};

use super::cell::{CellEditing, TableCell};
use super::geometry::{
    AXIS_HEIGHT, AXIS_LABEL_GAP, AXIS_LABEL_HEIGHT, AXIS_LABEL_WIDTH, AXIS_WIDTH, CELL_GAP,
    CELL_HEIGHT, CELL_WIDTH, visible_range,
};
use super::headers::{BinHighlight, ColumnHeader, HeaderEditing, RowHeader};
use super::heatmap::{decoration, heat_color};

// use super::heatmap::contrast_text;
// Unused while cell text is fixed at #121314. Put the import back together
// with the call site in render_cell if the per-cell contrast picker is needed
// again.
use super::model::{BulkOp, CellPos, Editing, HistoryStep, TableModel};

/// What the table asks its owner to do.
#[expect(
    clippy::exhaustive_enums,
    reason = "the table's owner handles a closed set of interactions"
)]
#[derive(Clone, Debug, PartialEq)]
pub enum TableEvent {
    /// The user committed edits for `name` (the cell variable's name).
    EditRequested {
        /// Cell variable, the table's `zBins`.
        name: String,
        /// New values, in user units.
        edits: Vec<CellEdit>,
    },
    /// The user committed header bin edits for one axis.
    AxisEditRequested {
        /// Table id (or curve name), as shown.
        table: String,
        /// Which axis the bins belong to.
        axis: oxtuner_core::AxisKind,
        /// New bin values, in user units.
        edits: Vec<oxtuner_core::AxisEdit>,
    },
    /// The selection or edit state changed; the owner's status line should
    /// repaint.
    SelectionChanged,
}

/// A realtime operating point, for the overlay slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RealtimePoint {
    /// Definition row.
    pub row: usize,
    /// Column.
    pub column: usize,
    /// The live value, for a badge.
    pub value: f64,
}

/// The tune table.
///
/// Chrome follows the application theme through `cx.theme()`; only the heatmap
/// cells keep their own data colours (see [`super::heatmap`]).
pub struct Table {
    model: TableModel,
    focus: FocusHandle,
    body_scroll: ScrollHandle,
    dragging: bool,
    realtime: Option<RealtimePoint>,
    /// Whether a selection of several cells also tints the bins of the headers
    /// it spans. One cell always tints its two bins.
    header_wash_for_ranges: bool,
}

impl std::fmt::Debug for Table {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Table")
            .field("model", &self.model)
            .field("dragging", &self.dragging)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<TableEvent> for Table {}

impl Focusable for Table {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Text input for the in-cell editor.
///
/// The edit lives in a borderless kit [`Input`], so a cell edit behaves like
/// any text field: arrows move the caret, Home and End jump, text can be
/// selected, cut and pasted, and IME composition is the kit's problem. The
/// table owns only the contract around it — Enter commits, Tab commits and
/// moves, Escape cancels — and both Tab and Escape reach [`Table::handle_key`]
/// because a single-line input propagates them.
/// Text input for the in-cell editor.
///
/// The table draws its own editor instead of an `Input`, so it registers this
/// handler through a canvas to receive composed text (IME) and platform paste.
/// The edit language is ASCII, so a UTF-16 index is a byte index and the caret
/// always sits at the end of the typed text.
impl EntityInputHandler for Table {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<String> {
        let text = self.model.edit_text()?;
        let start = range.start.min(text.len());
        let end = range.end.min(text.len()).max(start);
        *adjusted_range = Some(start..end);
        Some(text[start..end].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<UTF16Selection> {
        let text = self.model.edit_text()?;
        Some(UTF16Selection {
            range: text.len()..text.len(),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<'_, Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<'_, Self>) {}

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // Typing into an idle table starts an edit at the cursor, exactly as a
        // key press did before text came through the platform handler.
        if self.model.edit_text().is_none() {
            let mut characters = text.chars();
            let Some(first) = characters.find(|character| is_edit_start(*character)) else {
                return;
            };
            if !self.model.begin_edit(Some(first)) {
                return;
            }
            for character in characters {
                self.model.push_char(character);
            }
        } else if !self.model.replace_edit_text(range, text) {
            return;
        }
        cx.emit(TableEvent::SelectionChanged);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.replace_text_in_range(range, new_text, window, cx);
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<usize> {
        None
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> Option<usize> {
        self.model.edit_text().map(str::len)
    }
}

impl Table {
    /// An empty table.
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            model: TableModel::new(),
            focus: cx.focus_handle(),
            body_scroll: ScrollHandle::new(),
            dragging: false,
            realtime: None,
            header_wash_for_ranges: false,
        }
    }

    /// The model, for status lines and tests.
    pub fn model(&self) -> &TableModel {
        &self.model
    }

    /// Replaces the snapshot and repaints.
    pub fn set_table(&mut self, table: TableView, cx: &mut Context<'_, Self>) {
        self.model.set_table(table);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Shows a value in one cell while a paired curve drags it, or clears the
    /// preview with `None`. The value is display-only: it is not recorded for
    /// undo and it never reaches the core.
    pub fn preview_cell(&mut self, cell: CellPos, value: Option<f64>, cx: &mut Context<'_, Self>) {
        self.model.set_preview(value.map(|value| (cell, value)));
        cx.notify();
    }

    /// Selects one cell without telling the owner, so a paired view can mirror
    /// the table cursor without echoing the event back.
    pub fn select_cell(&mut self, cell: CellPos, cx: &mut Context<'_, Self>) {
        self.model.select(cell);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Flips the displayed Y axis.
    pub fn set_flip_y(&mut self, flip: bool, cx: &mut Context<'_, Self>) {
        self.model.set_flip_y(flip);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Sets whether a multi-cell selection also tints the row and column bins.
    ///
    /// Off by default: a range is read on the cells themselves, and washing
    /// every bin it spans only adds noise to the headers. A single selected
    /// cell tints its two bins either way.
    pub fn set_header_wash_for_ranges(&mut self, wash: bool, cx: &mut Context<'_, Self>) {
        self.header_wash_for_ranges = wash;
        cx.notify();
    }

    /// Drops optimistic values after the core rejected an edit.
    pub fn reject_pending(&mut self, cx: &mut Context<'_, Self>) {
        self.model.reject_pending();
        cx.notify();
    }

    /// Drops the table, keeping the Y-axis preference.
    pub fn clear_table(&mut self, cx: &mut Context<'_, Self>) {
        self.model.clear_table();
        cx.notify();
    }

    /// Drops any open editor and collapses the selection to the cursor.
    ///
    /// Clicks that land on table chrome or outside the grid call this: one
    /// focus owner at a time, and a click that selects nothing still leaves
    /// the grid in a clean state for the next gesture.
    pub fn dismiss_focus(&mut self, cx: &mut Context<'_, Self>) {
        self.model.cancel_edit();
        self.model.cancel_axis_edit();
        self.model.collapse_selection();
        cx.emit(TableEvent::SelectionChanged);
        cx.notify();
    }

    /// Sets the realtime operating point, or clears it.
    pub fn set_realtime(&mut self, point: Option<RealtimePoint>, cx: &mut Context<'_, Self>) {
        self.realtime = point;
        cx.notify();
    }

    /// Copies the selection as TSV.
    pub fn copy_selection(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(text) = self.model.copy_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        cx.notify();
    }

    /// Pastes TSV at the selection's top-left corner.
    pub fn paste_clipboard(&mut self, cx: &mut Context<'_, Self>) {
        let text = cx.read_from_clipboard().and_then(|item| item.text());
        let Some(text) = text else {
            cx.notify();
            return;
        };
        match self.model.paste_edits(&text) {
            Ok(edits) => self.request_edits(edits, cx),
            Err(_) => cx.notify(),
        }
    }

    /// Applies a bulk operation to the selection.
    pub fn bulk(&mut self, op: BulkOp, cx: &mut Context<'_, Self>) {
        match self.model.bulk_edit(op) {
            Ok(edits) => {
                if !edits.is_empty() {
                    self.request_edits(edits, cx);
                }
                cx.notify();
            }
            Err(_) => cx.notify(),
        }
    }

    /// Takes the newest edit back and asks the core to write the old values.
    pub fn undo(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(step) = self.model.undo() {
            self.request_step(step, cx);
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Re-applies the newest undone edit.
    pub fn redo(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(step) = self.model.redo() {
            self.request_step(step, cx);
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Spaces one axis onto even steps between its endpoints.
    pub fn rebin(&mut self, axis: oxtuner_core::AxisKind, cx: &mut Context<'_, Self>) {
        match self.model.rebin_axis(axis) {
            Ok(edits) => {
                if edits.is_empty() {
                    cx.notify();
                } else {
                    self.request_axis_edits(axis, edits, cx);
                }
            }
            Err(_) => cx.notify(),
        }
    }

    fn request_step(&mut self, step: HistoryStep, cx: &mut Context<'_, Self>) {
        match step {
            HistoryStep::Cells(edits) => self.request_edits(edits, cx),
            HistoryStep::Axis { axis, edits } => self.request_axis_edits(axis, edits, cx),
        }
    }

    /// The keyboard contract, in one place so tests can drive it through real
    /// events and a reader can see the whole rule set.
    pub(crate) fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<'_, Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let extend = modifiers.shift;

        if self.model.axis_editing().is_some() {
            match key {
                "enter" | "tab" => {
                    let axis = self.model.axis_editing().map(|editing| editing.target.axis);
                    if let Ok(edits) = self.model.commit_axis_edit() {
                        if !edits.is_empty()
                            && let Some(axis) = axis
                        {
                            self.request_axis_edits(axis, edits, cx);
                        } else {
                            cx.notify();
                        }
                    } else {
                        self.model.mark_axis_invalid();
                        cx.notify();
                    }
                }
                "escape" => {
                    self.model.cancel_axis_edit();
                    cx.notify();
                }
                "backspace" => {
                    self.model.pop_axis_char();
                    cx.notify();
                }
                // Printable text arrives through the platform input handler
                // below, which is what makes IME composition work.
                _ => {}
            }
            cx.emit(TableEvent::SelectionChanged);
            return;
        }

        if self.model.editing().is_some() {
            match key {
                "enter" | "tab" => {
                    let forward = key != "tab" || !modifiers.shift;
                    match self.model.commit_edit() {
                        Ok(edits) => {
                            if !edits.is_empty() {
                                self.request_edits(edits, cx);
                            }
                            if key == "tab" {
                                self.model.tab(forward);
                                self.ensure_cursor_visible();
                            }
                        }
                        Err(_) => self.model.mark_edit_invalid(),
                    }
                }
                "escape" => self.model.cancel_edit(),
                "backspace" => self.model.pop_char(),
                _ => {}
            }
            cx.emit(TableEvent::SelectionChanged);
            cx.notify();
            return;
        }

        if modifiers.platform {
            match key {
                "c" => self.copy_selection(cx),
                "v" => self.paste_clipboard(cx),
                "a" => {
                    self.model.select_all();
                    cx.notify();
                }
                "z" => {
                    if modifiers.shift {
                        self.redo(cx);
                    } else {
                        self.undo(cx);
                    }
                }
                "y" => self.redo(cx),
                _ => {}
            }
            return;
        }

        match key {
            "left" | "h" => self.model.move_cursor(0, -1, extend),
            "right" | "l" => self.model.move_cursor(0, 1, extend),
            "up" | "k" => self.model.move_cursor(-1, 0, extend),
            "down" | "j" => self.model.move_cursor(1, 0, extend),
            "tab" => self.model.tab(!modifiers.shift),
            "enter" => {
                self.model.begin_edit(None);
            }
            "escape" => self.model.collapse_selection(),
            "home" => self.model.move_to_edge(true, extend),
            "end" => self.model.move_to_edge(false, extend),
            "pageup" => self.model.move_page(false, extend),
            "pagedown" => self.model.move_page(true, extend),
            // Printable text starts an edit when the platform handler delivers
            // it; see `replace_text_in_range`.
            _ => {}
        }

        self.ensure_cursor_visible();
        cx.emit(TableEvent::SelectionChanged);
        cx.notify();
    }

    /// Moves the cursor one cell, for the frame-budget harness.
    #[cfg(test)]
    pub(crate) fn nudge_selection(&mut self, cx: &mut Context<'_, Self>) {
        let rows = self.model.rows();
        let columns = self.model.columns();
        if rows == 0 || columns == 0 {
            return;
        }
        let cursor = self.model.selection().cursor;
        let column = (cursor.column + 1) % columns;
        let row = if column == 0 {
            (cursor.row + 1) % rows
        } else {
            cursor.row
        };
        self.model.select(CellPos { row, column });
        cx.notify();
    }

    fn request_edits(&mut self, edits: Vec<CellEdit>, cx: &mut Context<'_, Self>) {
        let Some(table) = self.model.table() else {
            return;
        };
        let name = table.name.clone();
        cx.emit(TableEvent::EditRequested { name, edits });
        cx.notify();
    }

    fn request_axis_edits(
        &mut self,
        axis: oxtuner_core::AxisKind,
        edits: Vec<oxtuner_core::AxisEdit>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(table) = self.model.table() else {
            return;
        };
        let id = table.id.clone();
        cx.emit(TableEvent::AxisEditRequested {
            table: id,
            axis,
            edits,
        });
        cx.notify();
    }

    /// Scrolls the scrollable body until the cursor cell is inside it.
    fn ensure_cursor_visible(&self) {
        let bounds = self.body_scroll.bounds();
        if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return;
        }

        let cursor = self.model.selection().cursor;
        let display = self.model.display_row(cursor.row);
        let left = px(cursor.column as f32 * (CELL_WIDTH + CELL_GAP));
        let top = px(display as f32 * (CELL_HEIGHT + CELL_GAP));
        let maximum = self.body_scroll.max_offset();
        let mut offset = self.body_scroll.offset();

        if left + offset.x < px(0.) {
            offset.x = -left;
        } else if left + px(CELL_WIDTH) + offset.x > bounds.size.width {
            offset.x = bounds.size.width - left - px(CELL_WIDTH);
        }
        if top + offset.y < px(0.) {
            offset.y = -top;
        } else if top + px(CELL_HEIGHT) + offset.y > bounds.size.height {
            offset.y = bounds.size.height - top - px(CELL_HEIGHT);
        }

        offset.x = offset.x.clamp(-maximum.x, px(0.));
        offset.y = offset.y.clamp(-maximum.y, px(0.));
        if offset != self.body_scroll.offset() {
            self.body_scroll.set_offset(offset);
        }
    }

    fn render_cell(
        &self,
        data_row: usize,
        column: usize,
        editing: Option<&Editing>,
        minimum: f64,
        maximum: f64,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let cell = CellPos {
            row: data_row,
            column,
        };
        let value = self.model.value(cell);
        let editing = editing.filter(|editing| editing.cell == cell);

        let fill = heat_color(minimum, maximum, value);

        // let text_color = contrast_text(fill);
        // The old per-cell contrast picker, commented out because a fixed dark
        // text colour reads fine on the palette. Restore it with its import
        // above if the palette ever grows bright enough to need white text.
        let text_color = Hsla::from(rgb(0x121314));
        let display = self.model.display(cell).unwrap_or_else(|| Arc::from(""));
        let aria = self.model.aria(cell).unwrap_or_else(|| Arc::from(""));
        let theme = cx.theme();

        let id = ElementId::from(("table-cell", data_row * self.model.columns() + column));
        let on_press = cx.listener(move |this, event: &MouseDownEvent, window, cx| {
            window.focus(&this.focus, cx);
            this.model.clear_action();
            if event.click_count >= 2 {
                this.model.select(cell);
                this.model.begin_edit(None);
            } else if event.modifiers.shift {
                this.model.extend_to(cell);
            } else {
                this.dragging = true;
                this.model.begin_drag(cell);
            }
            cx.stop_propagation();
            cx.emit(TableEvent::SelectionChanged);
            cx.notify();
        });
        let on_drag = cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            if this.dragging && event.pressed_button == Some(MouseButton::Left) {
                this.model.extend_to(cell);
                cx.emit(TableEvent::SelectionChanged);
                cx.notify();
            }
        });

        TableCell {
            id,
            display: SharedString::from(display),
            aria: SharedString::from(aria),
            fill,
            text_color,
            danger: theme.danger,
            info: theme.info,
            editing: editing.map(|editing| CellEditing {
                text: SharedString::from(editing.text.clone()),
                invalid: editing.invalid,
            }),
            mark: decoration(
                self.model.state(cell),
                self.model.changed(cell),
                theme.danger,
                theme.warning,
                theme.info,
            ),
            realtime: self
                .realtime
                .is_some_and(|point| point.row == data_row && point.column == column),
            on_press: Box::new(on_press),
            on_drag: Box::new(on_drag),
        }
        .into_any_element()
    }

    fn render_column_headers(
        &self,
        columns: std::ops::Range<usize>,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let Some(snapshot) = self.model.table() else {
            return Vec::new();
        };
        let can_edit = snapshot.writable;
        let theme = cx.theme();
        let background = theme.secondary;
        let foreground = theme.foreground;
        let danger = theme.danger;
        // A single cell names the bins it sits between, the way a spreadsheet
        // does, so a value can be read against both axes at once. A range is
        // read on the cells themselves and leaves the bins plain unless the
        // owner asked for the wider wash.
        let selection = self.model.selection();
        let washes_bins = self.header_wash_for_ranges || selection.cell_count() == 1;
        let (_, left, _, right) = selection.bounds();
        let wash = theme.primary.opacity(0.28);

        columns
            .map(|column| {
                let value = self.model.axis_value(oxtuner_core::AxisKind::X, column);
                let label = format_value(value, usize::from(snapshot.x_axis.digits));
                let units = snapshot.x_axis.units_suffix();
                let aria = format!("{label}{units} {}", snapshot.x_axis.label);
                let editing = self.model.axis_editing().filter(|editing| {
                    editing.target.axis == oxtuner_core::AxisKind::X
                        && editing.target.index == column
                });
                let header_editing = editing.map(|editing| HeaderEditing {
                    text: SharedString::from(editing.text.clone()),
                    invalid: editing.invalid,
                });
                let on_press = cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    // A double press is the edit gesture, matching the cells.
                    // Any other press dismisses: a bin edit owns the focus,
                    // so a click that opens nothing must not leave the old
                    // owners behind.
                    if can_edit && event.click_count >= 2 {
                        this.model.clear_action();
                        if this
                            .model
                            .begin_axis_edit(oxtuner_core::AxisKind::X, column)
                        {
                            cx.notify();
                        }
                    } else if !this.model.axis_editing().is_some_and(|editing| {
                        editing.target.axis == oxtuner_core::AxisKind::X
                            && editing.target.index == column
                    }) {
                        this.dismiss_focus(cx);
                    }
                    cx.stop_propagation();
                    cx.emit(TableEvent::SelectionChanged);
                });

                ColumnHeader {
                    id: ("column-header", column).into(),
                    label: SharedString::from(label),
                    aria: SharedString::from(aria),
                    background,
                    foreground,
                    danger,
                    highlight: (washes_bins && (left..=right).contains(&column)).then(|| {
                        BinHighlight {
                            id: ("column-header-highlight", column).into(),
                            color: wash,
                        }
                    }),
                    editing: header_editing,
                    on_press: Box::new(on_press),
                }
                .into_any_element()
            })
            .collect()
    }

    fn render_row_headers(
        &self,
        rows: std::ops::Range<usize>,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let Some(snapshot) = self.model.table() else {
            return Vec::new();
        };
        let can_edit = snapshot.writable;
        let theme = cx.theme();
        let background = theme.secondary;
        let foreground = theme.foreground;
        let danger = theme.danger;
        // Rows are matched in definition order: the band may run downhill
        // when the axis is flipped, while the selection never changes shape.
        // The same rule as the columns: only one selected cell names its bin.
        let selection = self.model.selection();
        let washes_bins = self.header_wash_for_ranges || selection.cell_count() == 1;
        let (top, _, bottom, _) = selection.bounds();
        let wash = theme.primary.opacity(0.28);

        rows.map(|display_row| {
            let data_row = self.model.data_row(display_row);
            let value = self.model.axis_value(oxtuner_core::AxisKind::Y, data_row);
            let label = format_value(value, usize::from(snapshot.y_axis.digits));
            let units = snapshot.y_axis.units_suffix();
            let aria = format!("{label}{units} {}", snapshot.y_axis.label);
            let editing = self.model.axis_editing().filter(|editing| {
                editing.target.axis == oxtuner_core::AxisKind::Y && editing.target.index == data_row
            });
            let header_editing = editing.map(|editing| HeaderEditing {
                text: SharedString::from(editing.text.clone()),
                invalid: editing.invalid,
            });
            let on_press = cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                window.focus(&this.focus, cx);
                // A double press edits, matching the column headers and the
                // cells. Any other press dismisses, so a bin edit never
                // shares the focus with the cell range. The bin index is the
                // data row: on a flipped axis the top displayed row is the
                // highest bin.
                if can_edit && event.click_count >= 2 {
                    this.model.clear_action();
                    if this
                        .model
                        .begin_axis_edit(oxtuner_core::AxisKind::Y, data_row)
                    {
                        cx.notify();
                    }
                } else if !this.model.axis_editing().is_some_and(|editing| {
                    editing.target.axis == oxtuner_core::AxisKind::Y
                        && editing.target.index == data_row
                }) {
                    this.dismiss_focus(cx);
                }
                cx.stop_propagation();
                cx.emit(TableEvent::SelectionChanged);
            });

            RowHeader {
                id: ("row-header", display_row).into(),
                label: SharedString::from(label),
                aria: SharedString::from(aria),
                background,
                foreground,
                danger,
                highlight: (washes_bins && (top..=bottom).contains(&data_row)).then(|| {
                    BinHighlight {
                        id: ("row-header-highlight", display_row).into(),
                        color: wash,
                    }
                }),
                editing: header_editing,
                on_press: Box::new(on_press),
            }
            .into_any_element()
        })
        .collect()
    }
}

impl Render for Table {
    #[expect(
        clippy::too_many_lines,
        reason = "the grid composes headers, rows, cells and input in one retained render"
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some(table) = self.model.table().cloned() else {
            return div().size_full().into_any_element();
        };

        let rows = table.rows;
        let columns = table.cols;
        let minimum = table.minimum;
        let maximum = table.maximum;
        let x_caption = table.x_axis.caption();
        let y_caption = table.y_axis.caption();
        let selection = self.model.selection();
        let editing = self.model.editing().cloned();
        // Wheel events add their delta to the scroll handle before layout
        // runs, and layout clamps it to the content without repainting. The
        // body therefore paints the clamped offset while a raw read here can
        // still hold the overshoot, which pushed the frozen headers past the
        // cells after scrolling into a limit. Clamping here keeps both sides
        // on the same value in every frame.
        let raw_offset = self.body_scroll.offset();
        let max_offset = self.body_scroll.max_offset();
        let offset = point(
            raw_offset.x.clamp(-max_offset.x, px(0.)),
            raw_offset.y.clamp(-max_offset.y, px(0.)),
        );
        let body_width =
            px(columns as f32 * CELL_WIDTH + columns.saturating_sub(1) as f32 * CELL_GAP);
        let body_height = px(rows as f32 * CELL_HEIGHT + rows.saturating_sub(1) as f32 * CELL_GAP);
        // The scroll handle's bounds are only refreshed after a paint, so they
        // still describe the previous table on the frame a new one opens. A
        // short table would then report a viewport too small for a tall one and
        // the bottom rows would stay unbuilt until something repainted. The
        // window is an upper bound on the body, which over-renders a little but
        // can never hide a row that is on screen.
        let viewport = window.viewport_size();
        let step_x = CELL_WIDTH + CELL_GAP;
        let step_y = CELL_HEIGHT + CELL_GAP;
        let (first_column, last_column) = visible_range(offset.x, viewport.width, step_x, columns);
        let (first_row, last_row) = visible_range(offset.y, viewport.height, step_y, rows);

        let theme = cx.theme().clone();
        let cell_border_color = theme.border;
        let flipped = self.model.flip_y();

        let column_headers = self.render_column_headers(first_column..last_column, cx);
        // A one-row table is a key/value curve: there is no Y axis, so it
        // draws no row band at all and its caption sits above the values.
        let single_row = rows <= 1;
        let row_headers = if single_row {
            Vec::new()
        } else {
            self.render_row_headers(first_row..last_row, cx)
        };

        let body_rows = (first_row..last_row)
            .map(|display_row| {
                let data_row = self.model.data_row(display_row);
                let cells = (first_column..last_column)
                    .map(|column| {
                        self.render_cell(data_row, column, editing.as_ref(), minimum, maximum, cx)
                    })
                    .collect::<Vec<_>>();
                h_flex()
                    .gap(px(CELL_GAP))
                    .children(cells)
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        // The corner is empty: each axis names itself beside its own band, so
        // the caption cannot be mistaken for a row or column label.
        let corner = div()
            .id("table-corner")
            .test_support()
            .w(px(AXIS_WIDTH))
            .h(px(AXIS_HEIGHT))
            .flex_shrink_0()
            .bg(theme.secondary)
            .flex()
            .items_center()
            .justify_center()
            .child(
                Button::new("re-bins")
                    .icon(IconName::Move3d)
                    .ghost()
                    .small()
                    .w_full()
                    .h_full()
                    .rounded_none(),
            );

        let header_band = h_flex()
            .h(px(AXIS_HEIGHT))
            .flex_shrink_0()
            .gap(px(CELL_GAP))
            // Contrast between theme
            .bg(cell_border_color)
            .when(!single_row, |this| this.child(corner))
            // One clipped viewport: the headers ride the scroll offset inside
            // it, and the band's trailing space keeps the plain background.
            .child(
                div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .absolute()
                            .left(offset.x + px(first_column as f32 * step_x))
                            .top(px(0.))
                            .w(body_width)
                            .h_full()
                            // Contrast between theme
                            .bg(cell_border_color)
                            .gap(px(CELL_GAP))
                            .children(column_headers),
                    ),
            );

        // The selection is painted over the cells, the way a spreadsheet does
        // it: one soft tint across a multi-cell range and one outline around
        // the whole range. Drawing it over the grid instead of bordering each
        // cell keeps every value on the same pixel grid whether or not it is
        // selected, and the tint is thin enough to read the values through.
        let (top, left, bottom, right) = selection.bounds();
        let top_display = self.model.display_row(top);
        let bottom_display = self.model.display_row(bottom);
        let first_display = top_display.min(bottom_display);
        let rows_span = (top_display.max(bottom_display) - first_display + 1) as f32;

        let selection_width =
            (right - left + 1) as f32 * CELL_WIDTH + (right - left) as f32 * CELL_GAP;
        let selection_height = rows_span * CELL_HEIGHT + (rows_span - 1.) * CELL_GAP;
        // The heat colours span the whole hue circle, so a tinted accent
        // disappears on cells of the same hue. Foreground contrasts with every
        // fill in both themes.
        let selection_fill = theme.primary.opacity(0.35);

        let mut selection_layer = div()
            .absolute()
            .left(px(left as f32 * step_x))
            .top(px(first_display as f32 * step_y))
            .w(px(selection_width))
            .h(px(selection_height));

        // The tint is translucent and covers the whole range, the cursor cell
        // included: every selected value stays readable under it, and no cell
        // in a filled range looks left out.
        if selection.cell_count() > 1 {
            selection_layer = selection_layer.child(
                div()
                    .id("selection-fill")
                    .test_support()
                    .absolute()
                    .inset_0()
                    .bg(selection_fill),
            );
        }

        // The outline straddles the grid line instead of sitting inside the
        // range. Drawn on the bounds it would eat two pixels off the edge
        // cells, which makes a single selected cell read as inset.
        //
        // Every ramp stop sits at mid lightness, so the frame goes dark to
        // separate from all of them, yellow and red included. Purple keeps
        // the hue outside the red-to-blue span at the same time.
        selection_layer = selection_layer.child(
            div()
                .id("selection-frame")
                .test_support()
                .absolute()
                .left(px(-CELL_GAP))
                .top(px(-CELL_GAP))
                .w(px(selection_width + 2. * CELL_GAP))
                .h(px(selection_height + 2. * CELL_GAP))
                .border_2()
                .border_color(Hsla {
                    h: 288.0 / 360.0,
                    s: 0.90,
                    l: 0.38,
                    a: 0.8,
                }),
        );

        // The captions sit outside the grid box, the way a chart labels its
        // axes: the X caption on the side the column band takes.
        //
        // A table with rows stacks the Y caption one character per line in its
        // own column to the left, because glyphs cannot be rotated through the
        // styling API (the SVG element draws with its own font stack, so it
        // does not match application text). A space becomes a short gap
        // instead of an empty line, which would pull a two-word caption apart.
        // A one-row table has no bins to run beside, so its name is ordinary
        // text on the values' side instead: above them by default, below them
        // when the axis is flipped.
        let y_label = |stacked: bool, below: bool| -> AnyElement {
            if !stacked {
                return div()
                    .id("y-axis-label")
                    .test_support()
                    .aria_label(y_caption.clone())
                    .h(px(AXIS_LABEL_HEIGHT))
                    .w(body_width)
                    .when(below, |this| this.mt(px(AXIS_LABEL_GAP)))
                    .when(!below, |this| this.mb(px(AXIS_LABEL_GAP)))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_center()
                            .child(y_caption.clone()),
                    )
                    .into_any_element();
            }

            // The column is as tall as the caption, never shorter: the row
            // takes its height from the stacked text and the grid sits at the
            // foot, so a long name beside a short table is not clipped.
            div()
                .id("y-axis-label")
                .test_support()
                .aria_label(y_caption.clone())
                .flex_shrink_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(crate::caption::y_caption(
                    &table.y_axis.label,
                    &table.y_axis.units,
                    AXIS_LABEL_WIDTH,
                    theme.muted_foreground,
                ))
                .into_any_element()
        };

        // The X caption sits outside the grid box, centred over it.
        let x_label = div()
            .id("x-axis-label")
            .test_support()
            .aria_label(x_caption.clone())
            .h(px(AXIS_LABEL_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().min_w_0().truncate().text_center().child(x_caption));

        let mut body_band = h_flex()
            // The band takes exactly the table's height and shrinks only when
            // the editor cannot show it all. Growing to fill the viewport is
            // what pulled the column band away from the last row.
            .h(body_height)
            .flex_shrink_1()
            .min_h_0()
            .gap(px(CELL_GAP))
            // Contrast between theme
            .bg(cell_border_color);

        if !single_row {
            body_band = body_band.child(
                // One clipped viewport for the frozen row band: the headers
                // ride the body's scroll offset inside it.
                div()
                    .relative()
                    .w(px(AXIS_WIDTH))
                    .h_full()
                    .flex_shrink_0()
                    .overflow_hidden()
                    .border_b_1()
                    // Contrast between theme
                    .border_color(cell_border_color)
                    .child(
                        v_flex()
                            .absolute()
                            .left(px(0.))
                            .top(offset.y + px(first_row as f32 * step_y))
                            .w(px(AXIS_WIDTH))
                            .h(body_height)
                            // Contrast between theme
                            .bg(cell_border_color)
                            .gap(px(CELL_GAP))
                            .children(row_headers),
                    ),
            );
        }

        body_band = body_band.child(
            div()
                .id("table-body")
                .size_full()
                .min_w_0()
                .overflow_scroll()
                .track_scroll(&self.body_scroll)
                .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                .child(
                    // The content is exactly the grid's size: it carries
                    // the grid-line colour, and the cells sit on top.
                    div()
                        .relative()
                        .w(body_width)
                        .h(body_height)
                        // Contrast between theme
                        .bg(cell_border_color)
                        .child(
                            v_flex()
                                .absolute()
                                .left(px(first_column as f32 * step_x))
                                .top(px(first_row as f32 * step_y))
                                .gap(px(CELL_GAP))
                                .children(body_rows),
                        )
                        // Painted after the cells so the tint and the
                        // outline sit on top; being plain frames they leave
                        // the cells underneath fully clickable. A bin edit
                        // owns the focus instead: the cell range stays in
                        // the model but is not drawn until the edit lands.
                        .when(self.model.axis_editing().is_none(), |this| {
                            this.child(selection_layer)
                        }),
                ),
        );

        // Native text services need a handler registered while the frame
        // paints. A canvas fills the grid without taking part in layout or hit
        // testing; it only gives IME and platform paste somewhere to attach.
        let input = {
            let focus = self.focus.clone();
            let entity = cx.entity();
            canvas(
                |_, _, _| {},
                move |bounds, (), window, cx| {
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, entity.clone()),
                        cx,
                    );
                },
            )
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
        };
        let aria = format!("{}: {} rows by {} columns", table.title, rows, columns);

        // The column band follows the low end of the Y axis, which is the
        // bottom by default (chart orientation) and the top when flipped. One
        // column holds the bands and the X caption, so the caption is centred
        // over the table it names.
        //
        // The box draws the table's hairline around the two bands only, so
        // both axis captions stay outside it.
        let mut table_box = v_flex()
            .id("table-box")
            .test_support()
            .flex_shrink_1()
            .min_h_0()
            .border_1()
            .border_color(theme.border);

        if flipped {
            table_box = table_box.child(header_band).child(body_band);
        } else {
            table_box = table_box.child(body_band).child(header_band);
        }

        let mut grid = v_flex()
            .relative()
            .flex_grow_0()
            .flex_shrink_1()
            .min_w_0()
            .min_h_0()
            .justify_end()
            .child(input);

        if single_row {
            // The curve keeps each caption on its own axis: flipping moves the
            // X caption and bins above the values and the Y caption below.
            grid = if flipped {
                grid.child(x_label.mb(px(AXIS_LABEL_GAP)))
                    .child(table_box)
                    .child(y_label(false, true))
            } else {
                grid.child(y_label(false, false))
                    .child(table_box)
                    .child(x_label.mt(px(AXIS_LABEL_GAP)))
            };
        } else if flipped {
            grid = grid.child(x_label.mb(px(AXIS_LABEL_GAP))).child(table_box);
        } else {
            grid = grid.child(table_box).child(x_label.mt(px(AXIS_LABEL_GAP)));
        }

        // The grid is transparent: it fills the box its owner gives it and
        // paints nothing around it, so the panel colour and the bounds stay
        // the application's business.
        div()
            .id("table")
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .text_color(theme.foreground)
            .role(Role::Grid)
            .aria_label(aria)
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _, cx| this.handle_key(event, cx)),
            )
            // Cells and headers stop their own presses, so a press that
            // reaches here landed on chrome or padding and selects nothing.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.dismiss_focus(cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
            )
            .child(if single_row {
                // The curve caption sits above its values, so the table
                // reads top to bottom: name, values, X bins, X caption. The
                // row keeps the grid at the cells' width instead of letting
                // the caption stretch it across the panel.
                h_flex()
                    .flex_row()
                    .flex_grow_1()
                    .min_h_0()
                    .child(grid)
                    .into_any_element()
            } else {
                // The Y caption sits beside the grid, so it is centred on the
                // table rather than on the empty space below it.
                h_flex()
                    .flex_row()
                    .items_stretch()
                    .flex_grow_0()
                    .flex_shrink_1()
                    .min_h_0()
                    .gap(px(AXIS_LABEL_GAP))
                    .child(y_label(true, false))
                    .child(grid)
                    .into_any_element()
            })
            .into_any_element()
    }
}

/// Formats a table value with the definition's digits.
fn format_value(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

fn is_edit_start(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, '-' | '+' | '*' | '/' | '=')
}
