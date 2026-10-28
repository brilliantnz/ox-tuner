//! Headless tests for the reusable chart component.
//!
//! The data shaping (windows, decimation, ticks) is tested in `oxtuner-core`.
//! These tests cover the component's own contract: which channels it selects,
//! legend toggles, pause/follow, pan/zoom limits and a render pass in both
//! layouts.

use std::{cell::RefCell, rc::Rc};

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, TestAppContext, Window, WindowBounds, WindowOptions, div, point, px,
    size,
};
use oxtuner_core::{ChannelInfo, ChartHistory};

use super::{ChartEvent, ChartMode, ChartState, format_offset, format_value};

struct Host {
    chart: gpui_kit::Entity<ChartState>,
    _events: Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.chart.clone())
    }
}

fn history() -> ChartHistory {
    let history = ChartHistory::new(10_000);
    history.configure(vec![
        ChannelInfo::new("RPM", "rpm"),
        ChannelInfo::new("MAP", "kPa"),
        ChannelInfo::new("CLT", "°C"),
        ChannelInfo::new("TPS", "%"),
        ChannelInfo::new("AFR", "AFR"),
        ChannelInfo::new("batteryVoltage", "V"),
    ]);
    for step in 0..1_000 {
        let t = f64::from(step) * 0.05;
        history.push_frame(
            t,
            &[
                ("RPM", 900.0 + f64::from(step % 200) * 30.0),
                ("MAP", 45.0 + f64::from(step % 40)),
                ("CLT", 80.0 + f64::from(step % 20) * 0.2),
                ("TPS", f64::from(step % 100)),
                ("AFR", 14.0 + f64::from(step % 10) * 0.05),
                ("batteryVoltage", 14.1),
            ],
        );
    }
    history
}

fn open(
    cx: &mut TestAppContext,
    history: ChartHistory,
) -> (
    AnyWindowHandle,
    gpui_kit::Entity<ChartState>,
    Rc<RefCell<Vec<ChartEvent>>>,
) {
    cx.update(gpui_kit::init);
    let events = Rc::new(RefCell::new(Vec::new()));
    let (handle, chart) = cx.update(|cx| {
        let chart = cx.new(|cx| ChartState::new(history.clone(), cx));
        let sink = events.clone();
        let _subscription = cx.subscribe(&chart, move |_, event: &ChartEvent, _| {
            sink.borrow_mut().push(event.clone());
        });
        // Keep the subscription alive through the host.
        let host = cx.new(|_| Host {
            chart: chart.clone(),
            _events: _subscription,
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
        (handle, chart)
    });
    (handle, chart, events)
}

#[gpui_kit::test]
fn the_default_selection_is_the_five_live_channels(cx: &mut TestAppContext) {
    let (_handle, chart, _events) = open(cx, history());

    let series = cx.update(|cx| chart.read(cx).series(cx));
    let ids: Vec<&str> = series.iter().map(|series| series.id.as_str()).collect();

    assert_eq!(ids, ["RPM", "MAP", "CLT", "TPS", "AFR"]);
    assert_eq!(series[0].units, "rpm");
    assert!(series.iter().all(|series| series.visible));
}

#[gpui_kit::test]
fn the_default_selection_does_not_take_both_spellings_of_one_sensor(cx: &mut TestAppContext) {
    let history = ChartHistory::new(100);
    history.configure(vec![
        ChannelInfo::new("rpm", "rpm"),
        ChannelInfo::new("map", "kPa"),
        ChannelInfo::new("coolant", "C"),
        ChannelInfo::new("clt", "C"),
        ChannelInfo::new("tps", "%"),
        ChannelInfo::new("afr", "AFR"),
        ChannelInfo::new("ego", "AFR"),
    ]);
    history.push_frame(
        0.0,
        &[
            ("rpm", 1000.0),
            ("map", 40.0),
            ("coolant", 80.0),
            ("clt", 80.0),
            ("tps", 5.0),
            ("afr", 14.7),
            ("ego", 14.7),
        ],
    );
    let (_handle, chart, _events) = open(cx, history);

    let series = cx.update(|cx| chart.read(cx).series(cx));
    let ids: Vec<&str> = series.iter().map(|series| series.id.as_str()).collect();

    assert_eq!(ids, ["rpm", "map", "coolant", "tps", "afr"]);
}

#[gpui_kit::test]
fn the_first_channel_toggle_starts_from_the_visible_defaults(cx: &mut TestAppContext) {
    let (_handle, chart, _events) = open(cx, history());
    assert!(cx.update(|cx| chart.read(cx).is_selected("RPM")));

    cx.update(|cx| {
        chart.update(cx, |chart, cx| chart.toggle_selected("batteryVoltage", cx));
    });
    assert!(cx.update(|cx| chart.read(cx).is_selected("batteryVoltage")));
    assert!(cx.update(|cx| chart.read(cx).is_selected("RPM")));

    cx.update(|cx| {
        chart.update(cx, |chart, cx| chart.toggle_selected("MAP", cx));
    });
    assert!(!cx.update(|cx| chart.read(cx).is_selected("MAP")));
    assert!(cx.update(|cx| chart.read(cx).is_selected("batteryVoltage")));
}

#[gpui_kit::test]
fn a_legend_click_hides_and_shows_a_series(cx: &mut TestAppContext) {
    let (_handle, chart, events) = open(cx, history());

    cx.update(|cx| {
        chart.update(cx, |chart, cx| chart.toggle_visible("MAP", cx));
    });

    assert!(!cx.update(|cx| chart.read(cx).is_visible("MAP")));
    assert_eq!(
        events.borrow().last(),
        Some(&ChartEvent::VisibilityChanged {
            id: "MAP".to_owned(),
            visible: false
        })
    );

    cx.update(|cx| {
        chart.update(cx, |chart, cx| chart.toggle_visible("MAP", cx));
    });
    assert!(cx.update(|cx| chart.read(cx).is_visible("MAP")));
}

#[gpui_kit::test]
fn pause_freezes_the_window_and_follow_resumes_it(cx: &mut TestAppContext) {
    let (_handle, chart, events) = open(cx, history());
    let newest = cx.update(|cx| chart.read(cx).window().end);

    cx.update(|cx| {
        chart.update(cx, |chart, cx| chart.set_paused(true, cx));
    });
    let frozen = cx.update(|cx| chart.read(cx).window().end);
    assert!((frozen - newest).abs() < 1e-9);
    assert!(cx.update(|cx| chart.read(cx).is_paused()));
    assert!(
        events
            .borrow()
            .iter()
            .any(|event| matches!(event, ChartEvent::PausedChanged { paused: true }))
    );

    cx.update(|cx| {
        chart.update(cx, |chart, cx| chart.follow(cx));
    });
    assert!(!cx.update(|cx| chart.read(cx).is_paused()));
    assert!(cx.update(|cx| chart.read(cx).is_following()));
}

#[gpui_kit::test]
fn pan_and_zoom_stay_inside_the_history_and_the_configured_limits(cx: &mut TestAppContext) {
    let (_handle, chart, _events) = open(cx, history());

    cx.update(|cx| {
        chart.update(cx, |chart, cx| {
            chart.set_window_secs(20.0, cx);
            chart.pan_by(15.0, cx);
            assert!(!chart.is_following());
            let window = chart.window();
            assert!(window.span() <= 20.0 + 1e-6);
            chart.pan_by(10_000.0, cx);
            assert!(chart.window().start >= chart.history().oldest_time().unwrap_or(0.0));
            chart.zoom_by(0.0001, cx);
            assert_eq!(chart.window_secs(), 5.0, "clamped to the minimum");
            chart.zoom_by(1_000.0, cx);
            assert_eq!(chart.window_secs(), 120.0, "clamped to the maximum");
            chart.follow(cx);
            assert!(chart.is_following());
        });
    });
}

#[gpui_kit::test]
fn the_layout_switches_between_stacked_and_overlay(cx: &mut TestAppContext) {
    let (_handle, chart, events) = open(cx, history());

    cx.update(|cx| {
        chart.update(cx, |chart, cx| chart.set_mode(ChartMode::Overlay, cx));
    });

    assert_eq!(cx.update(|cx| chart.read(cx).mode()), ChartMode::Overlay);
    assert_eq!(
        events.borrow().last(),
        Some(&ChartEvent::ModeChanged {
            mode: ChartMode::Overlay
        })
    );
}

#[gpui_kit::test]
fn overlay_starts_per_series_and_switches_to_a_shared_scale(cx: &mut TestAppContext) {
    let (_handle, chart, _events) = open(cx, history());

    cx.update(|cx| {
        chart.update(cx, |chart, cx| {
            assert!(chart.is_normalized_overlay());
            chart.set_mode(ChartMode::Overlay, cx);
            chart.set_normalize_overlay(false, cx);
            assert!(!chart.is_normalized_overlay());
        });
    });

    assert!(!cx.update(|cx| chart.read(cx).is_normalized_overlay()));
}

#[gpui_kit::test]
fn five_series_render_inside_the_p9_budget(cx: &mut TestAppContext) {
    let history = ChartHistory::new(6_000);
    history.configure(vec![
        ChannelInfo::new("rpm", "rpm"),
        ChannelInfo::new("map", "kPa"),
        ChannelInfo::new("clt", "°C"),
        ChannelInfo::new("tps", "%"),
        ChannelInfo::new("afr", "AFR"),
    ]);
    for step in 0..6_000 {
        let t = f64::from(step) * 0.01;
        history.push_frame(
            t,
            &[
                ("rpm", 900.0 + f64::from(step % 600) * 10.0),
                ("map", 40.0 + f64::from(step % 60)),
                ("clt", 80.0 + f64::from(step % 200) * 0.1),
                ("tps", f64::from(step % 100)),
                ("afr", 13.5 + f64::from(step % 30) * 0.05),
            ],
        );
    }
    let (handle, _chart, _events) = open(cx, history);

    let started = std::time::Instant::now();
    let frames = 20;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        for _ in 0..frames {
            window.render_frame(cx);
        }
    })
    .unwrap();
    let mean = started.elapsed().as_secs_f64() * 1000.0 / f64::from(frames);

    // The P9 canvas measurement was 5.6 ms debug on a quiet machine. The
    // prototype allows 2.5× on a busy desktop; this is the same loose guard,
    // not a micro-benchmark. A real regression (per-point labels, no
    // decimation) is an order of magnitude, not a few milliseconds.
    assert!(
        mean < 41.75,
        "five-series chart rendered in {mean:.2} ms per frame"
    );
}

#[gpui_kit::test]
fn both_layouts_render_with_data(cx: &mut TestAppContext) {
    let (handle, chart, _events) = open(cx, history());

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        chart.update(cx, |chart, cx| chart.set_mode(ChartMode::Overlay, cx));
        window.render_frame(cx);
        chart.update(cx, |chart, cx| chart.set_mode(ChartMode::Stacked, cx));
        chart.update(cx, |chart, cx| chart.set_paused(true, cx));
        window.render_frame(cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_series_list_follows_a_definition_reload(cx: &mut TestAppContext) {
    let (_handle, chart, _events) = open(cx, history());

    let next = ChartHistory::new(10_000);
    next.configure(vec![
        ChannelInfo::new("rpm", "rpm"),
        ChannelInfo::new("tps", "%"),
    ]);
    next.push_frame(0.0, &[("rpm", 1200.0), ("tps", 10.0)]);
    cx.update(|cx| {
        chart.update(cx, |chart, cx| chart.set_history(next, cx));
    });

    let series = cx.update(|cx| chart.read(cx).series(cx));
    let ids: Vec<&str> = series.iter().map(|series| series.id.as_str()).collect();
    assert_eq!(ids, ["rpm", "tps"]);
}

#[test]
fn axis_labels_have_no_negative_zero_and_read_as_offsets() {
    assert_eq!(format_value(-0.001), "0");
    assert_eq!(format_value(-0.4), "-0.40");
    assert_eq!(format_offset(-30.0), "-30s");
    assert_eq!(format_offset(0.0), "0s");
    assert_eq!(format_offset(-90.0), "-1:30");
}

#[gpui_kit::test]
fn the_hover_fraction_is_measured_across_the_plot_not_the_body(cx: &mut TestAppContext) {
    let (handle, chart, _events) = open(cx, history());

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();

    cx.update(|cx| {
        chart.update(cx, |chart, _| {
            let body = chart.body_bounds.get();
            let plot = chart.plot_bounds.get();
            assert!(
                plot.size.width > px(0.),
                "a rendered frame records the plot rect"
            );
            assert!(
                plot.origin.x > body.origin.x,
                "the plot sits right of the body origin"
            );
            assert!(
                plot.size.width < body.size.width,
                "the plot is narrower than the body"
            );

            // Over the tick labels there is no crosshair: that x is outside the plot.
            chart.hover = Some(point(plot.origin.x - px(4.), plot.origin.y + px(10.)));
            assert_eq!(chart.hover_fraction(), None);

            // The middle of the plot is the middle of the window. Measured
            // against the body this would read well under 0.5.
            chart.hover = Some(point(plot.origin.x + plot.size.width / 2., plot.origin.y));
            let fraction = chart.hover_fraction().expect("inside the plot");
            assert!((fraction - 0.5).abs() < 0.001, "fraction {fraction}");

            // The body's right padding is outside the plot too.
            chart.hover = Some(point(
                plot.origin.x + plot.size.width + px(4.),
                plot.origin.y,
            ));
            assert_eq!(chart.hover_fraction(), None);
        });
    });
}
