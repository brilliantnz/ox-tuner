//! What the core currently holds: definition, tune, link and write access.
//!
//! These are standing facts, not the outcome of the last command: the tune that
//! is open, the session that is up and whether a write may start. Command
//! results live on the log tab, so this view does not grow a timeline.
//!
//! The view reads the status the event loop folded; it never renders an event
//! payload, so a slow repaint shows the current state rather than replaying
//! history.

use gpui_kit::base::v_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};
use oxtuner_core::FailureCode;

use crate::core::CoreService;
use crate::i18n::t;

/// The indicator tab.
#[derive(IntoElement)]
pub(super) struct Indicators;

impl RenderOnce for Indicators {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let status = CoreService::status(cx);
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;

        let definition = match status.definition() {
            Some(summary) => v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(summary.signature.clone()),
                )
                .child(div().text_xs().text_color(muted).child(t!(
                    "app.bottom_panel.definition_summary",
                    pages = summary.pages.to_string(),
                    constants = summary.constants.to_string(),
                    coverage = format!("{:.2}", summary.coverage_percent),
                )))
                .into_any_element(),
            None => div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.bottom_panel.no_definition"))
                .into_any_element(),
        };

        let tune =
            status.project.as_ref().map(|project| {
                v_flex()
                    .gap_1()
                    .child(div().text_xs().text_color(muted).child(t!(
                        "app.bottom_panel.tune_summary",
                        loaded = project.loaded.to_string(),
                        tune = project.tune_path.display().to_string(),
                    )))
                    .when(!project.warnings.is_empty(), |this| {
                        this.child(div().text_xs().text_color(muted).child(t!(
                            "app.bottom_panel.tune_warnings",
                            count = project.warnings.len().to_string(),
                        )))
                        // The load gate's findings name the variables involved; they
                        // are the difference between "it loaded" and "it is usable".
                        .children(project.warnings.iter().take(3).map(|warning| {
                            div().text_xs().text_color(muted).child(warning.clone())
                        }))
                    })
                    .when(project.read_only, |this| {
                        this.child(div().text_xs().text_color(danger).child(t!(
                            "app.bottom_panel.read_only",
                            reason = project.read_only_reasons.join("; "),
                        )))
                    })
                    .into_any_element()
            });

        let link = (status.connection.is_some()
            || status.link_lost.is_some()
            || status.pages_read.is_some())
        .then(|| {
            v_flex()
                .gap_1()
                .when_some(status.connection.as_ref(), |this, connection| {
                    this.child(div().text_xs().font_weight(FontWeight::MEDIUM).child(t!(
                        "app.bottom_panel.connected",
                        signature = connection.signature.clone(),
                        port = connection.port.clone(),
                        framing = connection.framing.to_owned(),
                    )))
                    .when(connection.read_only, |this| {
                        this.child(div().text_xs().text_color(danger).child(t!(
                            "app.bottom_panel.ecu_read_only",
                            reason = connection.read_only_reason.clone().unwrap_or_default(),
                        )))
                    })
                })
                .when_some(status.link_lost.as_ref(), |this, reason| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(danger)
                            .child(t!("app.bottom_panel.link_lost", reason = reason.clone(),)),
                    )
                })
                .when_some(status.pages_read.as_ref(), |this, read| {
                    this.child(div().text_xs().text_color(muted).child(t!(
                        "app.bottom_panel.pages_read",
                        pages = read.pages.to_string(),
                        bytes = read.bytes.to_string(),
                        constants = read.constants.to_string(),
                    )))
                })
                .into_any_element()
        });

        let writes = if let Some(reason) = &status.write_halted {
            div()
                .text_xs()
                .text_color(danger)
                .child(t!("app.bottom_panel.write_halted", reason = reason.clone()))
        } else if status.writes_enabled {
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.bottom_panel.writes_on"))
        } else {
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.bottom_panel.writes_off"))
        };

        let failure = status.failure.as_ref().map(|failure| {
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
                // The core's message names paths, lines and sections; showing it
                // verbatim is what makes a failure fixable.
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(failure.message.clone()),
                )
                .into_any_element()
        });

        v_flex()
            .gap_3()
            .child(section(
                t!("app.bottom_panel.section_definition"),
                definition,
            ))
            .when_some(tune, |this, tune| {
                this.child(section(t!("app.bottom_panel.section_tune"), tune))
            })
            .when_some(link, |this, link| {
                this.child(section(t!("app.bottom_panel.section_link"), link))
            })
            .child(section(
                t!("app.bottom_panel.section_write"),
                writes.into_any_element(),
            ))
            .when_some(failure, |this, failure| this.child(failure))
    }
}

/// One titled group of indicator lines.
fn section(title: SharedString, body: AnyElement) -> AnyElement {
    v_flex()
        .gap_1()
        .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(title))
        .child(body)
        .into_any_element()
}
