//! One modal at a time, dismissed only by its own controls.
//!
//! Two rules live here because every dialog needs both and they are easy to
//! get wrong per call site:
//!
//! - **Never stack.** A menu item or a shortcut still fires while a dialog is
//!   open, so opening another copy would hide the first one under an identical
//!   frame. The open dialog owns the interaction until it closes.
//! - **No accidental dismissal.** Clicking the backdrop and pressing Escape are
//!   off; the close icon and an explicit button are the only ways out, so a
//!   stray click cannot drop a half-filled connection form.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::{AlertDialog, Dialog};
use gpui_kit::{App, Window};

/// Opens `build` as the window's modal, unless one is already open.
pub(crate) fn open(
    window: &mut Window,
    cx: &mut App,
    build: impl Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
) -> bool {
    if window.has_active_dialog(cx) {
        return false;
    }
    if cx.has_global::<crate::core::CoreService>() {
        crate::core::CoreService::status_mut(cx).dialog_owner = None;
    }
    window.open_dialog(cx, move |dialog, window, cx| {
        build(
            dialog
                .overlay_closable(false)
                .keyboard(false)
                .close_button(true),
            window,
            cx,
        )
    });
    true
}

/// Opens `build` as the window's alert dialog, under the same rules.
pub(crate) fn open_alert(
    window: &mut Window,
    cx: &mut App,
    build: impl Fn(AlertDialog, &mut Window, &mut App) -> AlertDialog + 'static,
) -> bool {
    if window.has_active_dialog(cx) {
        return false;
    }
    window.open_alert_dialog(cx, move |alert, window, cx| {
        // An alert's backdrop dismissal is disabled by the library itself;
        // Escape is the one door this helper still has to close.
        build(alert.keyboard(false), window, cx)
    });
    true
}
