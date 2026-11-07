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
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
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
        let pending = status.pending_project.is_some();
        let read_only = status
            .connection
            .as_ref()
            .is_some_and(|state| state.read_only);
        let live = status.realtime;
        let writes_enabled = status.writes_enabled;
        let danger = cx.theme().danger;

        let link = if pending {
            t!("app.editor.opening_project")
        } else {
            match &status.connection {
                Some(connection) => t!(
                    "app.status.connected",
                    signature = &connection.signature,
                    port = &connection.port,
                    framing = connection.framing,
                ),
                None => match status.link_lost.as_deref() {
                    Some(reason) => t!("app.status.link_lost", reason = reason),
                    None => t!("app.status.not_connected"),
                },
            }
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
                    .disabled(pending)
                    .on_click(|_, window, cx| {
                        write_dialogs::show_enable_writes(window, cx);
                    }),
            )
            .when_some(status.write_halted.as_deref(), |this, reason| {
                this.right(
                    Button::new("status-write-halted")
                        .label(t!("app.status.write_halted"))
                        .ghost()
                        .xsmall()
                        .disabled(pending)
                        .text_color(danger)
                        .tooltip(gpui_kit::SharedString::from(reason))
                        .on_click(|_, window, cx| {
                            write_dialogs::show_write(window, cx);
                        }),
                )
            })
            .right(concat!("v", env!("CARGO_PKG_VERSION")))
            .right(crate::notifications::bell::NotificationBell)
    }
}
