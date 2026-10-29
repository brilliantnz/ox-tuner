//! The axis headers: column bins across the top or bottom band, row bins down
//! the frozen left column.
//!
//! Each header is a value-like element like a cell. A double press starts a
//! bin edit when the table is writable; the table owns that rule and forwards
//! only the raw event here.

use gpui_kit::base::StyledExt as _;
use gpui_kit::{
    App, ElementId, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    RenderOnce, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};

use super::cell::{PressHandler, caret};
use super::geometry::{AXIS_HEIGHT, AXIS_WIDTH, CELL_GAP, CELL_HEIGHT, CELL_WIDTH};

/// The in-header editor overlay, drawn while the user types into a bin.
#[derive(Clone, Debug)]
pub(crate) struct HeaderEditing {
    /// The text as typed.
    pub(crate) text: SharedString,
    /// Whether the last commit attempt could not parse the text.
    pub(crate) invalid: bool,
}

/// One bin of the X axis, in the column-header band.
#[derive(IntoElement)]
pub(crate) struct ColumnHeader {
    pub(crate) id: ElementId,
    pub(crate) label: SharedString,
    pub(crate) aria: SharedString,
    pub(crate) background: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) danger: Hsla,
    pub(crate) editing: Option<HeaderEditing>,
    pub(crate) on_press: PressHandler,
}

impl RenderOnce for ColumnHeader {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self {
            id,
            label,
            aria,
            background,
            muted,
            danger,
            editing,
            on_press,
        } = self;

        let editing_text = editing.as_ref().map(|editing| editing.text.clone());
        let editing_invalid = editing.as_ref().is_some_and(|editing| editing.invalid);

        let mut element = div()
            .id(id)
            .test_support()
            .w(px(CELL_WIDTH))
            .h(px(AXIS_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .bg(background)
            .text_xs()
            .font_semibold()
            .role(Role::ColumnHeader)
            .aria_label(aria)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                (on_press)(event, window, cx)
            });

        if let Some(text) = editing_text {
            element = element.gap(px(CELL_GAP)).child(text).child(caret(muted));
        } else {
            element = element.child(label);
        }

        if editing_invalid {
            element = element.border_1().border_color(danger);
        } else if editing.is_some() {
            element = element.border_1().border_color(muted);
        }
        element
    }
}

/// One bin of the Y axis, in the frozen row-header column.
#[derive(IntoElement)]
pub(crate) struct RowHeader {
    pub(crate) id: ElementId,
    pub(crate) label: SharedString,
    pub(crate) aria: SharedString,
    pub(crate) background: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) danger: Hsla,
    pub(crate) editing: Option<HeaderEditing>,
    pub(crate) on_press: PressHandler,
}

impl RenderOnce for RowHeader {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self {
            id,
            label,
            aria,
            background,
            muted,
            danger,
            editing,
            on_press,
        } = self;

        let editing_text = editing.as_ref().map(|editing| editing.text.clone());
        let editing_invalid = editing.as_ref().is_some_and(|editing| editing.invalid);

        let mut element = div()
            .id(id)
            .test_support()
            .w(px(AXIS_WIDTH))
            .h(px(CELL_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .px_2()
            .bg(background)
            .text_xs()
            .font_semibold()
            .role(Role::RowHeader)
            .aria_label(aria)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                (on_press)(event, window, cx)
            });

        if let Some(text) = editing_text {
            element = element.gap(px(CELL_GAP)).child(text).child(caret(muted));
        } else {
            element = element.child(label);
        }

        if editing_invalid {
            element = element.border_1().border_color(danger);
        } else if editing.is_some() {
            element = element.border_1().border_color(muted);
        }
        element
    }
}
