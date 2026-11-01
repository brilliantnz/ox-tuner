//! The newest realtime frame, one compact value per channel.
//!
//! The view reads the shared frame rather than an event payload: a slow repaint
//! shows the newest value, and no frame queues behind the render (ADR-0014).

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
};
use oxtuner_core::LiveValue;

use crate::core::CoreService;
use crate::i18n::t;

/// The channels worth a place in the panel, in this order.
///
/// The definition may declare hundreds of channels; these are the ones a tuner
/// watches while the engine is running. A definition that names none of them
/// falls back to its first few visible channels.
const PREFERRED_CHANNELS: [&str; 12] = [
    "rpm",
    "map",
    "tps",
    "coolant",
    "clt",
    "iat",
    "mat",
    "afr",
    "ego",
    "batteryVoltage",
    "baro",
    "advance",
];

/// The telemetry tab.
#[derive(IntoElement)]
pub(super) struct Telemetry;

impl RenderOnce for Telemetry {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let Some(frame) = CoreService::latest(cx) else {
            return v_flex().child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("app.bottom_panel.no_live_values")),
            );
        };

        let mut chosen: Vec<&LiveValue> = frame
            .values
            .iter()
            .filter(|value| {
                PREFERRED_CHANNELS
                    .iter()
                    .any(|name| value.name.eq_ignore_ascii_case(name))
            })
            .collect();

        if chosen.is_empty() {
            chosen = frame.values.iter().take(10).collect();
        }

        if chosen.is_empty() {
            return v_flex().child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("app.bottom_panel.no_live_values")),
            );
        }

        v_flex()
            .gap_3()
            .child(div().text_xs().text_color(muted).child(t!(
                "app.bottom_panel.frame_summary",
                sequence = frame.sequence.to_string(),
                channels = frame.values.len().to_string(),
            )))
            // A fixed value column keeps the numbers on one reading line as the
            // grid wraps, without turning a handful of channels into a table.
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_6()
                    .gap_y_3()
                    .children(chosen.into_iter().map(|value| {
                        v_flex()
                            .gap_0p5()
                            .w_32()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(value.name.clone()),
                            )
                            .child(
                                h_flex()
                                    .items_baseline()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(value.text.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(value.units.clone()),
                                    ),
                            )
                            .into_any_element()
                    })),
            )
    }
}
