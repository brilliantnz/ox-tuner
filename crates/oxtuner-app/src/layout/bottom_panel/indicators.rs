//! The definition's `[FrontPage]` tuning indicators, evaluated by the core.
//!
//! The tab is display only. It reads the definition summary and the newest
//! realtime frame, zips them by position and hands the result to the shared
//! widget. It never evaluates an expression or reads a tune value itself.

use gpui_kit::base::v_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};
use oxtuner_core::{Failure, FailureCode, IndicatorReading, IndicatorSummary};
use oxtuner_ui::indicator::{IndicatorState, IndicatorStrip, IndicatorView};

use crate::core::CoreService;
use crate::i18n::t;

/// The indicator tab.
#[derive(IntoElement)]
pub(super) struct Indicators;

impl RenderOnce for Indicators {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let status = CoreService::status(cx);
        let failure = status
            .failure
            .as_ref()
            .map(|failure| failure_banner(failure, danger, muted));

        let content = match status.definition() {
            None => empty(t!("app.bottom_panel.no_definition"), muted),
            Some(definition) if definition.indicators.is_empty() => {
                empty(t!("app.bottom_panel.no_indicators"), muted)
            }
            Some(definition) => {
                // The definition already names every card, so the strip is
                // useful before the first frame. Without one, show the off
                // labels, as TunerStudio does, instead of an empty state.
                let views = match CoreService::latest(cx) {
                    Some(frame) => indicator_views(&definition.indicators, &frame.indicators),
                    None => idle_views(&definition.indicators),
                };

                v_flex()
                    .w_full()
                    .child(IndicatorStrip::new(views))
                    .into_any_element()
            }
        };

        v_flex()
            .w_full()
            .gap_2()
            .when_some(failure, |this, failure| this.child(failure))
            .child(content)
    }
}

/// Shows why the last command failed, when one did.
///
/// The tab is the first place a failed definition or project load becomes
/// visible, so the message stays next to the empty strip until a command
/// succeeds.
fn failure_banner(failure: &Failure, danger: Hsla, muted: Hsla) -> AnyElement {
    let heading = match failure.code {
        FailureCode::DefinitionRejected => t!("app.bottom_panel.definition_failed"),
        FailureCode::ProjectRejected => t!("app.bottom_panel.project_failed"),
        FailureCode::TuneSaveFailed => t!("app.bottom_panel.save_failed"),
        FailureCode::NothingToSave => t!("app.bottom_panel.nothing_to_save"),
        FailureCode::TableUnavailable | FailureCode::TableEditRejected => {
            t!("app.bottom_panel.table_failed")
        }
        FailureCode::DialogUnavailable | FailureCode::DialogEditRejected => {
            t!("app.bottom_panel.dialog_failed")
        }
        _ => t!("app.bottom_panel.command_failed"),
    };

    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(danger)
                .child(heading),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(failure.message.clone()),
        )
        .into_any_element()
}

/// Maps the summary and the frame by position.
///
/// A reading the frame does not carry, or one the core could not evaluate,
/// becomes unavailable. The shortest list wins, so a mismatch never shows a
/// stale label as a current state.
fn indicator_views(
    indicators: &[IndicatorSummary],
    readings: &[IndicatorReading],
) -> Vec<IndicatorView> {
    indicators
        .iter()
        .enumerate()
        .map(|(index, indicator)| {
            let (state, label) = match readings.get(index) {
                Some(reading) if reading.available && reading.on => {
                    (IndicatorState::On, &indicator.on_label)
                }
                Some(reading) if reading.available => (IndicatorState::Off, &indicator.off_label),
                _ => (IndicatorState::Unavailable, &indicator.off_label),
            };

            IndicatorView::from_colour_name(
                label.clone(),
                state,
                indicator.on_colour.as_deref().unwrap_or_default(),
            )
        })
        .collect()
}

/// The cards before the first frame arrives: the definition's off labels.
///
/// The definition already declares the indicators, so there is something
/// useful to show while the link is idle. The cards make no live claim; the
/// first frame replaces them with the evaluated state.
fn idle_views(indicators: &[IndicatorSummary]) -> Vec<IndicatorView> {
    indicators
        .iter()
        .map(|indicator| {
            IndicatorView::from_colour_name(
                indicator.off_label.clone(),
                IndicatorState::Off,
                indicator.on_colour.as_deref().unwrap_or_default(),
            )
        })
        .collect()
}

fn empty(message: SharedString, muted: Hsla) -> AnyElement {
    div()
        .text_xs()
        .text_color(muted)
        .child(message)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtuner_ui::indicator::IndicatorTone;

    fn indicator(off_label: &str, on_label: &str) -> IndicatorSummary {
        IndicatorSummary {
            expression: String::new(),
            off_label: off_label.to_owned(),
            on_label: on_label.to_owned(),
            on_colour: None,
            line: 0,
        }
    }

    #[test]
    fn available_readings_pick_the_matching_label() {
        let indicators = [
            indicator("Not Running", "Running"),
            indicator("FLOOD OFF", "FLOOD CLEAR"),
        ];
        let readings = [
            IndicatorReading {
                on: true,
                available: true,
            },
            IndicatorReading {
                on: false,
                available: true,
            },
        ];

        let views = indicator_views(&indicators, &readings);

        assert_eq!(views[0].state, IndicatorState::On);
        assert_eq!(views[0].label.as_ref(), "Running");
        assert_eq!(views[1].state, IndicatorState::Off);
        assert_eq!(views[1].label.as_ref(), "FLOOD OFF");
    }

    #[test]
    fn the_summary_on_colour_reaches_the_view_tone() {
        let mut battery = indicator("Battery Voltage OK", "Battery Voltage Warning");
        battery.on_colour = Some("red".to_owned());
        let readings = [IndicatorReading {
            on: true,
            available: true,
        }];

        let views = indicator_views(&[battery], &readings);

        assert_eq!(views[0].state, IndicatorState::On);
        assert_eq!(views[0].tone, IndicatorTone::Danger);
    }

    #[test]
    fn an_idle_definition_shows_its_off_labels() {
        let indicators = [
            indicator("Not Running", "Running"),
            indicator("FLOOD OFF", "FLOOD CLEAR"),
        ];

        let views = idle_views(&indicators);

        assert_eq!(views[0].state, IndicatorState::Off);
        assert_eq!(views[0].label.as_ref(), "Not Running");
        assert_eq!(views[1].state, IndicatorState::Off);
        assert_eq!(views[1].label.as_ref(), "FLOOD OFF");
    }

    #[test]
    fn an_unavailable_or_missing_reading_makes_no_claim() {
        let indicators = [
            indicator("Not Running", "Running"),
            indicator("FLOOD OFF", "FLOOD CLEAR"),
        ];
        let readings = [IndicatorReading {
            on: false,
            available: false,
        }];

        let views = indicator_views(&indicators, &readings);

        assert_eq!(views[0].state, IndicatorState::Unavailable);
        assert_eq!(views[0].label.as_ref(), "Not Running");
        assert_eq!(views[1].state, IndicatorState::Unavailable);
        assert_eq!(views[1].label.as_ref(), "FLOOD OFF");
    }
}
