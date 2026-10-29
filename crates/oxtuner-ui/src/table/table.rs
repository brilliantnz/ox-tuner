//! The reusable tune table.
//!
//! One component serves every value table: the tune editor today, and any
//! future surface that shows a `TableSummary`. It owns only view state —
//! selection, cursor, the in-progress edit, the scroll position — and reads
//! values from [`TableModel`], a plain-Rust state that is tested without a
//! window (ADR-0013, ADR-0020).
//!
//! The table never writes tune values. A committed edit becomes a
//! [`TableEvent::EditRequested`], the owner sends it to the core, and the
//! echoed [`oxtuner_core::TableSummary`] clears the optimistic value. That
//! keeps the core the only owner of the tune while the cell still changes in
//! the same frame the key was pressed.
//!
//! Headers are frozen: the body scrolls under fixed header bands shifted by
//! the body's scroll offset. The column band follows the low end of the Y
//! axis: at the bottom by default and at the top when the axis is flipped.
//! There is no scrollbar; the body scrolls with the wheel, drag-select and
//! the keyboard.

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementId, ElementInputHandler,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, Render, Role, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, UTF16Selection, Window, canvas, div, px,
};

use oxtuner_core::{CellEdit, TableSummary};

use super::cell::{CellEditing, TableCell};
use super::geometry::{AXIS_HEIGHT, AXIS_WIDTH, CELL_GAP, CELL_HEIGHT, CELL_WIDTH, visible_range};
use super::headers::{ColumnHeader, HeaderEditing, RowHeader};
use super::heatmap::{contrast_text, decoration, heat_color};
use super::model::{BulkOp, CellPos, Editing, HistoryStep, Selection, TableModel};
use super::style::TableStyle;

/// What the table asks its owner to do.
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
pub struct Table {
    model: TableModel,
    style: TableStyle,
    focus: FocusHandle,
    body_scroll: ScrollHandle,
    dragging: bool,
    realtime: Option<RealtimePoint>,
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
        _: &mut Context<Self>,
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
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let text = self.model.edit_text()?;
        Some(UTF16Selection {
            range: text.len()..text.len(),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
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
        cx: &mut Context<Self>,
    ) {
        self.replace_text_in_range(range, new_text, window, cx);
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        self.model.edit_text().map(str::len)
    }
}

impl Table {
    /// An empty table with the default style.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            model: TableModel::new(),
            style: TableStyle::default(),
            focus: cx.focus_handle(),
            body_scroll: ScrollHandle::new(),
            dragging: false,
            realtime: None,
        }
    }

    /// The model, for status lines and tests.
    pub fn model(&self) -> &TableModel {
        &self.model
    }

    /// Replaces the visual tokens.
    pub fn set_style(&mut self, style: TableStyle, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    /// The current tokens, so a host can compare before pushing a new set.
    pub fn style(&self) -> &TableStyle {
        &self.style
    }

    /// Replaces the snapshot and repaints.
    pub fn set_table(&mut self, table: TableSummary, cx: &mut Context<Self>) {
        self.model.set_table(table);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Flips the displayed Y axis.
    pub fn set_flip_y(&mut self, flip: bool, cx: &mut Context<Self>) {
        self.model.set_flip_y(flip);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Drops optimistic values after the core rejected an edit.
    pub fn reject_pending(&mut self, cx: &mut Context<Self>) {
        self.model.reject_pending();
        cx.notify();
    }

    /// Drops the table, keeping the Y-axis preference.
    pub fn clear_table(&mut self, cx: &mut Context<Self>) {
        self.model.clear_table();
        cx.notify();
    }

    /// Sets the realtime operating point, or clears it.
    pub fn set_realtime(&mut self, point: Option<RealtimePoint>, cx: &mut Context<Self>) {
        self.realtime = point;
        cx.notify();
    }

    /// Copies the selection as TSV.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.model.copy_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        cx.notify();
    }

    /// Pastes TSV at the selection's top-left corner.
    pub fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
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
    pub fn bulk(&mut self, op: BulkOp, cx: &mut Context<Self>) {
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
    pub fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some(step) = self.model.undo() {
            self.request_step(step, cx);
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Re-applies the newest undone edit.
    pub fn redo(&mut self, cx: &mut Context<Self>) {
        if let Some(step) = self.model.redo() {
            self.request_step(step, cx);
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Spaces one axis onto even steps between its endpoints.
    pub fn rebin(&mut self, axis: oxtuner_core::AxisKind, cx: &mut Context<Self>) {
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

    fn request_step(&mut self, step: HistoryStep, cx: &mut Context<Self>) {
        match step {
            HistoryStep::Cells(edits) => self.request_edits(edits, cx),
            HistoryStep::Axis { axis, edits } => self.request_axis_edits(axis, edits, cx),
        }
    }

    /// The keyboard contract, in one place so tests can drive it through real
    /// events and a reader can see the whole rule set.
    pub(crate) fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let extend = modifiers.shift;

        if self.model.axis_editing().is_some() {
            match key {
                "enter" | "tab" => {
                    let axis = self.model.axis_editing().map(|editing| editing.target.axis);
                    match self.model.commit_axis_edit() {
                        Ok(edits) => {
                            if !edits.is_empty()
                                && let Some(axis) = axis
                            {
                                self.request_axis_edits(axis, edits, cx);
                            } else {
                                cx.notify();
                            }
                        }
                        Err(_) => {
                            self.model.mark_axis_invalid();
                            cx.notify();
                        }
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
    pub(crate) fn nudge_selection(&mut self, cx: &mut Context<Self>) {
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

    fn request_edits(&mut self, edits: Vec<CellEdit>, cx: &mut Context<Self>) {
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
        cx: &mut Context<Self>,
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

    #[allow(clippy::too_many_arguments)]
    fn render_cell(
        &self,
        data_row: usize,
        column: usize,
        selection: Selection,
        editing: Option<&Editing>,
        minimum: f64,
        maximum: f64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cell = CellPos {
            row: data_row,
            column,
        };
        let value = self.model.value(cell);
        let is_cursor = selection.cursor == cell;
        let editing = editing.filter(|editing| editing.cell == cell);

        let fill = heat_color(minimum, maximum, value);
        let text_color = contrast_text(fill);
        let display = self.model.display(cell).unwrap_or_else(|| Arc::from(""));
        let aria = self.model.aria(cell).unwrap_or_else(|| Arc::from(""));

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
            danger: self.style.danger,
            info: self.style.info,
            cursor: is_cursor,
            editing: editing.map(|editing| CellEditing {
                text: SharedString::from(editing.text.clone()),
                invalid: editing.invalid,
            }),
            mark: decoration(
                self.model.state(cell),
                self.model.changed(cell),
                &self.style,
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
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(snapshot) = self.model.table() else {
            return Vec::new();
        };
        let can_edit = snapshot.writable;
        let background = self.style.surface;
        let muted = self.style.muted;
        let danger = self.style.danger;

        columns
            .map(|column| {
                let value = self.model.axis_value(oxtuner_core::AxisKind::X, column);
                let label = format_value(value, usize::from(snapshot.x_axis.digits));
                let units = axis_units(&snapshot.x_axis);
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
                    // One press selects nothing: a double press is the edit
                    // gesture, matching the cells, so a click on the way to
                    // another table cannot leave a bin editor behind.
                    if can_edit && event.click_count >= 2 {
                        this.model.clear_action();
                        if this
                            .model
                            .begin_axis_edit(oxtuner_core::AxisKind::X, column)
                        {
                            cx.notify();
                        }
                    }
                    cx.stop_propagation();
                    cx.emit(TableEvent::SelectionChanged);
                });

                ColumnHeader {
                    id: ("column-header", column).into(),
                    label: SharedString::from(label),
                    aria: SharedString::from(aria),
                    background,
                    muted,
                    danger,
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
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(snapshot) = self.model.table() else {
            return Vec::new();
        };
        let can_edit = snapshot.writable;
        let background = self.style.surface;
        let muted = self.style.muted;
        let danger = self.style.danger;

        rows.map(|display_row| {
            let data_row = self.model.data_row(display_row);
            let value = self.model.axis_value(oxtuner_core::AxisKind::Y, data_row);
            let label = format_value(value, usize::from(snapshot.y_axis.digits));
            let units = axis_units(&snapshot.y_axis);
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
                // cells. The bin index is the data row: on a flipped axis the
                // top displayed row is the highest bin.
                if can_edit && event.click_count >= 2 {
                    this.model.clear_action();
                    if this
                        .model
                        .begin_axis_edit(oxtuner_core::AxisKind::Y, data_row)
                    {
                        cx.notify();
                    }
                }
                cx.stop_propagation();
                cx.emit(TableEvent::SelectionChanged);
            });

            RowHeader {
                id: ("row-header", display_row).into(),
                label: SharedString::from(label),
                aria: SharedString::from(aria),
                background,
                muted,
                danger,
                editing: header_editing,
                on_press: Box::new(on_press),
            }
            .into_any_element()
        })
        .collect()
    }
}

impl Render for Table {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(table) = self.model.table().cloned() else {
            return div().size_full().into_any_element();
        };

        let rows = table.rows;
        let columns = table.cols;
        let minimum = table.minimum;
        let maximum = table.maximum;
        let corner_label = corner_caption(&table.x_axis, &table.y_axis);
        let selection = self.model.selection();
        let editing = self.model.editing().cloned();
        let offset = self.body_scroll.offset();
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

        let style = self.style;
        let flipped = self.model.flip_y();

        let column_headers = self.render_column_headers(first_column..last_column, cx);
        let row_headers = self.render_row_headers(first_row..last_row, cx);

        let body_rows = (first_row..last_row)
            .map(|display_row| {
                let data_row = self.model.data_row(display_row);
                let cells = (first_column..last_column)
                    .map(|column| {
                        self.render_cell(
                            data_row,
                            column,
                            selection,
                            editing.as_ref(),
                            minimum,
                            maximum,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>();
                h_flex()
                    .gap(px(CELL_GAP))
                    .children(cells)
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let corner = div()
            .w(px(AXIS_WIDTH))
            .h(px(AXIS_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(style.surface)
            .text_xs()
            .text_color(style.muted)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_center()
                    .child(corner_label),
            );

        let header_band = h_flex()
            .h(px(AXIS_HEIGHT))
            .flex_shrink_0()
            .gap(px(CELL_GAP))
            .bg(style.border)
            .child(corner)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .bg(style.background)
                    .child(
                        div()
                            .relative()
                            .left(offset.x)
                            .w(body_width)
                            .h(px(AXIS_HEIGHT))
                            .bg(style.border)
                            .child(
                                h_flex()
                                    .absolute()
                                    .left(px(first_column as f32 * step_x))
                                    .top(px(0.))
                                    .gap(px(CELL_GAP))
                                    .children(column_headers),
                            ),
                    ),
            );

        // The selection reads as one spreadsheet-style outline around the
        // whole range rather than a border on every cell, and being a plain
        // frame it leaves the cells underneath fully clickable.
        let (top, left, bottom, right) = selection.bounds();
        let top_display = self.model.display_row(top);
        let bottom_display = self.model.display_row(bottom);
        let first_display = top_display.min(bottom_display);
        let rows_span = (top_display.max(bottom_display) - first_display + 1) as f32;
        let selection_frame = div()
            .id("selection-frame")
            .test_support()
            .absolute()
            .left(px(left as f32 * step_x))
            .top(px(first_display as f32 * step_y))
            .w(px(
                (right - left + 1) as f32 * CELL_WIDTH + (right - left) as f32 * CELL_GAP
            ))
            .h(px(rows_span * CELL_HEIGHT + (rows_span - 1.) * CELL_GAP))
            .border_1()
            .border_color(style.foreground);

        let body_band = h_flex()
            // The band takes exactly the table's height and shrinks only when
            // the editor cannot show it all. Growing to fill the viewport is
            // what pulled the column band away from the last row.
            .h(body_height)
            .flex_shrink_1()
            .min_h_0()
            .gap(px(CELL_GAP))
            .bg(style.border)
            .child(
                div()
                    .w(px(AXIS_WIDTH))
                    .h_full()
                    .flex_shrink_0()
                    .overflow_hidden()
                    .bg(style.background)
                    .child(
                        div()
                            .relative()
                            .top(offset.y)
                            .w(px(AXIS_WIDTH))
                            .h(body_height)
                            .bg(style.border)
                            .child(
                                v_flex()
                                    .absolute()
                                    .left(px(0.))
                                    .top(px(first_row as f32 * step_y))
                                    .w(px(AXIS_WIDTH))
                                    .gap(px(CELL_GAP))
                                    .children(row_headers),
                            ),
                    ),
            )
            .child(
                div()
                    .id("table-body")
                    .size_full()
                    .min_w_0()
                    .bg(style.background)
                    .overflow_scroll()
                    .track_scroll(&self.body_scroll)
                    .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                    .child(
                        div()
                            .relative()
                            .w(body_width)
                            .h(body_height)
                            .bg(style.border)
                            .child(
                                v_flex()
                                    .absolute()
                                    .left(px(first_column as f32 * step_x))
                                    .top(px(first_row as f32 * step_y))
                                    .gap(px(CELL_GAP))
                                    .children(body_rows),
                            )
                            // Painted after the cells so the outline sits on
                            // top of them; being a plain frame it leaves the
                            // cells underneath fully clickable.
                            .child(selection_frame),
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
                move |bounds, _, window, cx| {
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
        let focused = self.focus.is_focused(window);

        // The column band follows the low end of the Y axis, which is the
        // bottom by default (chart orientation) and the top when flipped.
        let mut bands = v_flex().relative().size_full().min_h_0().child(input);
        if flipped {
            bands = bands.child(header_band).child(body_band);
        } else {
            bands = bands.child(body_band).child(header_band);
        }

        div()
            .id("table")
            .size_full()
            .min_h_0()
            .p_3()
            .bg(style.background)
            .text_color(style.foreground)
            .role(Role::Grid)
            .aria_label(aria)
            .track_focus(&self.focus)
            // The focused border is the keyboard focus indicator for the whole
            // grid; the cursor cell carries the finer highlight.
            .when(focused, |this| {
                this.rounded(style.radius)
                    .border_1()
                    .border_color(style.accent)
            })
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _, cx| this.handle_key(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
            )
            .child(bands)
            .into_any_element()
    }
}

/// Formats a table value with the definition's digits.
fn format_value(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

/// The corner label names both axes with their units: `MAP (kPa) / RPM (rpm)`.
fn corner_caption(
    x_axis: &oxtuner_core::AxisSummary,
    y_axis: &oxtuner_core::AxisSummary,
) -> String {
    format!("{} / {}", axis_caption(y_axis), axis_caption(x_axis))
}

/// An axis name with its units, for the corner and the editor header.
fn axis_caption(axis: &oxtuner_core::AxisSummary) -> String {
    if axis.units.is_empty() {
        axis.label.clone()
    } else {
        format!("{} ({})", axis.label, axis.units)
    }
}

/// The units suffix for an axis value label: ` kPa`, or nothing when unitless.
fn axis_units(axis: &oxtuner_core::AxisSummary) -> String {
    if axis.units.is_empty() {
        String::new()
    } else {
        format!(" {}", axis.units)
    }
}

fn is_edit_start(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, '-' | '+' | '*' | '/' | '=')
}
