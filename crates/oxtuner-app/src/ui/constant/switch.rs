//! A boolean constant as a switch.

use gpui_kit::component::Disableable as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::{App, ElementId, SharedString, Window};

/// A switch for a row whose definition stores a zero or one.
///
/// The caller owns the value: the switch reports the requested checked state
/// and the next snapshot draws the stored one.
pub(crate) fn control(
    id: impl Into<ElementId>,
    checked: bool,
    enabled: bool,
    label: SharedString,
    on_change: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> Switch {
    Switch::new(id)
        .checked(checked)
        .disabled(!enabled)
        .accessibility_label(label)
        .on_change(on_change)
}
