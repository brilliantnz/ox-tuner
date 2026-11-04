//! One grid cell.
//!
//! A cell is a value-like element: the table derives everything it shows from
//! the model each frame and hands it over as props. The press and drag
//! handlers stay with the table, which owns the selection; the cell only
//! forwards the raw events.

use std::time::Duration;

use gpui_kit::base::{StyledExt as _, h_flex};
use gpui_kit::{
    Animation, AnimationExt as _, App, ElementId, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, RenderOnce, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    px,
};

use super::geometry::{CELL_GAP, CELL_HEIGHT, CELL_WIDTH};

/// The pointer handler a cell forwards to its table.
pub(crate) type PressHandler = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// The drag handler a cell forwards to its table.
pub(crate) type DragHandler = Box<dyn Fn(&MouseMoveEvent, &mut Window, &mut App)>;

/// The blinking caret of an active edit, shared by the cell and header editors.
///
/// The blink is a hard on/off toggle rather than a fade: a thin line
/// disappears below about half opacity, so a fade reads as jitter instead of
/// a blink. The animation id is shared, so every caret on the grid blinks in
/// step.
pub(crate) fn caret(color: Hsla) -> impl IntoElement {
    div()
        .w(px(1.))
        .h(px(12.))
        .flex_shrink_0()
        .bg(color)
        .with_animation(
            "edit-caret",
            Animation::new(Duration::from_millis(1060))
                .repeat()
                .with_max_fps(10.),
            |caret, delta| caret.opacity(if delta < 0.5 { 1.0 } else { 0.0 }),
        )
}

/// The in-cell editor overlay, drawn while the user types into the cell.
#[derive(Clone, Debug)]
pub(crate) struct CellEditing {
    /// The text as typed.
    pub(crate) text: SharedString,
    /// Whether the last commit attempt could not parse the text.
    pub(crate) invalid: bool,
}

/// One heatmap cell of the table body.
#[derive(IntoElement)]
pub(crate) struct TableCell {
    pub(crate) id: ElementId,
    pub(crate) display: SharedString,
    pub(crate) aria: SharedString,
    pub(crate) fill: Hsla,
    pub(crate) text_color: Hsla,
    /// The theme's danger colour, for an unparsable edit.
    pub(crate) danger: Hsla,
    /// The theme's info colour, for the realtime dot.
    pub(crate) info: Hsla,
    pub(crate) editing: Option<CellEditing>,
    /// The write-state dot's colour, or `None` for no mark.
    pub(crate) mark: Option<Hsla>,
    /// Whether the realtime operating point sits on this cell.
    pub(crate) realtime: bool,
    pub(crate) on_press: PressHandler,
    pub(crate) on_drag: DragHandler,
}

impl RenderOnce for TableCell {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self {
            id,
            display,
            aria,
            fill,
            text_color,
            danger,
            info,
            editing,
            mark,
            realtime,
            on_press,
            on_drag,
        } = self;

        let mut element = div()
            .id(id)
            .test_support()
            .relative()
            .w(px(CELL_WIDTH))
            .h(px(CELL_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .px_1()
            .bg(fill)
            .text_xs()
            .text_color(text_color)
            .font_semibold()
            .role(Role::GridCell)
            .aria_label(aria)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                (on_press)(event, window, cx);
            })
            .on_mouse_move(move |event, window, cx| (on_drag)(event, window, cx));

        if let Some(editing) = editing {
            element = element
                .border_2()
                .border_color(if editing.invalid { danger } else { text_color })
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(CELL_GAP))
                        .child(editing.text)
                        .child(caret(text_color)),
                );
        } else {
            element = element.child(display);
        }

        if let Some(color) = mark {
            element = element.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(2.))
                    .w(px(3.))
                    .h(px(3.))
                    .rounded_full()
                    .border_1()
                    .border_color(text_color)
                    .bg(color),
            );
        }

        if realtime {
            element = element.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .right(px(2.))
                    .w(px(5.))
                    .h(px(5.))
                    .rounded_full()
                    .border_1()
                    .border_color(text_color)
                    .bg(info),
            );
        }

        element
    }
}
