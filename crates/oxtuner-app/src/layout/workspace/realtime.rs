//! The realtime charts, pinned beside the cluster.
//!
//! The panel owns one [`RealtimeView`] for the life of the window. The view
//! holds the chart's pause, legend and window selection, and the tab cannot be
//! closed, so nothing the reader configured there is lost to a tab switch.

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, Styled as _, Window,
};
use oxtuner_ui::dock::{BasePanel, Panel, PanelEvent};

use super::ActiveDocument;
use crate::i18n::t;
use crate::realtime::RealtimeView;

/// The live chart tab.
pub(crate) struct RealtimePanel {
    focus: FocusHandle,
    view: Entity<RealtimeView>,
}

impl RealtimePanel {
    /// A chart tab over the core's shared history.
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            view: cx.new(RealtimeView::new),
        }
    }
}

impl EventEmitter<PanelEvent> for RealtimePanel {}

impl EventEmitter<ActiveDocument> for RealtimePanel {}

impl Focusable for RealtimePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for RealtimePanel {
    fn panel_name(&self) -> &'static str {
        "RealtimePanel"
    }

    /// The charts are a workspace page, not a document: the tab stays where it
    /// is for the life of the window.
    fn closable(&self, _: &App) -> bool {
        false
    }

    /// The tab strip draws no zoom control, so a zoomed group would have no way
    /// back out.
    fn zoomable(&self, _: &App) -> bool {
        false
    }

    /// The charts have no document behind them, so the sidebar tree clears.
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<'_, Self>) {
        if active {
            cx.emit(ActiveDocument(None));
        }
    }
}

impl Panel for RealtimePanel {
    /// The tab's face: the chart icon, then its name. Without a short name the
    /// strip draws this, which is how the icon reaches the tab.
    fn title(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(
                Icon::new(IconName::Activity)
                    .small()
                    .text_color(cx.theme().info),
            )
            .child(t!("app.realtime.title"))
    }

    /// The tab names the view, and the chart draws its own toolbar and
    /// padding.
    fn title_bar(&self, _cx: &App) -> bool {
        false
    }

    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for RealtimePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.view.clone())
    }
}
