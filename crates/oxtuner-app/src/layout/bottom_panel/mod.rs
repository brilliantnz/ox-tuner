//! The bottom panel: indicators, live telemetry and the session log.
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

mod indicators;
mod log;
mod telemetry;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use self::{indicators::Indicators, log::Log, telemetry::Telemetry};
use crate::i18n::t;

/// The categories the panel shows, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BottomTab {
    /// What the core currently holds and how the link is doing.
    Indicators,
    /// The newest realtime frame.
    Telemetry,
    /// Session log counters and recent activity.
    Log,
}

impl BottomTab {
    /// Every category, in the order the tab bar lays them out.
    const ALL: [Self; 3] = [Self::Indicators, Self::Telemetry, Self::Log];

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
            Self::Log => "log",
        }
    }

    /// The tab label in the interface language.
    fn label(self) -> SharedString {
        match self {
            Self::Indicators => t!("app.bottom_panel.tab_indicators"),
            Self::Telemetry => t!("app.bottom_panel.tab_telemetry"),
            Self::Log => t!("app.bottom_panel.tab_log"),
        }
    }

    /// The component that fills the content area for this category.
    fn content(self) -> AnyElement {
        match self {
            Self::Indicators => Indicators.into_any_element(),
            Self::Telemetry => Telemetry.into_any_element(),
            Self::Log => Log.into_any_element(),
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
}

impl BottomPanel {
    pub(crate) fn new() -> Self {
        Self {
            tab: BottomTab::Indicators,
        }
    }

    /// Switches the visible category.
    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let tab = BottomTab::from_index(index);
        if tab == self.tab {
            return;
        }

        self.tab = tab;
        cx.notify();
    }
}

impl Render for BottomPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab;

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // The folder bar is the panel's only chrome row: the tabs name the
            // categories, and closing stays one quiet icon at the trailing edge.
            .child(
                TabBar::new("bottom-panel-tabs")
                    .folder()
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
                                .tooltip(t!("app.bottom_panel.close_tooltip"))
                                .on_click(|_, _, cx| super::toggle_bottom_panel(cx)),
                        ),
                    ),
            )
            // The folder shape joins the content painted with the active-tab
            // color, so the selected tab reads as part of the panel below it.
            // Only the visible category is built, so switching to another tab
            // starts that category's content at the top.
            .child(
                div()
                    .id(ElementId::Name(
                        format!("bottom-panel-scroll-{}", tab.id()).into(),
                    ))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .bg(cx.theme().tokens.tab_active)
                    .p_3()
                    .child(tab.content()),
            )
    }
}
