//! The content inside one gpui-kit toast.
//!
//! gpui-kit still owns the toast card, its stack, hover behaviour and motion.
//! This module only builds the body: severity icon, title, message, time and
//! the details action. Edit the layout here without touching the toast
//! lifecycle.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, FontWeight, IntoElement, ParentElement as _, Styled as _, div,
};

use super::{NotificationRecord, activate, severity_icon, time_label};
use crate::i18n::t;

/// The layout inside a gpui-kit toast.
pub(crate) fn content(record: &NotificationRecord, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let id = record.id();

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_start()
                .gap_2()
                .child(div().pt_0p5().child(severity_icon(record, cx)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .pr_6()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(record.title().to_string()),
                                )
                                .when(record.count() > 1, |this| {
                                    this.child(div().flex_none().text_xs().text_color(muted).child(
                                        t!(
                                            "app.notifications.repeat",
                                            count = record.count().to_string(),
                                        ),
                                    ))
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .line_clamp(3)
                                .child(record.body().to_string()),
                        ),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(time_label(record.created())),
                )
                .when(record.action().is_some(), |this| {
                    this.child(
                        Button::new(ElementId::Name(format!("toast-action-{id}").into()))
                            .label(t!("app.notifications.show_details"))
                            .primary()
                            .xsmall()
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                window.remove_notification1::<super::Notifications>(id, cx);
                                activate(id, window, cx);
                            }),
                    )
                }),
        )
        .into_any_element()
}
