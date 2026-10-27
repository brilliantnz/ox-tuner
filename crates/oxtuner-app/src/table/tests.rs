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
use oxtuner_core::{AxisSummary, CellState, TableSummary};

use super::grid::RealtimePoint;
use super::model::CellPos;
use super::{TableGrid, TableGridEvent};

struct Host {
    grid: Entity<TableGrid>,
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
            digits: 0,
            values: (0..columns).map(|value| value as f64 * 500.0).collect(),
        },
        y_axis: AxisSummary {
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

fn open(
    cx: &mut TestAppContext,
    rows: usize,
    columns: usize,
) -> (
    AnyWindowHandle,
    Entity<TableGrid>,
    Rc<RefCell<Vec<TableGridEvent>>>,
) {
    cx.update(gpui_kit::init);
    let events = Rc::new(RefCell::new(Vec::new()));
    let (handle, grid) = cx.update(|cx| {
        let grid = cx.new(TableGrid::new);
        let sink = events.clone();
        let subscription = cx.subscribe(&grid, move |_, event: &TableGridEvent, _| {
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

fn selected(grid: &Entity<TableGrid>, cx: &mut TestAppContext) -> CellPos {
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
            TableGridEvent::EditRequested { name, edits }
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
        // A selected cell adds a heavier border; the header must not move.
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
