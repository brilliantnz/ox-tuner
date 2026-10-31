//! Tests for the curve widget.
//!
//! The model's arithmetic and state rules are tested directly; the rest drive
//! the real view through pointer and keyboard events, the same way a person
//! edits a point.

use std::{cell::RefCell, rc::Rc};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement,
    ParentElement as _, Pixels, Point, Render, Styled as _, Subscription, TestAppContext, Window,
    WindowBounds, WindowOptions, div, point, px, size,
};
use oxtuner_core::{AxisSummary, CellEdit, CellState, CurveAxes, TableSummary};

use super::model::{CurveModel, CurvePoint, CurveSnapshot, curve_ticks, format_tick, tick_step};
use super::{Curve, CurveEvent};

struct Host {
    curve: Entity<Curve>,
    _events: Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.curve.clone())
    }
}

/// A six-point dwell curve with the definition's fixed axis limits.
fn snapshot() -> CurveSnapshot {
    CurveSnapshot::new(
        "dwell_correction_curve",
        "Dwell voltage correction",
        vec![
            CurvePoint::new(6.0, 250.0),
            CurvePoint::new(8.0, 220.0),
            CurvePoint::new(10.0, 190.0),
            CurvePoint::new(12.0, 160.0),
            CurvePoint::new(14.0, 130.0),
            CurvePoint::new(16.0, 100.0),
        ],
    )
    .with_name("dwellRates")
    .with_units("%")
    .with_digits(0)
    .with_x_axis("Voltage", "V", 1)
    .with_y_label("Dwell")
    .with_x_range(6.0, 22.0)
    .with_y_range(0.0, 255.0)
    .with_divisions(6)
}

fn open(
    cx: &mut TestAppContext,
    snapshot: CurveSnapshot,
) -> (AnyWindowHandle, Entity<Curve>, Rc<RefCell<Vec<CurveEvent>>>) {
    cx.update(gpui_kit::init);
    let events = Rc::new(RefCell::new(Vec::new()));
    let (handle, curve) = cx.update(|cx| {
        let curve = cx.new(Curve::new);
        let sink = events.clone();
        let subscription = cx.subscribe(&curve, move |_, event: &CurveEvent, _| {
            sink.borrow_mut().push(event.clone());
        });
        let host = cx.new(|_| Host {
            curve: curve.clone(),
            _events: subscription,
        });
        let (handle, _) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(900.), px(600.)),
                ))),
                ..Default::default()
            },
            cx,
            move |_, _| host,
        )
        .expect("open test window");

        curve.update(cx, |curve, cx| curve.set_snapshot(snapshot, cx));

        (handle, curve)
    });

    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();

    (handle, curve, events)
}

fn plot(cx: &mut TestAppContext, handle: AnyWindowHandle) -> Bounds<Pixels> {
    cx.update_window(handle, |_, window, _| {
        window.find(ElementId::from("curve-plot")).bounds()
    })
    .unwrap()
}

/// A window position for one point on the plot.
fn point_at(plot: Bounds<Pixels>, x: f64, y: f64) -> Point<Pixels> {
    point(
        plot.origin.x + px(x as f32 * f32::from(plot.size.width)),
        plot.origin.y + px(y as f32 * f32::from(plot.size.height)),
    )
}

/// The Y fraction a value sits at in the fixed 0 to 255 range.
fn y_fraction(value: f64) -> f64 {
    (255.0 - value) / 255.0
}

/// The X fraction of a bin in the fixed 6 to 22 range.
fn x_fraction(value: f64) -> f64 {
    (value - 6.0) / 16.0
}

fn selected(curve: &Entity<Curve>, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| curve.read(cx).model().selected())
}

fn value(curve: &Entity<Curve>, index: usize, cx: &mut TestAppContext) -> Option<f64> {
    cx.update(|cx| curve.read(cx).model().value(index))
}

fn last_edit(events: &Rc<RefCell<Vec<CurveEvent>>>) -> Option<(String, Vec<CellEdit>)> {
    events.borrow().iter().rev().find_map(|event| match event {
        CurveEvent::EditRequested { name, edits } => Some((name.clone(), edits.clone())),
        _ => None,
    })
}

#[test]
fn dragging_up_raises_the_point_and_clamps_at_the_top() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());

    assert!(model.begin_drag(1, y_fraction(220.0)));
    assert_eq!(model.drag_to(-0.5), Some((1, 255.0)));
    assert_eq!(model.finish_drag(), Some((1, 255.0)));
    assert!(!model.is_dragging());
}

#[test]
fn a_grab_beside_the_dot_keeps_its_offset() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());

    // Pressing five units below the dot and moving the pointer to 240 must
    // leave the dot at 245, not snap it under the pointer.
    assert!(model.begin_drag(1, y_fraction(215.0)));
    assert_eq!(model.drag_to(y_fraction(240.0)), Some((1, 245.0)));
}

#[test]
fn a_drag_that_returns_to_its_start_writes_nothing() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());

    assert!(model.begin_drag(1, y_fraction(220.0)));
    assert!(model.drag_to(y_fraction(180.0)).is_some());
    assert!(model.drag_to(y_fraction(220.0)).is_some());
    assert_eq!(model.finish_drag(), None);
    assert_eq!(model.value(1), Some(220.0));
}

#[test]
fn a_cancelled_drag_restores_the_value() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());

    assert!(model.begin_drag(1, y_fraction(220.0)));
    assert!(model.drag_to(y_fraction(150.0)).is_some());
    assert_eq!(model.cancel_drag(), Some(1));
    assert_eq!(model.value(1), Some(220.0));
}

#[test]
fn read_only_points_cannot_be_dragged_or_nudged() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot().with_writable(false));

    assert!(!model.begin_drag(1, y_fraction(220.0)));
    assert_eq!(model.nudge(1), None);
    assert_eq!(model.value(1), Some(220.0));
}

#[test]
fn a_nudge_steps_by_the_display_digits() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot().with_digits(1));

    assert_eq!(model.step(), 0.1);
    assert_eq!(model.nudge(1), Some((0, 250.1)));
    assert_eq!(model.nudge(10), Some((0, 251.1)));
    assert_eq!(model.value(0), Some(251.1));
}

#[test]
fn a_confirmed_echo_drops_the_optimistic_value() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());

    assert_eq!(model.nudge(1), Some((0, 251.0)));
    assert_eq!(model.value(0), Some(251.0));

    model.set_snapshot(with_value(snapshot(), 0, 251.0));
    assert_eq!(model.value(0), Some(251.0));

    model.set_snapshot(with_value(snapshot(), 0, 230.0));
    assert_eq!(model.value(0), Some(230.0));
}

#[test]
fn selection_stops_at_either_end() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());

    assert!(!model.select_relative(-1));
    assert_eq!(model.selected(), 0);

    assert!(model.select_relative(99));
    assert_eq!(model.selected(), 5);

    assert!(!model.select_relative(1));
    assert_eq!(model.selected(), 5);
}

#[test]
fn a_data_derived_range_follows_the_values() {
    let mut model = CurveModel::new();
    model.set_snapshot(CurveSnapshot::new(
        "idle",
        "Idle",
        vec![CurvePoint::new(0.0, 10.0), CurvePoint::new(5.0, 30.0)],
    ));

    let (minimum, maximum) = model.y_range();

    assert!(minimum < 10.0);
    assert!(maximum > 30.0);
}

#[test]
fn a_declared_range_wins_over_the_values() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());

    assert_eq!(model.y_range(), (0.0, 255.0));
    assert_eq!(model.x_range(), (6.0, 22.0));
}

#[test]
fn a_shorter_snapshot_clamps_the_selection_and_drops_stale_values() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());
    assert!(model.select(5));
    assert_eq!(model.nudge(1), Some((5, 101.0)));

    let shorter = CurveSnapshot::new(
        "dwell_correction_curve",
        "Dwell voltage correction",
        vec![CurvePoint::new(6.0, 245.0), CurvePoint::new(8.0, 215.0)],
    )
    .with_name("dwellRates")
    .with_units("%")
    .with_digits(0)
    .with_x_axis("Voltage", "V", 1)
    .with_y_range(0.0, 255.0);

    model.set_snapshot(shorter);

    assert_eq!(model.point_count(), 2);
    assert_eq!(model.selected(), 1);
    assert_eq!(model.value(1), Some(215.0));
    assert_eq!(model.value(5), None);
}

#[test]
fn a_negative_bin_expands_below_zero() {
    let mut model = CurveModel::new();
    model.set_snapshot(
        CurveSnapshot::new(
            "temp_curve",
            "Temperature correction",
            vec![CurvePoint::new(-15.0, 1.0), CurvePoint::new(20.0, 2.0)],
        )
        .with_x_range(0.0, 100.0),
    );

    let (minimum, maximum) = model.x_range();

    assert!(minimum < -15.0, "the negative bin needs space below zero");
    assert_eq!(maximum, 100.0);
}

#[test]
fn a_zero_first_bin_gets_a_margin_below_zero() {
    let mut model = CurveModel::new();
    model.set_snapshot(
        snapshot()
            .with_x_range(75.0, 112.0)
            .with_x_values([0.0, 88.0, 92.0, 96.0, 98.0, 100.0]),
    );

    let (minimum, maximum) = model.x_range();

    assert!(minimum < 0.0, "the zero bin sits inside the plot");
    assert!(minimum > -20.0, "the margin stays small");
    assert_eq!(maximum, 112.0);
}

#[test]
fn the_value_frame_stops_at_the_cell_limits() {
    let mut table = curve_table();
    table.cell_maximum = Some(200.0);

    let snapshot = CurveSnapshot::from_table(&table).expect("a curve");

    assert_eq!(
        snapshot.y_range(),
        Some((0.0, 200.0)),
        "a drag cannot aim past a value the core will clamp"
    );
}

#[test]
fn a_pending_axis_preview_replaces_the_bin_values() {
    let snapshot = snapshot().with_x_values([6.0, 9.0, 10.0, 12.0, 14.0, 16.0]);

    assert_eq!(snapshot.points()[1], CurvePoint::new(9.0, 220.0));
    assert_eq!(snapshot.points()[0], CurvePoint::new(6.0, 250.0));
}

#[test]
fn an_edited_bin_outside_the_declared_range_expands_the_x_axis() {
    let mut model = CurveModel::new();
    model.set_snapshot(snapshot());

    assert_eq!(model.x_range(), (6.0, 22.0));

    // The first bin is dragged far left of the declared xAxis.
    let mut edited = snapshot();
    edited = with_value(edited, 0, 245.0);
    let mut points = edited.points().to_vec();
    points[0] = CurvePoint::new(3.0, points[0].y);
    edited = CurveSnapshot::new(edited.id(), edited.title(), points)
        .with_name(edited.name())
        .with_units(edited.units())
        .with_digits(edited.digits())
        .with_x_axis(edited.x_label(), edited.x_units(), edited.x_digits())
        .with_y_label(edited.y_label())
        .with_x_range(6.0, 22.0)
        .with_y_range(0.0, 255.0)
        .with_divisions(6);

    model.set_snapshot(edited);
    let (minimum, maximum) = model.x_range();
    assert!(
        minimum < 3.0,
        "the edited bin sits inside the expanded range, not on its edge"
    );
    assert_eq!(maximum, 22.0);
    assert_eq!(
        minimum, 2.0,
        "the new edge is the small round margin below the bin"
    );
}

#[test]
fn a_one_row_table_reads_as_a_curve() {
    let table = curve_table();

    let snapshot = CurveSnapshot::from_table(&table).expect("a curve");

    assert_eq!(snapshot.id(), "dwell_correction_curve");
    assert_eq!(snapshot.name(), "dwellRates");
    assert_eq!(snapshot.title(), "Dwell voltage correction");
    assert_eq!(snapshot.units(), "%");
    assert_eq!(snapshot.digits(), 0);
    assert_eq!(snapshot.x_label(), "Voltage");
    assert_eq!(snapshot.points().len(), 3);
    assert_eq!(snapshot.points()[1], CurvePoint::new(8.0, 220.0));
    assert_eq!(snapshot.x_range(), Some((6.0, 22.0)));
    assert_eq!(snapshot.y_range(), Some((0.0, 255.0)));
    assert_eq!(snapshot.divisions(), 6);
    assert!(snapshot.is_writable());
    assert!(CurveSnapshot::from_table(&two_row_table()).is_none());
}

#[test]
fn axes_keep_their_exact_limits_and_drop_crowded_ticks() {
    let ticks = curve_ticks((-12.7, 12.7), 5);

    assert_eq!(ticks.first(), Some(&-12.7));
    assert_eq!(ticks.last(), Some(&12.7));
    assert!(ticks.windows(2).all(|pair| pair[1] > pair[0]));

    let steps = curve_ticks((0.0, 255.0), 6);
    assert_eq!(steps.first(), Some(&0.0));
    assert_eq!(steps.last(), Some(&255.0));
    assert!(steps.iter().all(|tick| (tick - 250.0).abs() > 1e-9));
}

#[test]
fn tick_labels_carry_the_step_precision() {
    let step = tick_step(&curve_ticks((0.0, 255.0), 6));

    assert_eq!(format_tick(0.0, step), "0");
    assert_eq!(format_tick(100.0, step), "100");
    assert_eq!(format_tick(-0.00001, step), "0");
    assert_eq!(format_tick(2.5, 0.25), "2.50");
}

fn with_value(snapshot: CurveSnapshot, index: usize, value: f64) -> CurveSnapshot {
    let mut points = snapshot.points().to_vec();
    points[index] = CurvePoint::new(points[index].x, value);

    let mut next = CurveSnapshot::new(snapshot.id(), snapshot.title(), points)
        .with_name(snapshot.name())
        .with_units(snapshot.units())
        .with_digits(snapshot.digits())
        .with_x_axis(snapshot.x_label(), snapshot.x_units(), snapshot.x_digits())
        .with_y_label(snapshot.y_label())
        .with_divisions(snapshot.divisions())
        .with_writable(snapshot.is_writable());

    if let Some((minimum, maximum)) = snapshot.x_range() {
        next = next.with_x_range(minimum, maximum);
    }
    if let Some((minimum, maximum)) = snapshot.y_range() {
        next = next.with_y_range(minimum, maximum);
    }

    next
}

fn curve_table() -> TableSummary {
    TableSummary {
        id: "dwell_correction_curve".to_owned(),
        name: "dwellRates".to_owned(),
        title: "Dwell voltage correction".to_owned(),
        cols: 3,
        rows: 1,
        digits: 0,
        units: "%".to_owned(),
        cells: vec![250.0, 220.0, 100.0],
        states: vec![CellState::Unknown; 3],
        changed: vec![false; 3],
        x_axis: AxisSummary {
            name: "brvBins".to_owned(),
            label: "Voltage".to_owned(),
            units: "V".to_owned(),
            channel: Some("batteryVoltage".to_owned()),
            digits: 1,
            minimum: None,
            maximum: None,
            values: vec![6.0, 8.0, 16.0],
        },
        y_axis: AxisSummary {
            name: "dwellRates".to_owned(),
            label: "Dwell".to_owned(),
            units: "%".to_owned(),
            channel: None,
            digits: 0,
            minimum: None,
            maximum: None,
            values: vec![100.0],
        },
        minimum: 100.0,
        maximum: 250.0,
        cell_minimum: Some(0.0),
        cell_maximum: Some(255.0),
        writable: true,
        curve: Some(CurveAxes {
            x_min: 6.0,
            x_max: 22.0,
            y_min: 0.0,
            y_max: 255.0,
            divisions: 6,
        }),
    }
}

fn two_row_table() -> TableSummary {
    let mut table = curve_table();
    table.rows = 2;
    table.cells = vec![250.0, 220.0, 100.0, 90.0, 80.0, 70.0];
    table.curve = None;

    table
}

#[gpui_kit::test]
fn a_point_sits_on_its_value_and_its_dot_is_centered(cx: &mut TestAppContext) {
    let (handle, _, _) = open(cx, snapshot());
    let plot = plot(cx, handle);
    let expected = point_at(plot, x_fraction(8.0), y_fraction(220.0));
    let wrapper = cx
        .update_window(handle, |_, window, _| {
            window
                .find(ElementId::from(("curve-point", 1usize)))
                .bounds()
        })
        .unwrap();
    let dot = cx
        .update_window(handle, |_, window, _| {
            window
                .find(ElementId::from(("curve-point-dot", 1usize)))
                .bounds()
        })
        .unwrap();
    let center = |bounds: Bounds<Pixels>| {
        point(
            bounds.origin.x + bounds.size.width / 2.0,
            bounds.origin.y + bounds.size.height / 2.0,
        )
    };
    let wrapper_center = center(wrapper);
    let dot_center = center(dot);

    assert!((f32::from(wrapper_center.x - expected.x)).abs() < 1.5);
    assert!((f32::from(wrapper_center.y - expected.y)).abs() < 1.5);
    assert!((f32::from(dot_center.x - wrapper_center.x)).abs() < 0.5);
    assert!((f32::from(dot_center.y - wrapper_center.y)).abs() < 0.5);

    // The selected point carries a halo behind its dot. The two circles must
    // share one center, not sit side by side in the point's square.
    let selected_wrapper = cx
        .update_window(handle, |_, window, _| {
            window
                .find(ElementId::from(("curve-point", 0usize)))
                .bounds()
        })
        .unwrap();
    let selected_dot = cx
        .update_window(handle, |_, window, _| {
            window
                .find(ElementId::from(("curve-point-dot", 0usize)))
                .bounds()
        })
        .unwrap();
    let selected_halo = cx
        .update_window(handle, |_, window, _| {
            window
                .find(ElementId::from(("curve-point-halo", 0usize)))
                .bounds()
        })
        .unwrap();

    assert_eq!(center(selected_dot), center(selected_wrapper));
    assert_eq!(center(selected_halo), center(selected_wrapper));
}

#[gpui_kit::test]
fn clicking_a_point_selects_it(cx: &mut TestAppContext) {
    let (handle, curve, events) = open(cx, snapshot());
    let plot = plot(cx, handle);

    cx.update_window(handle, |_, window, cx| {
        window.click_at(
            ElementId::from("curve-plot"),
            point(
                px(x_fraction(12.0) as f32 * f32::from(plot.size.width)),
                px(y_fraction(160.0) as f32 * f32::from(plot.size.height)),
            ),
            cx,
        );
    })
    .unwrap();

    assert_eq!(selected(&curve, cx), 3);
    assert!(
        events
            .borrow()
            .iter()
            .any(|event| matches!(event, CurveEvent::SelectionChanged { index: 3 }))
    );
}

#[gpui_kit::test]
fn dragging_a_point_commits_one_edit(cx: &mut TestAppContext) {
    let (handle, curve, events) = open(cx, snapshot());
    let plot = plot(cx, handle);
    let start = point_at(plot, x_fraction(8.0), y_fraction(220.0));
    let target = point_at(plot, x_fraction(8.0), y_fraction(250.0));

    cx.update_window(handle, |_, window, cx| {
        window.drag(start, target, cx);
    })
    .unwrap();

    assert_eq!(
        value(&curve, 1, cx),
        Some(250.0),
        "the drag moves the point"
    );
    assert_eq!(
        last_edit(&events),
        Some((
            "dwellRates".to_owned(),
            vec![CellEdit {
                row: 0,
                column: 1,
                value: 250.0,
            }],
        ))
    );
    let borrowed = events.borrow();
    assert!(borrowed.iter().any(|event| matches!(
        event,
        CurveEvent::PreviewChanged {
            index: 1,
            value: Some(_)
        }
    )));
    assert!(borrowed.iter().any(|event| matches!(
        event,
        CurveEvent::PreviewChanged {
            index: 1,
            value: None
        }
    )));
    assert_eq!(
        borrowed
            .iter()
            .filter(|event| matches!(event, CurveEvent::EditRequested { .. }))
            .count(),
        1,
        "one write per drag, not one per pixel"
    );
}

#[gpui_kit::test]
fn a_read_only_curve_selects_but_does_not_write(cx: &mut TestAppContext) {
    let (handle, curve, events) = open(cx, snapshot().with_writable(false));
    let plot = plot(cx, handle);
    let start = point_at(plot, x_fraction(8.0), y_fraction(220.0));
    let target = point_at(plot, x_fraction(8.0), y_fraction(250.0));

    cx.update_window(handle, |_, window, cx| {
        window.drag(start, target, cx);
    })
    .unwrap();

    assert_eq!(selected(&curve, cx), 1);
    assert_eq!(last_edit(&events), None);
    assert_eq!(value(&curve, 1, cx), Some(220.0));
}

#[gpui_kit::test]
fn arrow_keys_nudge_the_selected_point(cx: &mut TestAppContext) {
    let (handle, curve, events) = open(cx, snapshot());
    let plot = plot(cx, handle);

    cx.update_window(handle, |_, window, cx| {
        window.click_at(
            ElementId::from("curve-plot"),
            point(
                px(x_fraction(8.0) as f32 * f32::from(plot.size.width)),
                px(y_fraction(220.0) as f32 * f32::from(plot.size.height)),
            ),
            cx,
        );
        window.press("up", cx);
    })
    .unwrap();

    assert_eq!(
        last_edit(&events),
        Some((
            "dwellRates".to_owned(),
            vec![CellEdit {
                row: 0,
                column: 1,
                value: 221.0,
            }],
        ))
    );

    cx.update_window(handle, |_, window, cx| window.press("shift-up", cx))
        .unwrap();

    assert_eq!(value(&curve, 1, cx), Some(231.0));
}

#[gpui_kit::test]
fn left_and_right_move_the_selection(cx: &mut TestAppContext) {
    let (handle, curve, _) = open(cx, snapshot());
    let plot = plot(cx, handle);

    cx.update_window(handle, |_, window, cx| {
        window.click_at(
            ElementId::from("curve-plot"),
            point(
                px(x_fraction(8.0) as f32 * f32::from(plot.size.width)),
                px(y_fraction(220.0) as f32 * f32::from(plot.size.height)),
            ),
            cx,
        );
        window.press("right", cx);
        window.press("right", cx);
    })
    .unwrap();

    assert_eq!(selected(&curve, cx), 3);
}

#[gpui_kit::test]
fn a_snapshot_echo_replaces_the_optimistic_value(cx: &mut TestAppContext) {
    let (handle, curve, _) = open(cx, snapshot());
    let plot = plot(cx, handle);

    cx.update_window(handle, |_, window, cx| {
        window.click_at(
            ElementId::from("curve-plot"),
            point(
                px(x_fraction(8.0) as f32 * f32::from(plot.size.width)),
                px(y_fraction(220.0) as f32 * f32::from(plot.size.height)),
            ),
            cx,
        );
        window.press("up", cx);
    })
    .unwrap();

    assert_eq!(value(&curve, 1, cx), Some(221.0));

    let confirmed = with_value(snapshot(), 1, 221.0);
    cx.update(|cx| {
        curve.update(cx, |curve, cx| curve.set_snapshot(confirmed, cx));
    });
    assert_eq!(value(&curve, 1, cx), Some(221.0));

    let moved = with_value(snapshot(), 1, 205.0);
    cx.update(|cx| {
        curve.update(cx, |curve, cx| curve.set_snapshot(moved, cx));
    });
    assert_eq!(value(&curve, 1, cx), Some(205.0));
}
