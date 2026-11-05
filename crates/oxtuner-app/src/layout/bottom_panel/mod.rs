//! The bottom panel: indicators, live telemetry, activity and diagnostics.
//!
//! It sits inside the editor column (not across the secondary sidebar), so
//! closing it gives the height back to the editor only. Its height is a
//! resizable-panel size and its open state is session state, so both survive a
//! restart.
//!
//! This module owns the panel frame: the category tabs, the close affordance
//! and the scrolling content area. Each category is a component in a sibling
//! module, so the status vocabulary and the telemetry layout can change without
//! touching the tab wiring.

mod activity;
mod diagnostics;
mod indicators;
mod telemetry;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div,
};
use oxtuner_ui::tabs::{Tab, TabBar};

pub(crate) use self::diagnostics::DiagnosticsView;
use self::{activity::Activity, indicators::Indicators, telemetry::Telemetry};
use crate::i18n::t;

/// The categories the panel shows, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BottomTab {
    /// What the core currently holds and how the link is doing.
    Indicators,
    /// The newest realtime frame.
    Telemetry,
    /// Session log counters, recent outcomes and the journal.
    Activity,
    /// The bounded live tracing tail.
    Diagnostics,
}

impl BottomTab {
    /// Every category, in the order the tab bar lays them out.
    const ALL: [Self; 4] = [
        Self::Indicators,
        Self::Telemetry,
        Self::Activity,
        Self::Diagnostics,
    ];

    /// The tab bar reports a position; this maps it back to the category.
    fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Indicators)
    }

    /// The position of the category in the tab bar.
    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or_default()
    }

    /// A stable name for the category, used as scroll state identity.
    ///
    /// The name must not follow the tab order: reordering the tabs must not
    /// hand one category's scroll state to another.
    fn id(self) -> &'static str {
        match self {
            Self::Indicators => "indicators",
            Self::Telemetry => "telemetry",
            Self::Activity => "activity",
            Self::Diagnostics => "diagnostics",
        }
    }

    /// The tab label in the interface language.
    fn label(self) -> SharedString {
        match self {
            Self::Indicators => t!("app.bottom_panel.tab_indicators"),
            Self::Telemetry => t!("app.bottom_panel.tab_telemetry"),
            Self::Activity => t!("app.bottom_panel.tab_activity"),
            Self::Diagnostics => t!("app.bottom_panel.tab_diagnostics"),
        }
    }

    /// The component that fills the content area for this category.
    fn content(self, diagnostics: Entity<DiagnosticsView>) -> AnyElement {
        match self {
            Self::Indicators => Indicators.into_any_element(),
            Self::Telemetry => Telemetry.into_any_element(),
            Self::Activity => Activity.into_any_element(),
            Self::Diagnostics => diagnostics.into_any_element(),
        }
    }
}

/// The output panel.
///
/// The selected category is view state, not session state: it lives as long as
/// the window does, so hiding and reopening the panel returns to the same tab
/// without writing a preference for it.
pub(crate) struct BottomPanel {
    tab: BottomTab,
    diagnostics: Entity<DiagnosticsView>,
}

impl BottomPanel {
    pub(crate) fn new(diagnostics: Entity<DiagnosticsView>) -> Self {
        Self {
            tab: BottomTab::Indicators,
            diagnostics,
        }
    }

    /// Switches the visible category.
    fn select(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let tab = BottomTab::from_index(index);
        if tab == self.tab {
            return;
        }

        self.tab = tab;
        self.diagnostics.update(cx, |diagnostics, cx| {
            diagnostics.set_active(tab == BottomTab::Diagnostics, cx);
        });
        cx.notify();
    }
}

impl Render for BottomPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let tab = self.tab;
        let diagnostics = self.diagnostics.clone();

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // The folder bar is the panel's only chrome row: the tabs name the
            // categories, and closing stays one quiet icon at the trailing edge.
            .child(
                TabBar::new("bottom-panel-tabs")
                    .folder()
                    .bg(cx.theme().tab_bar)
                    .small()
                    .selected_index(tab.index())
                    .on_click(cx.listener(|this, index, _, cx| this.select(*index, cx)))
                    .children(BottomTab::ALL.map(|tab| Tab::new().label(tab.label())))
                    .suffix(
                        h_flex().items_center().pr_1().child(
                            Button::new("close-bottom-panel")
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip(t!("app.bottom_panel.hide_tooltip"))
                                .on_click(|_, _, cx| super::toggle_bottom_panel(cx)),
                        ),
                    ),
            )
            // The folder shape joins the content painted with the active-tab
            // color, so the selected tab reads as part of the panel below it.
            // Only the visible category is built, so switching to another tab
            // starts that category's content at the top. Diagnostics owns its
            // own virtual-list scroll, so the outer scroll is skipped there.
            .child(
                div()
                    .id(ElementId::Name(
                        format!("bottom-panel-scroll-{}", tab.id()).into(),
                    ))
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(
                        tab == BottomTab::Diagnostics,
                        gpui_kit::Styled::overflow_hidden,
                    )
                    .when(tab != BottomTab::Diagnostics, |this| {
                        this.overflow_y_scroll()
                    })
                    .bg(cx.theme().tokens.tab_active)
                    .p_3()
                    .child(tab.content(diagnostics)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Bounds, Context, IntoElement, Point, Render, TestAppContext, Window,
        WindowBounds, WindowOptions, px, size,
    };

    struct Host {
        panel: Entity<BottomPanel>,
        show: bool,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
            div()
                .size_full()
                .children(self.show.then(|| self.panel.clone()))
        }
    }

    fn open(
        cx: &mut TestAppContext,
    ) -> (gpui_kit::AnyWindowHandle, Entity<Host>, Entity<BottomPanel>) {
        cx.update(gpui_kit::init);

        cx.update(|cx| {
            let (handle, host) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(900.), px(600.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let diagnostics = cx.new(|cx| DiagnosticsView::new(window, cx));
                    let panel = cx.new(|_| BottomPanel::new(diagnostics));
                    cx.new(|_| Host { panel, show: false })
                },
            )
            .expect("open test window");

            let panel = host.read(cx).panel.clone();
            (handle, host, panel)
        })
    }

    #[gpui_kit::test]
    fn selecting_diagnostics_switches_and_renders(cx: &mut TestAppContext) {
        let (handle, host, panel) = open(cx);
        assert_eq!(BottomTab::ALL.len(), 4);

        cx.update(|cx| {
            panel.update(cx, |panel, cx| panel.select(3, cx));
            host.update(cx, |host, cx| {
                host.show = true;
                cx.notify();
            });
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .expect("render the diagnostics tab");

        assert_eq!(cx.update(|cx| panel.read(cx).tab), BottomTab::Diagnostics);
    }
}
