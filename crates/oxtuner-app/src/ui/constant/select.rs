//! A constant with definition-supplied labels as a select.

use gpui_kit::component::IndexPath;
use gpui_kit::component::select::{Select, SelectItem, SelectState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, Focusable as _, IntoElement, SharedString,
    Styled as _, Window,
};
use oxtuner_core::DialogSelectOption;

/// One option of a `bits` constant, carrying the raw value and the label.
///
/// The label is what a tuner recognizes; the number is what the tune stores.
/// Keeping both means the select writes the definition's value rather than the
/// option's position in the list.
#[derive(Debug, Clone)]
pub(crate) struct ConstantOption {
    value: u32,
    label: SharedString,
    search: String,
}

impl SelectItem for ConstantOption {
    type Value = u32;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }

    fn matches(&self, query: &str) -> bool {
        self.search.contains(&query.to_lowercase())
    }
}

/// One constant row's option picker.
pub(crate) struct ConstantSelect {
    state: Entity<SelectState<Vec<ConstantOption>>>,
    /// The options the picker currently holds, including any raw fallback
    /// added for a value the definition has no label for.
    items: Vec<ConstantOption>,
}

impl ConstantSelect {
    /// A select over `options`, positioned at the current raw value.
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut App,
        options: &[DialogSelectOption],
        selected: Option<u32>,
    ) -> Self {
        let mut items: Vec<ConstantOption> = options
            .iter()
            .filter(|option| !option.label.trim().is_empty())
            .map(|option| ConstantOption {
                value: option.value,
                label: option.label.clone().into(),
                search: option.label.to_lowercase(),
            })
            .collect();

        // A raw value the definition has no label for still has to be visible.
        // Showing the number is honest; a blank trigger would hide the state.
        if let Some(value) = selected
            && !items.iter().any(|option| option.value == value)
        {
            items.push(ConstantOption {
                value,
                label: value.to_string().into(),
                search: value.to_string(),
            });
        }

        let index = selected
            .and_then(|value| items.iter().position(|option| option.value == value))
            .map(IndexPath::new);

        let state = cx.new(|cx| SelectState::new(items.clone(), index, window, cx));

        Self { state, items }
    }

    /// The entity the editor subscribes to for confirms and focus.
    pub(crate) fn state(&self) -> &Entity<SelectState<Vec<ConstantOption>>> {
        &self.state
    }

    /// The raw value the select shows, if the current value names an option.
    pub(crate) fn selected(&self, cx: &App) -> Option<u32> {
        self.state.read(cx).selected_value().copied()
    }

    /// Whether the picker or its open list currently has focus.
    pub(crate) fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Moves the picker to `value` without emitting a confirm.
    pub(crate) fn set_selected(&mut self, value: Option<u32>, window: &mut Window, cx: &mut App) {
        if self.selected(cx) == value {
            return;
        }

        if let Some(value) = value
            && !self.items.iter().any(|option| option.value == value)
        {
            // The core can move a shared constant to a value this definition's
            // list does not name. Add the raw value rather than blanking the
            // trigger.
            self.items.push(ConstantOption {
                value,
                label: value.to_string().into(),
                search: value.to_string(),
            });
            let items = self.items.clone();
            self.state
                .update(cx, |state, cx| state.set_items(items, window, cx));
        }

        self.state.update(cx, |state, cx| match value {
            Some(value) => state.set_selected_value(&value, window, cx),
            None => state.set_selected_index(None, window, cx),
        });
    }

    /// The select element.
    pub(crate) fn render(&self, enabled: bool, label: SharedString) -> AnyElement {
        Select::new(&self.state)
            .w_full()
            .disabled(!enabled)
            .accessibility_label(label)
            .into_any_element()
    }
}
