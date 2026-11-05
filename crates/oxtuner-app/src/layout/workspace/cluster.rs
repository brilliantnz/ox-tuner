//! The cluster: the page a tuner watches while the engine runs.
//!
//! It is the workspace's first tab and cannot be closed. Everything else in the
//! dock is a document that opens and closes; the cluster is the reading that
//! stays where it is, so a glance at engine state never needs a tab hunt.
//!
//! The page is empty on purpose. It is the home of the gauge cluster, the
//! dials and bars the reader will arrange here, and that layout does not exist
//! yet. The live values and the definition's indicators belong to the bottom
//! panel, which already shows them next to every tab, so nothing is drawn here
//! until there are gauges to draw.

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::{
    App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement as _, Render,
    Styled as _, Window,
};
use oxtuner_ui::dock::{BasePanel, Panel, PanelEvent};

use super::ActiveDocument;
use crate::i18n::t;

/// The pinned first tab of the workspace.
pub(crate) struct ClusterPanel {
    focus: FocusHandle,
}

impl ClusterPanel {
    /// An empty cluster page.
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }
}

impl EventEmitter<PanelEvent> for ClusterPanel {}

impl EventEmitter<ActiveDocument> for ClusterPanel {}

impl Focusable for ClusterPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for ClusterPanel {
    fn panel_name(&self) -> &'static str {
        "ClusterPanel"
    }

    /// The cluster is not a document. Closing it would leave the workspace with
    /// nothing to put back in its place, so the dock never offers the command.
    fn closable(&self, _: &App) -> bool {
        false
    }

    /// The tab strip draws no zoom control, so a zoomed group would have no way
    /// back out.
    fn zoomable(&self, _: &App) -> bool {
        false
    }

    /// The cluster has no document behind it, so the sidebar tree clears.
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<'_, Self>) {
        if active {
            cx.emit(ActiveDocument(None));
        }
    }
}

impl Panel for ClusterPanel {
    /// The tab's face: the page's own icon, then its name. Without a short
    /// name the strip draws this, which is how the icon reaches the tab.
    fn title(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(
                Icon::new(IconName::Gauge)
                    .small()
                    .text_color(cx.theme().primary),
            )
            .child(t!("app.cluster.title"))
    }

    /// The tab already names the panel, and the page owns its padding.
    fn title_bar(&self, _cx: &App) -> bool {
        false
    }

    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for ClusterPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
    }
}
