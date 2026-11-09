//! A constant value as a single-line input.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, Focusable as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, TextAlign, Window, div,
};
use oxtuner_ui::table::parse_edit;

use crate::i18n::t;

/// Whether an input holds a number or free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputKind {
    /// A number, right-aligned and validated as one.
    Number,
    /// A string the definition stores, shown read-only until the core can
    /// write it.
    Text,
}

/// One constant row's text or number input.
///
/// The value is a plain input rather than the stepper control: a dialog row
/// shows its value in a fixed trailing lane, and a pair of step buttons on
/// every row would read as a toolbar repeated down the page. The input still
/// validates numbers, so a letter never parses as one.
pub(crate) struct ConstantInput {
    state: Entity<InputState>,
}

impl ConstantInput {
    /// An input seeded with `text`, right-aligned when the value is numeric.
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut App,
        text: SharedString,
        kind: InputKind,
    ) -> Self {
        let numeric = kind == InputKind::Number;

        let state = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .default_value(text)
                .placeholder(t!("app.dialog.no_value"));

            if numeric {
                state.set_text_align(TextAlign::Right, cx);
                state = state.validate(|text, _| {
                    let text = text.trim();

                    text.is_empty() || parse_edit(text).is_ok()
                });
            }

            state
        });

        Self { state }
    }

    /// The entity the editor subscribes to for commits and focus.
    pub(crate) fn state(&self) -> &Entity<InputState> {
        &self.state
    }

    /// Whether the user is typing into this row.
    pub(crate) fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Writes a new display value without emitting a change event.
    ///
    /// The editor calls this when a snapshot arrives; a focused row keeps the
    /// text the user is typing instead of having it replaced mid-edit.
    pub(crate) fn set_text(&self, text: SharedString, window: &mut Window, cx: &mut App) {
        if self.state.read(cx).value() == text {
            return;
        }

        self.state
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    /// The input element, with its units inside the frame.
    pub(crate) fn render(
        &self,
        units: SharedString,
        enabled: bool,
        editable: bool,
        cx: &App,
    ) -> AnyElement {
        let mut input = Input::new(&self.state)
            .w_full()
            .disabled(!enabled)
            .readonly(!editable);

        if !units.is_empty() {
            input = input.suffix(
                div()
                    .pl_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(units),
            );
        }

        input.into_any_element()
    }
}
