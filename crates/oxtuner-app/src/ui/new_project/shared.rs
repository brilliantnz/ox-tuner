//! One-line rows shared by the wizard steps.

use gpui_kit::base::h_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement as _, ParentElement as _, Styled as _, div, px,
};

/// One review line: a fixed label and the value beside it.
pub(super) fn row(label: String, value: String, cx: &App) -> AnyElement {
    h_flex()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .w(px(120.))
                .child(label),
        )
        .child(div().flex_1().text_sm().child(value))
        .into_any_element()
}

pub(super) fn section_title(label: String, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}
