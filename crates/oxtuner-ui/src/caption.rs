//! The stacked Y captions shared by the table and the curve.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};

/// Line height of one stacked caption character, in pixels.
const LINE_HEIGHT: f32 = 13.0;
/// Height of the gap between two words in a stacked caption, in pixels.
const WORD_GAP: f32 = 6.0;
/// Space between the stacked name and its units, in pixels.
const UNITS_GAP: f32 = 4.0;
/// Font size of the units, in pixels. Slightly smaller than the name so the
/// units read as a subordinate part of the caption.
const UNITS_TEXT: f32 = 10.0;

/// A Y axis caption with its name stacked one character per line and its
/// units kept whole below.
///
/// Glyphs cannot be rotated through the styling API, so a vertical caption is
/// stacked. Squeezing `(%)` through that stack turns the parentheses and the
/// unit into three unrelated lines, which reads badly. The unit stays a single
/// horizontal word instead.
pub(crate) fn y_caption(label: &str, units: &str, width: f32, color: Hsla) -> AnyElement {
    let rows = label
        .chars()
        .map(|character| {
            if character.is_whitespace() {
                div().h(px(WORD_GAP))
            } else {
                div().h(px(LINE_HEIGHT)).child(character.to_string())
            }
        })
        .collect::<Vec<_>>();

    div()
        .w(px(width))
        .flex_shrink_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(color)
        .children(rows)
        .when(!units.is_empty(), |this| {
            this.child(
                div()
                    .mt(px(UNITS_GAP))
                    .text_size(px(UNITS_TEXT))
                    .whitespace_nowrap()
                    .child(SharedString::from(units.to_owned())),
            )
        })
        .into_any_element()
}
