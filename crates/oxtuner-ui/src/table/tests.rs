//! Headless interaction tests for the table grid.
//!
//! The model's rules are unit-tested in [`super::model`]; these tests drive the
//! real view through pointer and keyboard events, including the frozen headers,
//! which are a layout result rather than a model property.

use std::{cell::RefCell, rc::Rc, time::Instant};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, ElementId, Entity, Focusable as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, TestAppContext, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use oxtuner_core::{AxisKind, AxisSummary, CellState, TableSummary};

use super::RealtimePoint;
use super::geometry::{AXIS_LABEL_GAP, AXIS_LABEL_HEIGHT, CELL_GAP, CELL_HEIGHT, CELL_WIDTH};
use super::model::{AxisTarget, CellPos};
use super::{Table, TableEvent};

struct Host {
    grid: Entity<Table>,
    _events: Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.grid.clone())
    }
}

fn table(rows: usize, columns: usize) -> TableSummary {
    let cells = (0..rows * columns).map(|value| value as f64).collect();
    TableSummary {
        id: "veTableTbl".to_owned(),
        name: "veTable".to_owned(),
        title: "VE Table".to_owned(),
        cols: columns,
        rows,
        digits: 0,
        units: "%".to_owned(),
        cells,
        states: vec![CellState::Unknown; rows * columns],
        changed: vec![false; rows * columns],
        x_axis: AxisSummary {
            name: "rpmBins".to_owned(),
            label: "RPM".to_owned(),
            units: "rpm".to_owned(),
            channel: Some("rpm".to_owned()),
            digits: 0,
            minimum: Some(0.0),
            maximum: Some(10_000.0),
            values: (0..columns).map(|value| value as f64 * 500.0).collect(),
        },
        y_axis: AxisSummary {
            name: "mapBins".to_owned(),
            label: "MAP".to_owned(),
            units: "kPa".to_owned(),
            channel: Some("map".to_owned()),
            digits: 0,
            minimum: Some(0.0),
            maximum: Some(255.0),
            values: (0..rows).map(|value| value as f64 * 10.0).collect(),
        },
        minimum: 0.0,
        maximum: (rows * columns) as f64,
        cell_minimum: Some(0.0),
        cell_maximum: Some(255.0),
        writable: true,
        curve: None,
    }
}

fn open(
    cx: &mut TestAppContext,
    rows: usize,
    columns: usize,
) -> (AnyWindowHandle, Entity<Table>, Rc<RefCell<Vec<TableEvent>>>) {
    cx.update(gpui_kit::init);
    let events = Rc::new(RefCell::new(Vec::new()));
    let (handle, grid) = cx.update(|cx| {
        let grid = cx.new(Table::new);
        let sink = events.clone();
        let subscription = cx.subscribe(&grid, move |_, event: &TableEvent, _| {
            sink.borrow_mut().push(event.clone());
        });
        let host = cx.new(|_| Host {
            grid: grid.clone(),
            _events: subscription,
        });
        let (handle, _) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1000.), px(800.)),
                ))),
                ..Default::default()
            },
            cx,
            move |_, _| host,
        )
        .expect("open test window");
        grid.update(cx, |grid, cx| grid.set_table(table(rows, columns), cx));
        (handle, grid)
    });

    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let focus = cx.update(|cx| grid.read(cx).focus_handle(cx));
    cx.update_window(handle, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    (handle, grid, events)
}

fn cell(row: usize, column: usize, columns: usize) -> ElementId {
    ElementId::from(("table-cell", row * columns + column))
}

fn row_header(display_row: usize) -> ElementId {
    ElementId::from(("row-header", display_row))
}

fn column_header(column: usize) -> ElementId {
    ElementId::from(("column-header", column))
}

fn column_highlight(column: usize) -> ElementId {
    ElementId::from(("column-header-highlight", column))
}

fn row_highlight(display_row: usize) -> ElementId {
    ElementId::from(("row-header-highlight", display_row))
}

fn selected(grid: &Entity<Table>, cx: &mut TestAppContext) -> CellPos {
    cx.update(|cx| grid.read(cx).model().selection().cursor)
}

#[gpui_kit::test]
fn clicking_a_cell_selects_it(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.click(cell(3, 4, 8), cx);
    })
    .unwrap();

    assert_eq!(selected(&grid, cx), CellPos { row: 3, column: 4 });
}

#[gpui_kit::test]
fn a_selected_cell_washes_its_column_and_row_bins(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(cell(3, 4, 8), cx);
    })
    .unwrap();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);

        for column in 0..8 {
            assert_eq!(
                window.try_find(column_highlight(column)).is_some(),
                column == 4,
                "only the selected column's bin is washed"
            );
        }

        // Data row 3 is drawn at display row 4 on the default grid.
        for display_row in 0..8 {
            assert_eq!(
                window.try_find(row_highlight(display_row)).is_some(),
                display_row == 4,
                "only the selected row's bin is washed"
            );
        }

        let bin = window.find(column_header(4)).bounds();
        let wash = window.find(column_highlight(4)).bounds();
        assert_eq!(wash.origin, bin.origin, "the wash covers the whole bin");
        assert_eq!(
            wash.size, bin.size,
            "the wash is an overlay, so the bin keeps its size"
        );

        let value = window.find(cell(3, 4, 8)).bounds();
        assert_eq!(
            f32::from(value.origin.x),
            f32::from(bin.origin.x),
            "the wash never moves the bin off its column"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_range_leaves_the_bins_plain(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(cell(1, 1, 8), cell(5, 6, 8), cx);
    })
    .unwrap();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);

        for column in 0..8 {
            assert!(
                window.try_find(column_highlight(column)).is_none(),
                "a range must not wash the column bins"
            );
        }

        for display_row in 0..8 {
            assert!(
                window.try_find(row_highlight(display_row)).is_none(),
                "a range must not wash the row bins"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_range_washes_every_bin_it_spans_when_asked(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);
    cx.update(|cx| {
        grid.update(cx, |grid, cx| grid.set_header_wash_for_ranges(true, cx));
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(cell(1, 1, 8), cell(5, 6, 8), cx);
    })
    .unwrap();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);

        for column in 0..8 {
            assert_eq!(
                window.try_find(column_highlight(column)).is_some(),
                (1..=6).contains(&column),
                "the wash spans the selected columns"
            );
        }

        // Data rows 1..=5 are drawn at display rows 6..=2 by default.
        for display_row in 0..8 {
            assert_eq!(
                window.try_find(row_highlight(display_row)).is_some(),
                (2..=6).contains(&display_row),
                "the wash spans the selected rows, in display order"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_wash_follows_the_axis_when_it_is_flipped(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);
    cx.update(|cx| grid.update(cx, |grid, cx| grid.set_flip_y(true, cx)));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(cell(3, 4, 8), cx);
    })
    .unwrap();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);

        for display_row in 0..8 {
            assert_eq!(
                window.try_find(row_highlight(display_row)).is_some(),
                display_row == 3,
                "a flipped axis draws data row 3 at display row 3"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_curve_washes_only_the_column_bin(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, 1, 6);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(cell(0, 2, 6), cx);
    })
    .unwrap();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);

        for column in 0..6 {
            assert_eq!(
                window.try_find(column_highlight(column)).is_some(),
                column == 2,
                "a curve still names the selected column"
            );
        }
        assert!(
            window.try_find(row_highlight(0)).is_none(),
            "a curve has no row band, so there is no bin to wash"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_header_bin_edit_starts_on_a_double_click(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(column_header(2), cx);
    })
    .unwrap();
    assert!(
        cx.update(|cx| grid.read(cx).model().axis_editing().is_none()),
        "a single press must not open a bin editor"
    );

    cx.update_window(handle, |_, window, cx| {
        window.double_click(column_header(2), cx);
    })
    .unwrap();

    let target = cx.update(|cx| {
        grid.read(cx)
            .model()
            .axis_editing()
            .map(|editing| editing.target)
    });
    assert_eq!(
        target,
        Some(AxisTarget {
            axis: AxisKind::X,
            index: 2,
        }),
        "a double press opens the bin editor for that column"
    );
}

#[gpui_kit::test]
fn dragging_selects_a_range(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.drag_to(cell(1, 1, 8), cell(5, 6, 8), cx);
    })
    .unwrap();

    let selection = cx.update(|cx| grid.read(cx).model().selection());
    let (top, left, bottom, right) = selection.bounds();
    assert!(
        selection.cell_count() > 1 && top <= 1 && left <= 1 && bottom >= 5 && right >= 6,
        "drag must cover both corners, got {top},{left}..{bottom},{right}"
    );
}

#[gpui_kit::test]
fn the_default_y_axis_puts_the_highest_row_on_top(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(cell(0, 0, 8), cx);
        window.press("up", cx);
    })
    .unwrap();

    assert_eq!(
        selected(&grid, cx),
        CellPos { row: 1, column: 0 },
        "up on the default grid moves toward higher bins"
    );
}

#[gpui_kit::test]
fn a_flipped_y_axis_puts_the_lowest_row_on_top(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);
    cx.update(|cx| {
        grid.update(cx, |grid, cx| grid.set_flip_y(true, cx));
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(cell(0, 0, 8), cx);
        window.press("down", cx);
    })
    .unwrap();

    assert_eq!(
        selected(&grid, cx),
        CellPos { row: 1, column: 0 },
        "down on a flipped grid moves toward higher bins"
    );
}

#[gpui_kit::test]
fn a_taller_table_lays_out_every_row_on_the_frame_it_opens(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 4, 4);

    cx.update(|cx| {
        grid.update(cx, |grid, cx| grid.set_table(table(16, 16), cx));
    });

    // One frame only: the scroll handle still holds the 4x4 body's bounds, so
    // deriving the viewport from it used to skip the lower rows until the
    // pointer moved. Data row 0 is the bottom row of the default display.
    let bottom = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(cell(5, 0, 16))
        })
        .unwrap();

    assert!(
        bottom.is_some(),
        "the whole table must be laid out as soon as it opens"
    );
}

#[gpui_kit::test]
fn typing_an_edit_requests_it_and_shows_it_immediately(cx: &mut TestAppContext) {
    let (handle, grid, events) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.click(cell(2, 3, 8), cx);
        window.press("5", cx);
        window.press("enter", cx);
    })
    .unwrap();

    assert_eq!(
        cx.update(|cx| grid.read(cx).model().value(CellPos { row: 2, column: 3 })),
        5.0,
        "the optimistic value is visible before the core answers"
    );
    let requested = events.borrow().iter().any(|event| {
        matches!(
            event,
            TableEvent::EditRequested { name, edits }
                if name == "veTable"
                    && edits.len() == 1
                    && edits[0].row == 2
                    && edits[0].column == 3
                    && edits[0].value == 5.0
        )
    });
    assert!(requested, "the grid asked the owner to send the edit");
}

#[gpui_kit::test]
fn shift_navigation_widens_the_selection_outline(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(cell(2, 2, 8), cx);
        window.press("shift-right", cx);
        window.render_frame(cx);
    })
    .unwrap();

    // Two selected columns: two cell widths, the gap between them, and the
    // gap the outline bleeds past the range on either side.
    let expected = 2.0 * CELL_WIDTH + 3.0 * CELL_GAP;
    let width = cx
        .update_window(handle, |_, window, _| {
            window.try_find(ElementId::from("selection-frame"))
        })
        .unwrap()
        .map(|frame| f32::from(frame.bounds().size.width));
    let selection = cx.update(|cx| grid.read(cx).model().selection().bounds());
    assert_eq!(
        width,
        Some(expected),
        "the outline follows shift-navigation like a spreadsheet selection"
    );
    assert_eq!(selection, (2, 2, 2, 3), "shift-right extended one column");
}

#[gpui_kit::test]
fn the_selection_draws_one_outline_over_the_range(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(cell(1, 1, 8), cell(3, 4, 8), cx);
        window.render_frame(cx);
    })
    .unwrap();

    // The outline must wrap the selection rectangle exactly, whatever the drag
    // produced — one frame over the range, not a border per cell — and it
    // bleeds one gap past it on each side so it covers the grid line instead
    // of eating into the edge cells.
    let (columns, expected) = cx.update(|cx| {
        let model = grid.read(cx).model();
        let (_, left, _, right) = model.selection().bounds();
        let columns = right - left + 1;
        let span = columns as f32;
        (
            columns,
            span * CELL_WIDTH + (span - 1.) * CELL_GAP + 2. * CELL_GAP,
        )
    });
    assert!(columns > 1, "the drag must select several columns");

    let width = cx
        .update_window(handle, |_, window, _| {
            window.try_find(ElementId::from("selection-frame"))
        })
        .unwrap()
        .map(|frame| f32::from(frame.bounds().size.width));
    assert_eq!(
        width,
        Some(expected),
        "the outline spans the whole selection as one rectangle"
    );
}

#[gpui_kit::test]
fn a_multi_cell_selection_tints_the_whole_range(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.drag_to(cell(1, 1, 8), cell(3, 4, 8), cx);
        window.render_frame(cx);
    })
    .unwrap();

    let (top, left, bottom, right) = cx.update(|cx| grid.read(cx).model().selection().bounds());
    assert!(bottom > top && right > left, "the drag must select a range");

    let cursor = cx.update(|cx| grid.read(cx).model().selection().cursor);

    let (fill, frame, cursor_cell) = cx
        .update_window(handle, |_, window, _| {
            (
                window.find(ElementId::from("selection-fill")).bounds(),
                window.find(ElementId::from("selection-frame")).bounds(),
                window.find(cell(cursor.row, cursor.column, 8)).bounds(),
            )
        })
        .unwrap();

    let columns = (right - left + 1) as f32;
    let rows = (bottom - top + 1) as f32;
    let tint_width = columns * CELL_WIDTH + (columns - 1.) * CELL_GAP;
    let tint_height = rows * CELL_HEIGHT + (rows - 1.) * CELL_GAP;

    assert_eq!(f32::from(fill.size.width), tint_width);
    assert_eq!(f32::from(fill.size.height), tint_height);

    // The cursor cell is part of the range, so the tint covers it too.
    let fill_right = fill.origin.x + fill.size.width;
    let fill_bottom = fill.origin.y + fill.size.height;
    assert!(cursor_cell.origin.x >= fill.origin.x && cursor_cell.origin.x < fill_right);
    assert!(cursor_cell.origin.y >= fill.origin.y && cursor_cell.origin.y < fill_bottom);

    // The outline wraps the tint and bleeds one gap past it on each side, so it
    // covers the grid line instead of eating into the edge cells.
    assert_eq!(
        f32::from(frame.size.width),
        tint_width + 2. * CELL_GAP,
        "the outline wraps the tint"
    );
    assert_eq!(f32::from(frame.size.height), tint_height + 2. * CELL_GAP);
    assert_eq!(
        f32::from(frame.origin.x),
        f32::from(fill.origin.x) - CELL_GAP
    );
    assert_eq!(
        f32::from(frame.origin.y),
        f32::from(fill.origin.y) - CELL_GAP
    );
}

#[gpui_kit::test]
fn a_single_cell_selection_outlines_the_cell_without_an_inset(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(cell(2, 3, 8), cx);
        window.render_frame(cx);
    })
    .unwrap();

    let (fill, frame, value) = cx
        .update_window(handle, |_, window, _| {
            (
                window.try_find(ElementId::from("selection-fill")).is_some(),
                window.find(ElementId::from("selection-frame")).bounds(),
                window.find(cell(2, 3, 8)).bounds(),
            )
        })
        .unwrap();

    assert!(
        !fill,
        "a single cell reads as an outline, not a filled range"
    );
    assert_eq!(
        f32::from(value.origin.x) - f32::from(frame.origin.x),
        CELL_GAP,
        "the outline straddles the grid line outside the cell"
    );
    assert_eq!(
        f32::from(value.origin.y) - f32::from(frame.origin.y),
        CELL_GAP
    );
    assert_eq!(
        f32::from(frame.size.width),
        f32::from(value.size.width) + 2. * CELL_GAP,
        "the outline never eats into the cell it outlines"
    );
    assert_eq!(
        f32::from(frame.size.height),
        f32::from(value.size.height) + 2. * CELL_GAP
    );
}

#[gpui_kit::test]
fn the_corner_is_empty_and_each_axis_names_itself_beside_its_headers(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update(|cx| {
        let model = grid.read(cx).model();
        assert_eq!(
            model.axis_channel(AxisKind::X),
            Some("rpm"),
            "the follower reads the X channel from the table, never a constant"
        );
        assert_eq!(model.axis_channel(AxisKind::Y), Some("map"));
        assert_eq!(
            model.realtime_cell(1_250.0, 25.0),
            Some(CellPos { row: 2, column: 2 }),
            "1250 ties 1000 and 1500, so it lands on the lower bin; 25 rounds to 20"
        );
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);

        let corner = window.find(ElementId::from("table-corner"));
        assert_eq!(corner.label(), None, "the corner must not carry a caption");

        let x_label = window.find(ElementId::from("x-axis-label"));
        assert_eq!(x_label.label(), Some("RPM (rpm)"));
        let y_label = window.find(ElementId::from("y-axis-label"));
        assert_eq!(y_label.label(), Some("MAP (kPa)"));

        let y_bounds = y_label.bounds();
        let header = window.find(row_header(0)).bounds();
        assert!(
            f32::from(header.origin.x)
                - (f32::from(y_bounds.origin.x) + f32::from(y_bounds.size.width))
                >= AXIS_LABEL_GAP,
            "the Y caption sits outside the grid box, left of the row headers"
        );
        assert!(
            f32::from(y_bounds.size.width) < f32::from(header.size.width),
            "the Y caption is a narrow vertical band"
        );

        let column = window.find(column_header(0)).bounds();
        let grid_box = window.find(ElementId::from("table-box")).bounds();
        let gap = f32::from(x_label.bounds().origin.y)
            - (f32::from(grid_box.origin.y) + f32::from(grid_box.size.height));
        assert!(
            (gap - AXIS_LABEL_GAP).abs() < 0.5,
            "the X caption sits outside the grid box, {gap} pt below it"
        );
        assert!(
            f32::from(column.origin.y) + f32::from(column.size.height)
                <= f32::from(grid_box.origin.y) + f32::from(grid_box.size.height),
            "the column band stays inside the grid box"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn keyboard_navigation_scrolls_the_cursor_into_view(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, 32, 32);

    cx.update_window(handle, |_, window, cx| {
        // The default grid starts on the highest bin, so End must travel to
        // the lowest row at the bottom and scroll it into view.
        window.click(cell(31, 0, 32), cx);
        window.press("end", cx);
        window.render_frame(cx);
        assert!(
            window.find(cell(0, 0, 32)).visible(),
            "End scrolls the last displayed row into view and renders it"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_row_headers_stay_aligned_with_their_rows(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        // The selection paints over the cells; the header must not move.
        window.click(cell(3, 2, 8), cx);
        window.render_frame(cx);

        for display_row in 0..8 {
            let data_row = 7 - display_row;
            let header = window.find(row_header(display_row)).bounds();
            let value = window.find(cell(data_row, 0, 8)).bounds();
            assert!(
                (f32::from(header.origin.y) - f32::from(value.origin.y)).abs() < 0.5,
                "row header {display_row} starts at {} but its row starts at {}",
                f32::from(header.origin.y),
                f32::from(value.origin.y)
            );
            assert!(
                (f32::from(header.size.height) - f32::from(value.size.height)).abs() < 0.5,
                "row header {display_row} is {} high but its row is {} high",
                f32::from(header.size.height),
                f32::from(value.size.height)
            );

            let label = window
                .find(row_header(display_row))
                .label()
                .unwrap_or_default()
                .to_owned();
            assert_eq!(label, format!("{} kPa MAP", data_row * 10));
            let cell_label = window
                .find(cell(data_row, 0, 8))
                .label()
                .unwrap_or_default()
                .to_owned();
            assert!(
                cell_label.starts_with(&format!("{} ", data_row * 8)),
                "row {display_row} header {label:?} does not describe cell {cell_label:?}"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_column_headers_stay_aligned_with_their_columns(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.click(cell(3, 2, 8), cx);
        window.render_frame(cx);

        for column in 0..8 {
            let header = window.find(column_header(column)).bounds();
            let value = window.find(cell(0, column, 8)).bounds();
            assert!(
                (f32::from(header.origin.x) - f32::from(value.origin.x)).abs() < 0.5,
                "column header {column} starts at {} but its column starts at {}",
                f32::from(header.origin.x),
                f32::from(value.origin.x)
            );
            assert!(
                (f32::from(header.size.width) - f32::from(value.size.width)).abs() < 0.5,
                "column header {column} is {} wide but its column is {} wide",
                f32::from(header.size.width),
                f32::from(value.size.width)
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn flipped_row_headers_match_the_flipped_rows(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);
    cx.update(|cx| grid.update(cx, |grid, cx| grid.set_flip_y(true, cx)));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        for display_row in 0..8 {
            let data_row = display_row;
            let header = window.find(row_header(display_row)).bounds();
            let value = window.find(cell(data_row, 0, 8)).bounds();
            assert!(
                (f32::from(header.origin.y) - f32::from(value.origin.y)).abs() < 0.5,
                "flipped row header {display_row} does not align with data row {data_row}"
            );

            let label = window
                .find(row_header(display_row))
                .label()
                .unwrap_or_default()
                .to_owned();
            assert_eq!(
                label,
                format!("{} kPa MAP", data_row * 10),
                "a flipped axis starts with the lowest Y value on top"
            );
            let cell_label = window
                .find(cell(data_row, 0, 8))
                .label()
                .unwrap_or_default()
                .to_owned();
            assert!(
                cell_label.starts_with(&format!("{} ", data_row * 8)),
                "flipped row header {label:?} does not describe cell {cell_label:?}"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_column_band_sits_next_to_the_body(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Default: high bins on top, so the lowest data row is last and the
        // column band must hug it instead of drifting to the viewport bottom.
        let header = window.find(column_header(0)).bounds();
        let last_row = window.find(cell(0, 0, 8)).bounds();
        let gap = f32::from(header.origin.y)
            - (f32::from(last_row.origin.y) + f32::from(last_row.size.height));
        assert!(
            (0.0..1.5).contains(&gap),
            "the column band sits {gap} pt below the last row"
        );
    })
    .unwrap();

    cx.update(|cx| grid.update(cx, |grid, cx| grid.set_flip_y(true, cx)));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Flipped: the lowest row is first and the band moves above it.
        let header = window.find(column_header(0)).bounds();
        let first_row = window.find(cell(0, 0, 8)).bounds();
        let gap = f32::from(first_row.origin.y)
            - (f32::from(header.origin.y) + f32::from(header.size.height));
        assert!(
            (0.0..1.5).contains(&gap),
            "the column band sits {gap} pt above the first row when flipped"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_short_table_grows_its_row_for_the_stacked_caption(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 2, 6);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let label = window.find(ElementId::from("y-axis-label")).bounds();
        let grid_box = window.find(ElementId::from("table-box")).bounds();
        let viewport = window.viewport_size();
        assert!(
            f32::from(grid_box.size.height) < f32::from(viewport.height) * 0.5,
            "the border stays around the bands instead of stretching over the free area"
        );
        assert!(
            f32::from(label.size.height) > f32::from(grid_box.size.height),
            "the caption column takes the height the stacked text needs"
        );
        assert!(
            f32::from(label.origin.y) >= 0.0
                && f32::from(label.origin.y) + f32::from(label.size.height)
                    <= f32::from(viewport.height),
            "the caption stays whole and inside the window"
        );
    })
    .unwrap();

    cx.update(|cx| grid.update(cx, |grid, cx| grid.set_flip_y(true, cx)));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let label = window.find(ElementId::from("y-axis-label")).bounds();
        let grid_box = window.find(ElementId::from("table-box")).bounds();
        assert!(
            f32::from(label.size.height) > f32::from(grid_box.size.height),
            "the caption keeps its height when the band moves above the body"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn headers_stay_aligned_when_scrolled_past_the_end(cx: &mut TestAppContext) {
    use gpui_kit::{ScrollDelta, point, px};

    let (handle, _, _) = open(cx, 32, 32);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Wheel past the bottom end: the body clamps there, so the row
        // headers must clamp too instead of sliding past the cells. Scroll
        // one axis at a time: a diagonal gesture only moves its dominant
        // axis.
        window.scroll(
            cell(20, 5, 32),
            ScrollDelta::Pixels(point(px(0.0), px(-5000.0))),
            cx,
        );
        // No extra frame: the scroll helper already rendered once, and that
        // frame must be consistent on its own.

        // The clamped body shows the end, so the last row and its header
        // must both be rendered, exactly aligned.
        let rows: Vec<usize> = (0..32)
            .filter(|display| window.try_find(row_header(*display)).is_some())
            .collect();
        assert!(
            window.try_find(row_header(31)).is_some(),
            "last row header missing after scrolling past the end; rendered rows={rows:?}"
        );
        assert!(
            window.try_find(cell(0, 0, 32)).is_some(),
            "last-row first cell missing after scrolling past the end; rendered rows={rows:?}"
        );
        let header = window.find(row_header(31));
        let value = window.find(cell(0, 0, 32));
        let dy = f32::from(header.bounds().origin.y) - f32::from(value.bounds().origin.y);
        assert!(
            dy.abs() < 1.5,
            "last row header off by {dy} after scrolling past the end"
        );

        // Every other rendered row pair must agree too.
        for display_row in 0..32 {
            if let (Some(header), Some(value)) = (
                window.try_find(row_header(display_row)),
                window.try_find(cell(31 - display_row, 0, 32)),
            ) {
                let dy = f32::from(header.bounds().origin.y) - f32::from(value.bounds().origin.y);
                assert!(dy.abs() < 1.5, "row header {display_row} off by {dy}");
            }
        }

        // Same for the right end: the last column and its header line up.
        window.scroll(
            cell(0, 5, 32),
            ScrollDelta::Pixels(point(px(-5000.0), px(0.0))),
            cx,
        );

        let header = window.find(column_header(31));
        let value = window.find(cell(0, 31, 32));
        let dx = f32::from(header.bounds().origin.x) - f32::from(value.bounds().origin.x);
        assert!(
            dx.abs() < 1.5,
            "last column header off by {dx} after scrolling past the end"
        );

        for column in 0..32 {
            if let (Some(header), Some(value)) = (
                window.try_find(column_header(column)),
                window.try_find(cell(0, column, 32)),
            ) {
                let dx = f32::from(header.bounds().origin.x) - f32::from(value.bounds().origin.x);
                assert!(dx.abs() < 1.5, "column header {column} off by {dx}");
            }
        }

        // And back past the start: the body clamps at zero, so the first
        // row and column must line up again.
        window.scroll(
            cell(0, 26, 32),
            ScrollDelta::Pixels(point(px(5000.0), px(0.0))),
            cx,
        );
        window.scroll(
            cell(0, 5, 32),
            ScrollDelta::Pixels(point(px(0.0), px(5000.0))),
            cx,
        );

        let header = window.find(row_header(0));
        let value = window.find(cell(31, 0, 32));
        let dy = f32::from(header.bounds().origin.y) - f32::from(value.bounds().origin.y);
        assert!(
            dy.abs() < 1.5,
            "first row header off by {dy} after scrolling back"
        );

        let header = window.find(column_header(0));
        let value = window.find(cell(31, 0, 32));
        let dx = f32::from(header.bounds().origin.x) - f32::from(value.bounds().origin.x);
        assert!(
            dx.abs() < 1.5,
            "first column header off by {dx} after scrolling back"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_one_row_table_draws_no_row_band_and_lifts_the_caption(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 1, 6);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(row_header(0)).is_none(),
            "a key/value curve has no row band"
        );
        assert!(
            window.try_find(ElementId::from("table-corner")).is_none(),
            "without a row band there is no corner cell"
        );

        let label = window.find(ElementId::from("y-axis-label")).bounds();
        let grid_box = window.find(ElementId::from("table-box")).bounds();
        assert!(
            (f32::from(label.size.height) - AXIS_LABEL_HEIGHT).abs() < 0.5,
            "the curve caption is one ordinary line, not a stacked column"
        );
        assert!(
            f32::from(label.origin.y) + f32::from(label.size.height)
                <= f32::from(grid_box.origin.y),
            "the curve caption sits above its values"
        );

        let value = window.find(cell(0, 0, 6)).bounds();
        let bins = window.find(column_header(0)).bounds();
        assert!(
            f32::from(value.origin.y) + f32::from(value.size.height) <= f32::from(bins.origin.y),
            "the values sit above their X bins"
        );
    })
    .unwrap();

    cx.update(|cx| grid.update(cx, |grid, cx| grid.set_flip_y(true, cx)));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let label = window.find(ElementId::from("y-axis-label")).bounds();
        let x_label = window.find(ElementId::from("x-axis-label")).bounds();
        let grid_box = window.find(ElementId::from("table-box")).bounds();

        assert!(
            f32::from(x_label.origin.y) + f32::from(x_label.size.height)
                <= f32::from(grid_box.origin.y),
            "flipping moves the X caption above the values"
        );
        assert!(
            f32::from(grid_box.origin.y) + f32::from(grid_box.size.height)
                <= f32::from(label.origin.y),
            "flipping moves the Y caption below the values"
        );

        let value = window.find(cell(0, 0, 6)).bounds();
        let bins = window.find(column_header(0)).bounds();
        assert!(
            f32::from(bins.origin.y) + f32::from(bins.size.height) <= f32::from(value.origin.y),
            "flipping puts the X bins above their values"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn double_clicking_a_bin_hides_the_cell_selection(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.click(cell(3, 2, 8), cx);
        window.render_frame(cx);
        assert!(
            window
                .try_find(ElementId::from("selection-frame"))
                .is_some(),
            "a selected cell draws its outline"
        );
        window.double_click(column_header(2), cx);
    })
    .unwrap();
    assert!(
        cx.update(|cx| grid.read(cx).model().axis_editing().is_some()),
        "double-clicking a bin opens the bin editor"
    );

    cx.update_window(handle, |_, window, cx| {
        assert!(
            window
                .try_find(ElementId::from("selection-frame"))
                .is_none(),
            "the cell range hides while a bin owns the focus"
        );
        window.press("escape", cx);
    })
    .unwrap();

    cx.update_window(handle, |_, window, _| {
        assert!(
            window
                .try_find(ElementId::from("selection-frame"))
                .is_some(),
            "cancelling the bin edit returns the focus to the cells"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn clicking_the_corner_dismisses_the_range(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.click(cell(3, 2, 8), cx);
        window.press(&shortcut("a"), cx);
    })
    .unwrap();
    assert_eq!(
        cx.update(|cx| grid.read(cx).model().selection().cell_count()),
        64,
        "select-all covers the whole 8x8 grid"
    );

    cx.update_window(handle, |_, window, cx| {
        window.click(ElementId::from("table-corner"), cx);
    })
    .unwrap();
    assert_eq!(
        cx.update(|cx| grid.read(cx).model().selection().cell_count()),
        1,
        "a chrome click collapses the range back to the cursor"
    );
}

#[gpui_kit::test]
fn single_click_on_another_header_dismisses_the_bin_edit(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 8, 8);

    cx.update_window(handle, |_, window, cx| {
        window.double_click(column_header(2), cx);
    })
    .unwrap();
    assert!(
        cx.update(|cx| grid.read(cx).model().axis_editing().is_some()),
        "double-clicking a bin opens the bin editor"
    );

    // Clicking the same bin keeps typing where it is.
    cx.update_window(handle, |_, window, cx| {
        window.click(column_header(2), cx);
    })
    .unwrap();
    assert!(
        cx.update(|cx| grid.read(cx).model().axis_editing().is_some()),
        "clicking the open bin does not drop its text"
    );

    // Clicking anywhere else dismisses it.
    cx.update_window(handle, |_, window, cx| {
        window.click(column_header(4), cx);
    })
    .unwrap();
    assert!(
        cx.update(|cx| grid.read(cx).model().axis_editing().is_none()),
        "a click outside the open bin dismisses it"
    );
}

/// A keystroke with the platform's command modifier: Cmd on macOS, Ctrl
/// elsewhere. The grid reads `modifiers.platform`, so the test must press the
/// same key a user would.
fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("cmd-{key}")
    } else {
        format!("ctrl-{key}")
    }
}

#[gpui_kit::test]
fn undo_and_redo_keys_write_the_old_and_new_values(cx: &mut TestAppContext) {
    let (handle, grid, events) = open(cx, 8, 8);
    let position = CellPos { row: 2, column: 3 };

    cx.update_window(handle, |_, window, cx| {
        window.click(cell(2, 3, 8), cx);
        window.press("5", cx);
        window.press("enter", cx);
    })
    .unwrap();
    assert_eq!(cx.update(|cx| grid.read(cx).model().value(position)), 5.0);

    cx.update_window(handle, |_, window, cx| {
        window.press(&shortcut("z"), cx);
    })
    .unwrap();
    assert_eq!(
        cx.update(|cx| grid.read(cx).model().value(position)),
        19.0,
        "undo restores the value the edit replaced"
    );
    let undone = events.borrow().iter().rev().find_map(|event| match event {
        TableEvent::EditRequested { edits, .. } => edits.first().map(|edit| edit.value),
        _ => None,
    });
    assert_eq!(undone, Some(19.0), "undo is sent to the core as an edit");

    cx.update_window(handle, |_, window, cx| {
        window.press(&shortcut("shift-z"), cx);
    })
    .unwrap();
    assert_eq!(
        cx.update(|cx| grid.read(cx).model().value(position)),
        5.0,
        "redo writes the edit again"
    );
    let redone = events.borrow().iter().rev().find_map(|event| match event {
        TableEvent::EditRequested { edits, .. } => edits.first().map(|edit| edit.value),
        _ => None,
    });
    assert_eq!(redone, Some(5.0));
}

#[gpui_kit::test]
fn a_128x128_table_lays_out_only_the_cells_on_screen(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, 128, 128);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // The default orientation draws data row 127 at the top and the first
        // columns at the left. The far corner is outside the viewport.
        assert!(
            window.try_find(cell(127, 0, 128)).is_some(),
            "the top-left cell must be built"
        );
        assert!(
            window.try_find(cell(127, 127, 128)).is_none(),
            "a cell outside the viewport must not be built, or 128x128 builds 16k elements"
        );
    })
    .unwrap();
}

/// 1-minute load average (macOS `sysctl vm.loadavg`), or 0 when unavailable.
fn load_1min() -> f64 {
    let Ok(output) = std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
    else {
        return 0.0;
    };
    String::from_utf8_lossy(&output.stdout)
        .replace(['{', '}'], " ")
        .split_whitespace()
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0)
}

/// The frame budget, with room for a busy desktop so a loaded machine is not
/// reported as a renderer regression. A real regression exceeds both.
fn judged_budget_ms() -> f64 {
    if load_1min() > 3.0 { 16.7 * 2.5 } else { 16.7 }
}

/// ADR-0013's measurable criterion: a 32×32 grid with a moving selection and a
/// live overlay stub must stay inside one 60 fps frame.
#[gpui_kit::test]
fn renders_32x32_within_the_60fps_budget(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 32, 32);
    let frames = 240;
    let mut batch_p95 = Vec::new();
    let mut batch_stats = Vec::new();

    cx.update_window(handle, |_, window, cx| {
        for _ in 0..3 {
            let mut samples = Vec::with_capacity(frames);
            for frame in 0..frames {
                grid.update(cx, |grid, cx| {
                    grid.nudge_selection(cx);
                    grid.set_realtime(
                        Some(RealtimePoint {
                            row: frame % 32,
                            column: (frame * 7) % 32,
                            value: 42.0,
                        }),
                        cx,
                    );
                });
                let started = Instant::now();
                window.render_frame(cx);
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(f64::total_cmp);
            let mean = samples.iter().sum::<f64>() / samples.len() as f64;
            let p50 = samples[samples.len() / 2];
            let p95 = samples[samples.len() * 95 / 100];
            let p99 = samples[samples.len() * 99 / 100];
            let max = samples[samples.len() - 1];
            batch_stats.push(format!(
                "mean {mean:.3}, p50 {p50:.3}, p95 {p95:.3}, p99 {p99:.3}, max {max:.3}"
            ));
            batch_p95.push(p95);
        }
    })
    .unwrap();

    for (batch, stats) in batch_stats.iter().enumerate() {
        eprintln!("table grid perf 32x32 batch {batch}: {stats} ms");
    }
    batch_p95.sort_by(f64::total_cmp);
    let median_p95 = batch_p95[1];
    let allowed = judged_budget_ms();
    eprintln!(
        "table grid perf 32x32: median batch p95 {median_p95:.3} ms, budget {allowed:.1} ms \
         (load {:.2})",
        load_1min()
    );

    assert!(
        median_p95 < allowed,
        "median batch p95 must stay inside the 60 fps budget, got {median_p95:.3} ms \
         (batches: {batch_p95:?}, load {:.2})",
        load_1min()
    );
}

/// A 128×128 table is the widest the definitions declare. Virtualisation must
/// keep it on the same frame budget as a 32×32 one: only the visible window of
/// cells is built, so the table's area must not matter.
#[gpui_kit::test]
fn renders_128x128_within_the_60fps_budget(cx: &mut TestAppContext) {
    let (handle, grid, _) = open(cx, 128, 128);
    let frames = 120;
    let mut batch_p95 = Vec::new();
    let mut batch_stats = Vec::new();

    cx.update_window(handle, |_, window, cx| {
        for _ in 0..3 {
            let mut samples = Vec::with_capacity(frames);
            for frame in 0..frames {
                grid.update(cx, |grid, cx| {
                    grid.nudge_selection(cx);
                    grid.set_realtime(
                        Some(RealtimePoint {
                            row: frame % 128,
                            column: (frame * 7) % 128,
                            value: 42.0,
                        }),
                        cx,
                    );
                });
                let started = Instant::now();
                window.render_frame(cx);
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(f64::total_cmp);
            let mean = samples.iter().sum::<f64>() / samples.len() as f64;
            let p50 = samples[samples.len() / 2];
            let p95 = samples[samples.len() * 95 / 100];
            let p99 = samples[samples.len() * 99 / 100];
            let max = samples[samples.len() - 1];
            batch_stats.push(format!(
                "mean {mean:.3}, p50 {p50:.3}, p95 {p95:.3}, p99 {p99:.3}, max {max:.3}"
            ));
            batch_p95.push(p95);
        }
    })
    .unwrap();

    for (batch, stats) in batch_stats.iter().enumerate() {
        eprintln!("table grid perf 128x128 batch {batch}: {stats} ms");
    }
    batch_p95.sort_by(f64::total_cmp);
    let median_p95 = batch_p95[1];
    let allowed = judged_budget_ms();
    eprintln!(
        "table grid perf 128x128: median batch p95 {median_p95:.3} ms, budget {allowed:.1} ms \
         (load {:.2})",
        load_1min()
    );

    assert!(
        median_p95 < allowed,
        "median batch p95 must stay inside the 60 fps budget, got {median_p95:.3} ms \
         (batches: {batch_p95:?}, load {:.2})",
        load_1min()
    );
}
