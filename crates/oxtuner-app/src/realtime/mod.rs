//! The realtime workspace: the reusable chart over the core's shared history.
//!
//! The view owns only presentation: window buttons, the record toggle and the
//! channel picker. Acquisition, the ring buffer and the CSV writer all live in
//! the core (ADR-0014); this view reads what exists on its render tick.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px,
};
use oxtuner_ui::chart::{ChartConfig, ChartState, ChartStyle};

use crate::core::{self, CoreService};
use crate::i18n::t;

mod channels;

/// The chart windows offered in the toolbar: seconds, label and stable id.
const WINDOW_CHOICES: [(f64, &str, &str); 3] = [
    (10.0, "10s", "realtime-window-10"),
    (30.0, "30s", "realtime-window-30"),
    (60.0, "60s", "realtime-window-60"),
];

/// The chart tokens from the application theme.
fn chart_style(theme: &gpui_kit::component::Theme) -> ChartStyle {
    ChartStyle {
        background: theme.background,
        surface: theme.popover,
        surface_alt: theme.muted,
        foreground: theme.foreground,
        muted: theme.muted_foreground,
        border: theme.border,
        grid: theme.chart_grid,
        popover: theme.popover,
        popover_foreground: theme.popover_foreground,
        accent: theme.chart_1,
        danger: theme.danger,
        series: vec![
            theme.chart_1,
            theme.chart_2,
            theme.chart_3,
            theme.chart_4,
            theme.chart_5,
        ],
        radius: theme.radius_lg,
    }
}

/// The live-chart workspace.
pub(crate) struct RealtimeView {
    chart: Entity<ChartState>,
}

impl RealtimeView {
    /// Builds the chart over the core's shared history. One instance for the
    /// lifetime of the window keeps pause, legend and window selection across
    /// workspace switches.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let history = CoreService::history(cx);
        let chart = cx.new(move |cx| {
            let mut chart =
                ChartState::with_config(history, ChartConfig::new().with_window(30.0), cx);
            chart.set_style(chart_style(cx.theme()), cx);
            chart
        });
        Self { chart }
    }

    /// The realtime toolbar: window buttons, channel picker and recording.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = CoreService::status(cx);
        let log = CoreService::log(cx);
        let connected = status.connection.is_some();
        let recording = log.running;
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let log_label = if recording {
            t!("app.realtime.stop_log")
        } else {
            t!("app.realtime.start_log")
        };
        let window = self.chart.read(cx).window_secs();

        h_flex()
            .h_9()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(t!("app.realtime.title")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if connected { muted } else { danger })
                    .child(if connected {
                        t!("app.realtime.connected")
                    } else {
                        t!("app.realtime.not_connected")
                    }),
            )
            .child(div().w(px(1.)).h_4().bg(cx.theme().border))
            .children(WINDOW_CHOICES.into_iter().map(|(seconds, label, id)| {
                Button::new(id)
                    .label(SharedString::new_static(label))
                    .small()
                    .ghost()
                    .selected((window - seconds).abs() < 0.01)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.chart.update(cx, |chart, cx| {
                            chart.set_window_secs(seconds, cx);
                        });
                    }))
            }))
            .child(
                Button::new("realtime-channels")
                    .label(t!("app.realtime.channels"))
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| {
                        channels::show(window, cx, this.chart.clone());
                    })),
            )
            .child(
                Button::new("realtime-log")
                    .label(log_label)
                    .small()
                    .ghost()
                    .selected(recording)
                    .disabled(!connected)
                    .on_click(|_, _, cx: &mut App| {
                        if CoreService::log(cx).running {
                            core::stop_log(cx);
                        } else {
                            core::start_log(cx);
                        }
                        cx.refresh_windows();
                    }),
            )
            .child(div().flex_1())
            .child(
                v_flex()
                    .items_end()
                    .gap_0p5()
                    .when_some(log.path.clone(), |this, path| {
                        this.child(div().text_xs().text_color(muted).child(SharedString::from(
                            format!(
                                "{} · {} frames{}",
                                path.display(),
                                log.frames,
                                if log.dropped > 0 {
                                    format!(" · {} dropped", log.dropped)
                                } else {
                                    String::new()
                                }
                            ),
                        )))
                    })
                    .when(log.dropped > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.realtime.dropped", count = log.dropped.to_string())),
                        )
                    }),
            )
    }
}

impl Render for RealtimeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = chart_style(cx.theme());
        self.chart
            .update(cx, |chart, cx| chart.set_style(style, cx));
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p_2()
                    .child(self.chart.clone()),
            )
    }
}
