//! The safety disclaimer.
//!
//! Shown on first run and reachable again from Help. Acceptance is stored in
//! `settings.safety.disclaimer_accepted`, so the first-run dialog appears once
//! per installation rather than once per window.

use gpui_kit::base::v_flex;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, WindowExt as _};
use gpui_kit::{App, ParentElement as _, Styled as _, Window, div, px};
use rust_i18n::t;

use crate::settings::preferences::AppSettings;

/// Opens the disclaimer in `window`.
///
/// An alert dialog, not a plain dialog: the alert builds its footer button from
/// `button_props`, while a plain `Dialog` renders no buttons at all and only
/// wires those props to the Confirm/Cancel actions — which is why the first
/// version could only be dismissed with Enter.
///
/// Neither the overlay, the close button, nor Escape dismisses it: acknowledging
/// it is the one way past the gate. Closing the whole window still works, and
/// the dialog returns on the next launch because acceptance was never recorded.
pub(crate) fn show(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |alert, _, cx| {
        let muted = cx.theme().muted_foreground;

        alert
            .title(t!("app.disclaimer.title"))
            .width(px(520.))
            .icon(Icon::new(IconName::TriangleAlert))
            .ok_text(t!("app.disclaimer.ok_text"))
            .show_cancel(false)
            .close_button(false)
            .keyboard(false)
            .description(t!("app.disclaimer.warning"))
            .child(
                v_flex().gap_2().child(t!("app.disclaimer.rule")).child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(t!("app.disclaimer.acceptance")),
                ),
            )
            .on_ok(|_, _, cx| {
                AppSettings::set_disclaimer_accepted(cx);
                true
            })
    });
}
