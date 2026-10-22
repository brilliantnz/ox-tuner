//! The secondary sidebar: details about whatever the workspace has selected.
//!
//! Unlike the primary sidebar it does **not** reach the window's top edge: it
//! starts under the main bar, because the bar spans the whole editor area and
//! the inspector belongs to the workspace rather than to the window chrome. It
//! is a resizable panel that can be closed from the toolbar or its shortcut,
//! and it keeps its width while closed.

use gpui_kit::base::v_flex;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
};

use crate::i18n::t;

/// The inspector column.
#[derive(IntoElement)]
pub(crate) struct SecondarySidebar;

impl RenderOnce for SecondarySidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            // A header row rather than a drag strip: this column starts below
            // the main bar, which is already the window's drag region here.
            .child(
                gpui_kit::base::h_flex()
                    .h_8()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().sidebar_border)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("app.secondary_sidebar.title"))
                    .child(
                        Button::new("close-secondary-sidebar")
                            .icon(IconName::Close)
                            .ghost()
                            .small()
                            .tooltip(t!("app.secondary_sidebar.close_tooltip"))
                            .on_click(|_, _, cx| super::toggle_secondary_sidebar(cx)),
                    ),
            )
            .child(
                v_flex()
                    .px_3()
                    .pt_2()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(t!("app.secondary_sidebar.nothing_selected")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("app.secondary_sidebar.select_hint")),
                    ),
            )
    }
}
