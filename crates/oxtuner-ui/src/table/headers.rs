//! The axis headers: column bins across the top or bottom band, row bins down
//! the frozen left column.
//!
//! Each header is a value-like element like a cell. A double press starts a
//! bin edit when the table is writable; the table owns that rule and forwards
//! only the raw event here.

use gpui_kit::base::{StyledExt as _, h_flex};
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

/// The wash that ties a bin to a selected cell.
///
/// It is an overlay inside the bin, not a border or an inset: the bin keeps
/// its exact size and its label stays where it was, and the tint reads the
/// same in both themes because it is drawn over the bin's own background.
#[derive(Clone, Debug)]
pub(crate) struct BinHighlight {
    /// Element id, so a test can find the wash.
    pub(crate) id: ElementId,
    /// The translucent colour painted over the bin.
    pub(crate) color: Hsla,
}

/// One bin of the X axis, in the column-header band.
#[derive(IntoElement)]
pub(crate) struct ColumnHeader {
    pub(crate) id: ElementId,
    pub(crate) label: SharedString,
    pub(crate) aria: SharedString,
    pub(crate) background: Hsla,
    /// The neutral foreground, for the bin editor frame and caret.
    pub(crate) foreground: Hsla,
    /// The theme's danger colour, for an unparsable edit.
    pub(crate) danger: Hsla,
    /// The selection wash, when a single selected cell sits in this bin's
    /// column, or a range covers it and the owner asked for the wider wash.
    pub(crate) highlight: Option<BinHighlight>,
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
            foreground,
            danger,
            highlight,
            editing,
            on_press,
        } = self;

        let editing_text = editing.as_ref().map(|editing| editing.text.clone());
        let editing_invalid = editing.as_ref().is_some_and(|editing| editing.invalid);

        let mut element = div()
            .id(id)
            .test_support()
            .relative()
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
                (on_press)(event, window, cx);
            });

        // Painted under the text so the bin keeps its full contrast, and
        // absolutely positioned so it neither moves the label nor changes
        // the bin's size.
        if let Some(highlight) = highlight {
            element = element.child(
                div()
                    .id(highlight.id)
                    .test_support()
                    .absolute()
                    .inset_0()
                    .bg(highlight.color),
            );
        }

        // The bin editor reads like the cell editor: the same frame around
        // the typed text, but a neutral foreground instead of the heatmap's
        // contrasting colour.
        if let Some(text) = editing_text {
            element = element
                .border_2()
                .border_color(if editing_invalid { danger } else { foreground })
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(CELL_GAP))
                        .child(text)
                        .child(caret(foreground)),
                );
        } else {
            element = element.child(label);
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
    /// The neutral foreground, for the bin editor frame and caret.
    pub(crate) foreground: Hsla,
    /// The theme's danger colour, for an unparsable edit.
    pub(crate) danger: Hsla,
    /// The selection wash, when a single selected cell sits in this bin's row,
    /// or a range covers it and the owner asked for the wider wash.
    pub(crate) highlight: Option<BinHighlight>,
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
            foreground,
            danger,
            highlight,
            editing,
            on_press,
        } = self;

        let editing_text = editing.as_ref().map(|editing| editing.text.clone());
        let editing_invalid = editing.as_ref().is_some_and(|editing| editing.invalid);

        let mut element = div()
            .id(id)
            .test_support()
            .relative()
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
                (on_press)(event, window, cx);
            });

        // Painted under the text so the bin keeps its full contrast, and
        // absolutely positioned so it neither moves the label nor changes
        // the bin's size.
        if let Some(highlight) = highlight {
            element = element.child(
                div()
                    .id(highlight.id)
                    .test_support()
                    .absolute()
                    .inset_0()
                    .bg(highlight.color),
            );
        }

        // Same frame as the cell editor, in the neutral foreground.
        if let Some(text) = editing_text {
            element = element
                .border_2()
                .border_color(if editing_invalid { danger } else { foreground })
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(CELL_GAP))
                        .child(text)
                        .child(caret(foreground)),
                );
        } else {
            element = element.child(label);
        }

        element
    }
}
