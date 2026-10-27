//! The table grid view: a `div` grid over [`TableModel`] (ADR-0013).
//!
//! The grid never writes tune values. A committed edit becomes a
//! [`TableGridEvent::EditRequested`], the owner sends it to the core, and the
//! echoed [`oxtuner_core::TableSummary`] clears the optimistic value. That keeps
//! the core the only owner of the tune while the cell still changes in the same
//! frame the key was pressed.
//!
//! Headers are frozen: the body scrolls under fixed header bands shifted by the
//! body's scroll offset. The column band follows the low end of the Y axis: at
//! the bottom by default and at the top when the axis is flipped.

use std::sync::Arc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::scroll::{ScrollableElement as _, ScrollbarAxis};
use gpui_kit::{
    App, ClipboardItem, Context, ElementId, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, Pixels, Render, Rgba, Role, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px, rgb,
};

use oxtuner_core::{CellEdit, TableSummary};

use super::model::{CellPos, Selection, TableModel};

/// Width of one cell, in pixels.
pub const CELL_WIDTH: f32 = 52.0;
/// Height of one cell, in pixels.
pub const CELL_HEIGHT: f32 = 24.0;
/// Width of the row-header column.
pub const AXIS_WIDTH: f32 = 72.0;
/// Height of the column-header band.
pub const AXIS_HEIGHT: f32 = 24.0;
/// The grid line between cells, drawn as a gap rather than per-cell borders.
pub const CELL_GAP: f32 = 1.0;

/// What the grid asks its owner to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TableGridEvent {
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
pub(crate) struct RealtimePoint {
    /// Definition row.
    pub row: usize,
    /// Column.
    pub column: usize,
    /// The live value, for a badge.
    pub value: f64,
}

/// The tune table grid.
pub(crate) struct TableGrid {
    model: TableModel,
    focus: FocusHandle,
    body_scroll: ScrollHandle,
    dragging: bool,
    realtime: Option<RealtimePoint>,
}

impl EventEmitter<TableGridEvent> for TableGrid {}

impl Focusable for TableGrid {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TableGrid {
    /// An empty grid.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            model: TableModel::new(),
            focus: cx.focus_handle(),
            body_scroll: ScrollHandle::new(),
            dragging: false,
            realtime: None,
        }
    }

    /// The model, for status lines and tests.
    pub(crate) fn model(&self) -> &TableModel {
        &self.model
    }

    /// Replaces the snapshot and repaints.
    pub(crate) fn set_table(&mut self, table: TableSummary, cx: &mut Context<Self>) {
        self.model.set_table(table);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Flips the displayed Y axis.
    pub(crate) fn set_flip_y(&mut self, flip: bool, cx: &mut Context<Self>) {
        self.model.set_flip_y(flip);
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Drops optimistic values after the core rejected an edit.
    pub(crate) fn reject_pending(&mut self, cx: &mut Context<Self>) {
        self.model.reject_pending();
        cx.notify();
    }

    /// Drops the table, keeping the Y-axis preference.
    pub(crate) fn clear_table(&mut self, cx: &mut Context<Self>) {
        self.model.clear_table();
        cx.notify();
    }

    /// Sets the realtime operating point, or clears it.
    #[allow(
        dead_code,
        reason = "the realtime pipeline is not connected yet; the overlay slot is ready"
    )]
    pub(crate) fn set_realtime(&mut self, point: Option<RealtimePoint>, cx: &mut Context<Self>) {
        self.realtime = point;
        cx.notify();
    }

    /// Copies the selection as TSV.
    pub(crate) fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.model.copy_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        cx.notify();
    }

    /// Pastes TSV at the selection's top-left corner.
    pub(crate) fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
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
                _ => {
                    if let Some(text) = &event.keystroke.key_char {
                        for character in text.chars() {
                            self.model.push_axis_char(character);
                        }
                    }
                    cx.notify();
                }
            }
            cx.emit(TableGridEvent::SelectionChanged);
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
                _ => {
                    if let Some(text) = &event.keystroke.key_char {
                        for character in text.chars() {
                            self.model.push_char(character);
                        }
                    }
                }
            }
            cx.emit(TableGridEvent::SelectionChanged);
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
            _ => {
                if let Some(text) = &event.keystroke.key_char
                    && let Some(first) = text.chars().next()
                    && is_edit_start(first)
                {
                    self.model.begin_edit(Some(first));
                }
            }
        }

        self.ensure_cursor_visible();
        cx.emit(TableGridEvent::SelectionChanged);
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
        cx.emit(TableGridEvent::EditRequested { name, edits });
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
        cx.emit(TableGridEvent::AxisEditRequested {
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
        editing: Option<&super::model::Editing>,
        minimum: f64,
        maximum: f64,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let cell = CellPos {
            row: data_row,
            column,
        };
        let value = self.model.value(cell);
        let selected = selection.contains(cell);
        let is_cursor = selection.cursor == cell;
        let editing = editing.filter(|editing| editing.cell == cell);

        let theme = cx.theme();
        let danger = theme.danger;
        let warning = theme.warning;
        let info = theme.info;

        let fill = heat_color(minimum, maximum, value);
        let text_color = contrast_text(fill);
        let display = self.model.display(cell).unwrap_or_else(|| Arc::from(""));
        let aria = self.model.aria(cell).unwrap_or_else(|| Arc::from(""));

        let mut element = div()
            .id(ElementId::from((
                "table-cell",
                data_row * self.model.columns() + column,
            )))
            .test_support()
            .relative()
            .w(px(CELL_WIDTH))
            .h(px(CELL_HEIGHT))
            .flex()
            .items_center()
            .justify_end()
            .px_1()
            .bg(fill)
            .text_xs()
            .text_color(text_color)
            .role(Role::GridCell)
            .aria_label(SharedString::from(aria))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
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
                    cx.emit(TableGridEvent::SelectionChanged);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.dragging && event.pressed_button == Some(MouseButton::Left) {
                    this.model.extend_to(cell);
                    cx.emit(TableGridEvent::SelectionChanged);
                    cx.notify();
                }
            }));

        if let Some(editing) = editing {
            element = element
                .border_2()
                .border_color(if editing.invalid { danger } else { text_color })
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(1.))
                        .child(editing.text.clone())
                        .child(div().w(px(1.)).h(px(12.)).bg(text_color)),
                );
        } else {
            element = element.child(SharedString::from(display));
        }

        if is_cursor {
            element = element.border_2().border_color(text_color);
        } else if selected {
            element = element.border_1().border_color(text_color.opacity(0.75));
        }

        if let Some(color) = decoration(
            self.model.state(cell),
            self.model.changed(cell),
            danger,
            warning,
            info,
        ) {
            element = element.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(2.))
                    .w(px(3.))
                    .h(px(3.))
                    .rounded_full()
                    .border_1()
                    .border_color(text_color)
                    .bg(color),
            );
        }

        if self
            .realtime
            .is_some_and(|point| point.row == data_row && point.column == column)
        {
            element = element.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .right(px(2.))
                    .w(px(5.))
                    .h(px(5.))
                    .rounded_full()
                    .border_1()
                    .border_color(text_color)
                    .bg(info),
            );
        }

        element.into_any_element()
    }

    fn render_column_headers(
        &self,
        columns: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let snapshot = self.model.table().cloned();
        let Some(snapshot) = snapshot else {
            return Vec::new();
        };
        let theme = cx.theme();
        let background = theme.group_box;
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let can_edit = snapshot.writable;

        columns
            .map(|column| {
                let value = self.model.axis_value(oxtuner_core::AxisKind::X, column);
                let label = format_value(value, usize::from(snapshot.x_axis.digits));
                let units = axis_units(&snapshot.x_axis);
                let editing = self.model.axis_editing().filter(|editing| {
                    editing.target.axis == oxtuner_core::AxisKind::X
                        && editing.target.index == column
                });
                let invalid = editing.as_ref().is_some_and(|editing| editing.invalid);
                let text = editing.map_or_else(
                    || SharedString::from(label.clone()),
                    |editing| SharedString::from(editing.text.clone()),
                );

                let mut element = div()
                    .id(("column-header", column))
                    .test_support()
                    .w(px(CELL_WIDTH))
                    .h(px(AXIS_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(background)
                    .text_xs()
                    .text_color(muted)
                    .role(Role::ColumnHeader)
                    .aria_label(format!("{label}{units} {}", snapshot.x_axis.label))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if can_edit {
                                this.model.clear_action();
                                if this
                                    .model
                                    .begin_axis_edit(oxtuner_core::AxisKind::X, column)
                                {
                                    cx.notify();
                                }
                            }
                            cx.stop_propagation();
                            cx.emit(TableGridEvent::SelectionChanged);
                        }),
                    )
                    .child(text);
                if invalid {
                    element = element.border_1().border_color(danger);
                } else if editing.is_some() {
                    element = element.border_1().border_color(muted);
                }
                element.into_any_element()
            })
            .collect()
    }

    fn render_row_headers(
        &self,
        rows: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let snapshot = self.model.table().cloned();
        let Some(snapshot) = snapshot else {
            return Vec::new();
        };
        let theme = cx.theme();
        let background = theme.group_box;
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let can_edit = snapshot.writable;

        rows.map(|display_row| {
            let data_row = self.model.data_row(display_row);
            let value = self.model.axis_value(oxtuner_core::AxisKind::Y, data_row);
            let label = format_value(value, usize::from(snapshot.y_axis.digits));
            let units = axis_units(&snapshot.y_axis);
            let editing = self.model.axis_editing().filter(|editing| {
                editing.target.axis == oxtuner_core::AxisKind::Y && editing.target.index == data_row
            });
            let invalid = editing.as_ref().is_some_and(|editing| editing.invalid);
            let text = editing.map_or_else(
                || SharedString::from(label.clone()),
                |editing| SharedString::from(editing.text.clone()),
            );

            let mut element = div()
                .id(("row-header", display_row))
                .test_support()
                .w(px(AXIS_WIDTH))
                .h(px(CELL_HEIGHT))
                .flex()
                .items_center()
                .justify_end()
                .px_2()
                .bg(background)
                .text_xs()
                .text_color(muted)
                .role(Role::RowHeader)
                .aria_label(format!("{label}{units} {}", snapshot.y_axis.label))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        if can_edit {
                            this.model.clear_action();
                            // The bin index is the data row: on a flipped axis
                            // the top displayed row is the highest bin.
                            if this
                                .model
                                .begin_axis_edit(oxtuner_core::AxisKind::Y, data_row)
                            {
                                cx.notify();
                            }
                        }
                        cx.stop_propagation();
                        cx.emit(TableGridEvent::SelectionChanged);
                    }),
                )
                .child(text);
            if invalid {
                element = element.border_1().border_color(danger);
            } else if editing.is_some() {
                element = element.border_1().border_color(muted);
            }
            element.into_any_element()
        })
        .collect()
    }
}

impl Render for TableGrid {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(table) = self.model.table() else {
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

        let theme = cx.theme();
        let background = theme.background;
        let foreground = theme.foreground;
        let border = theme.border;
        let group_box = theme.group_box;
        let muted = theme.muted_foreground;
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
                        .into_any_element()
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
            .bg(group_box)
            .text_xs()
            .text_color(muted)
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
            .bg(border)
            .child(corner)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .bg(background)
                    .child(
                        div()
                            .relative()
                            .left(offset.x)
                            .w(body_width)
                            .h(px(AXIS_HEIGHT))
                            .bg(border)
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

        let body_band = h_flex()
            // The band takes exactly the table's height and shrinks only when
            // the editor cannot show it all. Growing to fill the viewport is
            // what pulled the column band away from the last row.
            .h(body_height)
            .flex_shrink_1()
            .min_h_0()
            .gap(px(CELL_GAP))
            .bg(border)
            .child(
                div()
                    .w(px(AXIS_WIDTH))
                    .h_full()
                    .flex_shrink_0()
                    .overflow_hidden()
                    .bg(background)
                    .child(
                        div()
                            .relative()
                            .top(offset.y)
                            .w(px(AXIS_WIDTH))
                            .h(body_height)
                            .bg(border)
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
                // The frame does not scroll; it only hosts the overlay
                // scrollbar. Drawn inside the scroll area the bar would travel
                // with the cells instead of staying on the edge.
                div()
                    .id("table-body-frame")
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .id("table-body")
                            .size_full()
                            .bg(background)
                            .overflow_scroll()
                            .track_scroll(&self.body_scroll)
                            .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                            .child(
                                div()
                                    .relative()
                                    .w(body_width)
                                    .h(body_height)
                                    .bg(border)
                                    .child(
                                        v_flex()
                                            .absolute()
                                            .left(px(first_column as f32 * step_x))
                                            .top(px(first_row as f32 * step_y))
                                            .gap(px(CELL_GAP))
                                            .children(body_rows),
                                    ),
                            ),
                    )
                    // The scrollbar tracks the same handle the frozen headers
                    // read, so the thumbs and the header offset cannot drift.
                    .scrollbar(&self.body_scroll, ScrollbarAxis::Both),
            );

        // The column band follows the low end of the Y axis, which is the
        // bottom by default (chart orientation) and the top when flipped.
        let root = v_flex()
            .id("table-grid")
            .size_full()
            .min_h_0()
            .p_3()
            .bg(background)
            .text_color(foreground)
            .role(Role::Grid)
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _, cx| this.handle_key(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui_kit::MouseUpEvent, _, _| this.dragging = false),
            );
        if flipped {
            root.child(header_band).child(body_band).into_any_element()
        } else {
            root.child(body_band).child(header_band).into_any_element()
        }
    }
}

/// The range of cells that can be seen, with one row/column of overscan.
///
/// Only visible cells are turned into elements. A 32×32 table is wider than the
/// editor, and a 128×128 one would otherwise build sixteen thousand `div`s.
fn visible_range(offset: Pixels, viewport: Pixels, step: f32, count: usize) -> (usize, usize) {
    if count == 0 {
        return (0, 0);
    }
    let viewport = f32::from(viewport);
    if viewport <= 0.0 {
        return (0, count);
    }

    let offset = f32::from(offset);
    let first = ((-offset / step).floor().max(0.0) as usize).saturating_sub(1);
    let last = (((-offset + viewport) / step).ceil().max(0.0) as usize + 1).min(count);
    (first.min(count), last.max(first).min(count))
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

/// The heatmap's data palette: a saturated blue → cyan → green → yellow →
/// orange → red ramp.
///
/// These are data colours, not interface chrome: the value is the datum and its
/// colour is the encoding, so they stay stable across light and dark themes.
/// The stops are picked for chroma rather than pastel tints so adjacent values
/// stay distinguishable, and the blend between them is linear — easing pushed
/// the midpoints towards the same washed-out middle and made neighbours look
/// alike.
const HEAT_STOPS: [u32; 7] = [
    0x2563EB, // blue
    0x0891B2, // cyan
    0x16A34A, // green
    0x84CC16, // lime
    0xEAB308, // yellow
    0xF97316, // orange
    0xDC2626, // red
];

fn heat_color(minimum: f64, maximum: f64, value: f64) -> Hsla {
    let span = maximum - minimum;
    if span <= f64::EPSILON {
        // Every cell is the same value: show the middle of the scale.
        return Hsla::from(rgb(HEAT_STOPS[HEAT_STOPS.len() / 2]));
    }

    let t = ((value - minimum) / span).clamp(0.0, 1.0) as f32;
    let scaled = t * (HEAT_STOPS.len() - 1) as f32;
    let index = (scaled.floor() as usize).min(HEAT_STOPS.len() - 2);
    let ratio = scaled - index as f32;
    let low = Hsla::from(rgb(HEAT_STOPS[index]));
    let high = Hsla::from(rgb(HEAT_STOPS[index + 1]));
    low.blend(high.opacity(ratio))
}

/// Black or white text, whichever reads on the heat colour.
fn contrast_text(fill: Hsla) -> Hsla {
    let rgba = Rgba::from(fill);
    let luminance = 0.299 * rgba.r + 0.587 * rgba.g + 0.114 * rgba.b;
    if luminance > 0.55 {
        Hsla::from(rgb(0x111418))
    } else {
        Hsla::from(rgb(0xFFFFFF))
    }
}

/// The write-state dot for one cell, or `None` when the cell needs no mark.
fn decoration(
    state: oxtuner_core::CellState,
    changed: bool,
    danger: Hsla,
    warning: Hsla,
    info: Hsla,
) -> Option<Hsla> {
    use oxtuner_core::CellState;
    match state {
        CellState::WriteFailed | CellState::Conflict => Some(danger),
        CellState::InRam => Some(info),
        CellState::LocalDirty => Some(warning),
        CellState::Synced | CellState::Unknown | CellState::NotApplicable => {
            changed.then_some(warning)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb_of(color: Hsla) -> (f32, f32, f32) {
        let rgba = Rgba::from(color);
        (rgba.r, rgba.g, rgba.b)
    }

    #[test]
    fn the_heatmap_follows_the_saturated_gradient() {
        let blue = rgb_of(heat_color(0.0, 100.0, 0.0));
        assert!((blue.0 - 0x25 as f32 / 255.0).abs() < 1e-3);
        assert!((blue.1 - 0x63 as f32 / 255.0).abs() < 1e-3);
        assert!((blue.2 - 0xEB as f32 / 255.0).abs() < 1e-3);
        let red = rgb_of(heat_color(0.0, 100.0, 100.0));
        assert!((red.0 - 0xDC as f32 / 255.0).abs() < 1e-3);
        assert!((red.1 - 0x26 as f32 / 255.0).abs() < 1e-3);
        assert!((red.2 - 0x26 as f32 / 255.0).abs() < 1e-3);
        let middle = rgb_of(heat_color(0.0, 100.0, 50.0));
        assert!(
            (middle.0 - 0x84 as f32 / 255.0).abs() < 1e-3
                && (middle.1 - 0xCC as f32 / 255.0).abs() < 1e-3
                && (middle.2 - 0x16 as f32 / 255.0).abs() < 1e-3,
            "halfway sits on the middle stop, got {middle:?}"
        );
        let constant = rgb_of(heat_color(5.0, 5.0, 5.0));
        assert!(
            (constant.0 - middle.0).abs() < 1e-6
                && (constant.1 - middle.1).abs() < 1e-6
                && (constant.2 - middle.2).abs() < 1e-6,
            "a constant table sits on the middle stop, got {constant:?}"
        );
    }

    #[test]
    fn the_heatmap_stays_saturated_between_stops() {
        // A blend between two stops must not collapse to grey: the strongest
        // channel and the weakest must stay far apart across the whole scale.
        for step in 0..=100 {
            let color = rgb_of(heat_color(0.0, 100.0, f64::from(step)));
            let high = color.0.max(color.1).max(color.2);
            let low = color.0.min(color.1).min(color.2);
            assert!(
                high - low > 0.18,
                "value {step} blends to a flat colour {color:?}"
            );
        }
    }

    #[test]
    fn neighbouring_shades_change_gradually() {
        let step = 2.0;
        let mut previous = rgb_of(heat_color(0.0, 100.0, 0.0));
        let mut value = step;
        while value <= 100.0 {
            let current = rgb_of(heat_color(0.0, 100.0, value));
            let jump = (current.0 - previous.0).abs()
                + (current.1 - previous.1).abs()
                + (current.2 - previous.2).abs();
            assert!(
                jump < 0.45,
                "a {step} point step at {value} jumps {jump:.3}, which reads as banding"
            );
            previous = current;
            value += step;
        }
    }

    #[test]
    fn text_contrast_flips_at_the_bright_end_of_the_scale() {
        let on_blue = Rgba::from(contrast_text(heat_color(0.0, 100.0, 0.0)));
        assert!(on_blue.r > 0.9 && on_blue.g > 0.9 && on_blue.b > 0.9);
        let on_yellow = Rgba::from(contrast_text(heat_color(0.0, 100.0, 60.0)));
        assert!(on_yellow.r < 0.2 && on_yellow.g < 0.2);
    }
}
