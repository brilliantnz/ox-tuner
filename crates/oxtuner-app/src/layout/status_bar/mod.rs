//! The status bar along the window's bottom edge.
//!
//! It reports the link and the write state and offers the commands that change
//! them: enabling writes and clearing a halted write. Every value comes from
//! the core's status, so the bar cannot disagree with the session it reports.
//!
//! The bar is one gpui-kit [`StatusBar`]: the connection label on the leading
//! edge, the session state and the actions on the trailing edge.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div};

use super::write_dialogs;
use crate::core::CoreService;
use crate::i18n::t;

/// The window's status bar.
#[derive(IntoElement)]
pub(crate) struct StatusBarView;

impl RenderOnce for StatusBarView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let status = CoreService::status(cx);
        let connection = status.connection.clone();
        let lost = status.link_lost.clone();
        let read_only = connection.as_ref().is_some_and(|state| state.read_only);
        let live = status.realtime;
        let writes_enabled = status.writes_enabled;
        let write_halted = status.write_halted.clone();
        let danger = cx.theme().danger;

        let link = match connection {
            Some(connection) => t!(
                "app.status.connected",
                signature = connection.signature.clone(),
                port = connection.port.clone(),
                framing = connection.framing.to_owned(),
            ),
            None => match lost {
                Some(reason) => t!("app.status.link_lost", reason = reason),
                None => t!("app.status.not_connected"),
            },
        };

        StatusBar::new()
            .flex_shrink_0()
            .bg(cx.theme().background)
            .left(link)
            .when(read_only, |this| {
                this.right(div().text_color(danger).child(t!("app.status.read_only")))
            })
            .when(live, |this| this.right(t!("app.status.live")))
            .right(
                Button::new("status-writes")
                    .label(if writes_enabled {
                        t!("app.status.writes_on")
                    } else {
                        t!("app.status.writes_off")
                    })
                    .ghost()
                    .xsmall()
                    .on_click(|_, window, cx| {
                        write_dialogs::show_enable_writes(window, cx);
                    }),
            )
            .when_some(write_halted, |this, reason| {
                this.right(
                    Button::new("status-write-halted")
                        .label(t!("app.status.write_halted"))
                        .ghost()
                        .xsmall()
                        .text_color(danger)
                        .tooltip(gpui_kit::SharedString::from(reason))
                        .on_click(|_, window, cx| {
                            write_dialogs::show_write(window, cx);
                        }),
                )
            })
            .right(concat!("v", env!("CARGO_PKG_VERSION")))
    }
}
