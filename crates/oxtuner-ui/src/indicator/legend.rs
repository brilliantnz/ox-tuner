//! The compact state legend for the indicator strip.
//!
//! The legend is deliberately not a row of cards: a coloured circle and a
//! short label are enough to explain the states. The consumer owns the panel
//! around it, just as it owns the panel around the strip.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, Hsla, IntoElement, ParentElement as _, Styled as _, div, px};

use super::IndicatorStyle;

/// One state key: a circle in the state colour and a short label.
///
/// `hollow` draws the circle as an outline, for the unavailable state. The
/// label is static copy for a fixed key; a consumer that needs different text
/// composes its own row from these items.
pub fn legend(label: &'static str, dot: Hsla, hollow: bool, style: &IndicatorStyle) -> AnyElement {
    let dot = div()
        .w(style.dot_size)
        .h(style.dot_size)
        .rounded_full()
        .flex_shrink_0()
        .when(hollow, |this| {
            this.border_1().border_color(style.inactive_dot)
        })
        .when(!hollow, |this| this.bg(dot));

    div()
        .flex()
        .items_center()
        .gap(style.dot_gap)
        .text_size(style.label_size)
        .text_color(style.inactive_text)
        .child(dot)
        .child(label)
        .into_any_element()
}

/// The standard five-state key, ready to place under a strip.
///
/// The labels and the order are design constants. A consumer that needs
/// different copy can compose [`legend`] items itself.
pub fn state_legend(style: &IndicatorStyle) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(16.))
        .child(legend("Off", style.inactive_dot, false, style))
        .child(legend("Ok", style.ok_dot, false, style))
        .child(legend("Warning", style.warning_dot, false, style))
        .child(legend("Danger", style.danger_dot, false, style))
        .child(legend("Unavailable", style.inactive_dot, true, style))
        .into_any_element()
}
