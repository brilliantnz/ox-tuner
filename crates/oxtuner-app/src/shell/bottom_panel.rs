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

    let headline: AnyElement = match &status.summary {
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

    v_flex()
        .gap_1()
        .child(headline)
        .when_some(status.failure.clone(), |this, message| {
            this.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(danger)
                            .child(t!("app.bottom_panel.definition_failed")),
                    )
                    // The parser's message names lines and sections; showing it
                    // verbatim is what makes a bad definition fixable.
                    .child(div().text_xs().text_color(muted).child(message)),
            )
        })
        .into_any_element()
}
