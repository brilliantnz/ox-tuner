//! The status-bar bell and its popover.
//!
//! The bell shows the unread count and opens the session inbox newest first.
//! Opening the popover changes nothing; only a click on one row marks that row
//! read and runs its action. Dismissing one row or clearing all affects the
//! inbox only, so Activity and the log file keep their own record.

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, ScrollHandle, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, div, px,
};

use super::{
    NotificationRecord, activate, clear, dismiss, records, set_inbox_open, severity_icon,
    time_label, unread,
};
use crate::i18n::t;

/// The notification bell in the status bar.
#[derive(IntoElement)]
pub(crate) struct NotificationBell;

impl RenderOnce for NotificationBell {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let unread = unread(cx);
        let icon = if unread == 0 {
            IconName::Bell
        } else {
            IconName::BellDot
        };
        let tooltip = if unread == 0 {
            t!("app.notifications.bell_tooltip")
        } else {
            t!("app.notifications.bell_unread", count = unread.to_string())
        };

        let trigger = Button::new("notification-bell")
            .icon(icon)
            .ghost()
            .xsmall()
            .tooltip(tooltip.clone())
            .accessibility_label(tooltip);

        Popover::new("notification-bell-popover")
            .anchor(Anchor::BottomRight)
            .offset(px(8.))
            .trigger(trigger)
            .p_0()
            .on_open_change(|open, window, cx| {
                set_inbox_open(*open, window, cx);
            })
            .content(|_, window, cx| {
                let popover = cx.entity().downgrade();
                let items = records(cx);
                let empty = items.is_empty();
                let scroll = window
                    .use_keyed_state("notification-list-scroll", cx, |_, _| ScrollHandle::new())
                    .read(cx)
                    .clone();

                v_flex()
                    .w(px(450.))
                    .child(header(cx, empty, &popover))
                    // .when(empty, |this| this.child(empty_state(cx)))
                    .when(!empty, |this| {
                        this.child(
                            div()
                                .relative()
                                .w_full()
                                .child(
                                    v_flex()
                                        .id("notification-list")
                                        .max_h(px(360.))
                                        .gap_1()
                                        .overflow_y_scroll()
                                        .track_scroll(&scroll)
                                        .children(
                                            items.iter().map(|record| row(record, &popover, cx)),
                                        ),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom_0()
                                        .w(px(4.))
                                        .child(list_scrollbar(cx, &scroll)),
                                ),
                        )
                    })
            })
    }
}

/// A thin rail at the popover's right edge.
///
/// `Scrollable` uses the theme's default rail width. The notification center
/// asks for a thinner one, so this popover owns the scroll handle and places
/// the bar itself.
fn list_scrollbar(cx: &App, scroll: &ScrollHandle) -> Scrollbar {
    Scrollbar::vertical(scroll)
        .id("notification-list-scrollbar")
        .mode(ScrollbarMode::Scrolling)
        .viewport_from_layout()
        .styles(|styles| {
            styles
                .track(|style| style.width(px(4.)).bg(cx.theme().transparent))
                .track_hover(|style| style.width(px(4.)).bg(cx.theme().transparent))
                .track_active(|style| style.width(px(4.)).bg(cx.theme().transparent))
                .thumb(|style| {
                    style
                        .width(px(3.))
                        .inset(px(1.))
                        .radius(px(1.5))
                        .bg(cx.theme().tokens.scrollbar_thumb)
                })
                .thumb_hover(|style| {
                    style
                        .width(px(3.))
                        .inset(px(1.))
                        .radius(px(1.5))
                        .bg(cx.theme().tokens.scrollbar_thumb_hover)
                })
                .thumb_active(|style| {
                    style
                        .width(px(3.))
                        .inset(px(1.))
                        .radius(px(1.5))
                        .bg(cx.theme().tokens.scrollbar_thumb_hover)
                })
        })
}

fn header(cx: &App, empty: bool, popover: &WeakEntity<PopoverState>) -> impl IntoElement {
    let close = popover.clone();

    h_flex()
        .w_full()
        .flex_none()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(cx.theme().border)
        .gap_2()
        .p_1()
        .child(
            div()
                .text_size(cx.theme().mono_font_size)
                .child(t!("app.notifications.title")),
        )
        .child(
            h_flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new("notification-clear")
                        .icon(IconName::ListX)
                        .ghost()
                        .xsmall()
                        .disabled(empty)
                        .tooltip(t!("app.notifications.clear"))
                        .accessibility_label(t!("app.notifications.clear"))
                        .on_click(|_, window, cx| clear(window, cx)),
                )
                .child(
                    Button::new("notification-close")
                        .icon(IconName::ChevronDown)
                        .ghost()
                        .xsmall()
                        .tooltip(t!("app.notifications.close"))
                        .accessibility_label(t!("app.notifications.close"))
                        .on_click(move |_, window, cx| {
                            close.update(cx, |state, cx| state.dismiss(window, cx)).ok();
                        }),
                ),
        )
}

// fn empty_state(cx: &App) -> impl IntoElement {
//     let muted = cx.theme().muted_foreground;

//     v_flex()
//         .w_full()
//         .items_center()
//         .justify_center()
//         .gap_2()
//         .py_8()
//         .text_center()
//         .child(
//             Icon::new(IconName::BellOff)
//                 .with_size(px(28.))
//                 .text_color(muted),
//         )
//         .child(
//             div()
//                 .text_sm()
//                 .font_weight(FontWeight::MEDIUM)
//                 .child(t!("app.notifications.empty")),
//         )
//         .child(
//             div()
//                 .text_xs()
//                 .text_color(muted)
//                 .child(t!("app.notifications.empty_hint")),
//         )
// }

fn row(
    record: &NotificationRecord,
    popover: &WeakEntity<PopoverState>,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let id = record.id();
    let details = popover.clone();

    v_flex()
        .id(ElementId::Name(format!("notification-row-{id}").into()))
        .w_full()
        .p_2()
        .gap_2()
        .rounded(theme.radius)
        .hover(|this| this.bg(theme.tokens.list_hover))
        // .on_click(move |_, window, cx| {
        //     close.update(cx, |state, cx| state.dismiss(window, cx)).ok();
        //     activate(id, window, cx);
        // })
        .child(
            h_flex()
                .w_full()
                .items_start()
                .gap_2()
                .child(div().pt_0p5().child(severity_icon(record, cx)))
                .child(row_body(record, cx))
                .child(dismiss_button(id)),
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
                        Button::new(ElementId::Name(format!("notification-details-{id}").into()))
                            .label(t!("app.notifications.show_details"))
                            .primary()
                            .xsmall()
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                details
                                    .update(cx, |state, cx| state.dismiss(window, cx))
                                    .ok();
                                activate(id, window, cx);
                            }),
                    )
                }),
        )
        .into_any_element()
}

fn row_body(record: &NotificationRecord, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;

    v_flex()
        .flex_1()
        .min_w_0()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
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
                    this.child(div().flex_none().text_xs().text_color(muted).child(t!(
                        "app.notifications.repeat",
                        count = record.count().to_string(),
                    )))
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.foreground)
                .line_clamp(3)
                .child(record.body().to_string()),
        )
        .into_any_element()
}

fn dismiss_button(id: u64) -> Button {
    Button::new(ElementId::Name(format!("notification-dismiss-{id}").into()))
        .icon(IconName::Close)
        .ghost()
        .xsmall()
        .tooltip(t!("app.notifications.dismiss"))
        .accessibility_label(t!("app.notifications.dismiss"))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            dismiss(id, window, cx);
        })
}
