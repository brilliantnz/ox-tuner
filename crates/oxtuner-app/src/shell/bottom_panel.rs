//! The bottom panel: output and, later, the other workspace consoles.
//!
//! It sits inside the editor column (not across the secondary sidebar), so
//! closing it gives the height back to the editor only. Its height is a
//! resizable-panel size and its open state is session state, so both survive a
//! restart.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::{App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window};

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
                    .items_center()
                    .justify_center()
                    .px_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("app.bottom_panel.empty")),
            )
    }
}
