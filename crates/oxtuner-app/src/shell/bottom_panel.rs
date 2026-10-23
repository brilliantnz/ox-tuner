//! The bottom panel: output and, later, the other workspace consoles.
//!
//! It sits inside the editor column (not across the secondary sidebar), so
//! closing it gives the height back to the editor only. Its height is a
//! resizable-panel size and its open state is session state, so both survive a
//! restart.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window,
    div,
};
use oxtuner_core::FailureCode;

use crate::core::CoreService;
use crate::i18n::t;

/// The output panel.
#[derive(IntoElement)]
pub(crate) struct BottomPanel;

impl RenderOnce for BottomPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .h_8()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("app.bottom_panel.title"))
                    // Closing from the panel itself is the obvious affordance;
                    // the same command has a shortcut and a toolbar button, all
                    // through one action.
                    .child(
                        Button::new("close-bottom-panel")
                            .icon(IconName::Close)
                            .ghost()
                            .small()
                            .tooltip(t!("app.bottom_panel.close_tooltip"))
                            .on_click(|_, _, cx| super::toggle_bottom_panel(cx)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .justify_center()
                    .gap_1()
                    .px_3()
                    .child(core_status(cx)),
            )
    }
}

/// What the core currently holds.
///
/// The output panel is where the app reports what the headless core is doing, so
/// the definition status lives here rather than in the editor: the editor is for
/// the tune, this panel is for the machinery behind it.
fn core_status(cx: &App) -> AnyElement {
    let status = CoreService::status(cx);
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    let headline: AnyElement = match status.definition() {
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
            .text_sm()
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
                    .children(
                        project.warnings.iter().take(3).map(|warning| {
                            div().text_xs().text_color(muted).child(warning.clone())
                        }),
                    )
                })
                .when(project.read_only, |this| {
                    this.child(div().text_xs().text_color(danger).child(t!(
                        "app.bottom_panel.read_only",
                        reason = project.read_only_reasons.join("; "),
                    )))
                })
        });

    let saved = status.last_save.as_ref().map(|outcome| {
        div().text_xs().text_color(muted).child(t!(
            "app.bottom_panel.saved",
            cells = outcome.cells.to_string(),
            tune = outcome.path.display().to_string(),
        ))
    });

    let failure = status.failure.as_ref().map(|failure| {
        let heading = match failure.code {
            FailureCode::DefinitionRejected => t!("app.bottom_panel.definition_failed"),
            FailureCode::ProjectRejected => t!("app.bottom_panel.project_failed"),
            FailureCode::TuneSaveFailed => t!("app.bottom_panel.save_failed"),
            FailureCode::NothingToSave => t!("app.bottom_panel.nothing_to_save"),
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
    });

    v_flex()
        .gap_1()
        .child(headline)
        .when_some(tune, |this, tune| this.child(tune))
        .when_some(saved, |this, saved| this.child(saved))
        .when_some(failure, |this, failure| this.child(failure))
        .into_any_element()
}
