// Vendored from gpui-component 0.9.0; migrated to 0.11.0 (Apache-2.0).
// See NOTICE in this crate.

//! The dock area's appearance: the outer frame, the split frames, and one
//! dock's chrome.

use std::{rc::Rc, sync::Arc};

use gpui_kit::base::{
    HandleEdge, ResizeHandleContext,
    dock::{
        DockAreaRenderer, DockContext, DockEvent, DockPlacement, NodeId, PanelState, PanelView,
        TabGroupRenderer,
    },
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::resizable::{resize_handle, resize_handle_appearance};
use gpui_kit::{
    AnyElement, App, AppContext as _, Axis, Context, Div, Element, Empty, InteractiveElement as _,
    IntoElement, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, Stateful, Style,
    Styled as _, Window, div,
};

use crate::dock::{
    DockSkin, SkinShared, invalid_panel::InvalidPanel, panel_handle, report_user_toggle,
    tab_panel::TabGroupSkin,
};

/// The payload a dock's resize handle drags. It draws nothing: the handle
/// itself is the affordance.
#[derive(Clone)]
struct ResizePanel;

impl Render for ResizePanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        Empty
    }
}

impl DockAreaRenderer for DockSkin {
    // The row, the fill and the clip are base's now -- applied around whatever
    // these return -- so a skin that has no appearance to add returns a bare
    // frame and still gets a dock area the right shape.
    fn frame(&self, _: &mut Window, _: &mut App) -> Stateful<Div> {
        div().id("dock-area")
    }

    fn center_frame(&self, _: &mut Window, _: &mut App) -> Stateful<Div> {
        div().id("dock-area-center")
    }

    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        resize_handle_appearance()(handle, window, cx)
    }

    fn split_frame(&self, node: NodeId, _: Axis, _: &mut Window, cx: &mut App) -> Stateful<Div> {
        // The size is base's; the background is this skin's, and is the only
        // reason this hook is implemented at all.
        div()
            .id(("dock-split-frame", node.as_u64()))
            .bg(cx.theme().tokens.tab_bar)
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        // No box here any more. A dock's extent is structural, so
        // `DockArea::render_dock` applies it around whatever this returns --
        // which also means a renderer that draws no chrome still gets a dock
        // the right shape. This adds the edge you drag and nothing else.
        div()
            .flex()
            .size_full()
            .relative()
            .child(content)
            .child(self.render_resize_handle(dock, window, cx))
            .child(DockResizeTracker {
                dock: dock.clone(),
                shared: Rc::clone(self.shared()),
            })
            .into_any_element()
    }

    /// The "unknown panel" message the old `InvalidPanel` drew.
    ///
    /// It answers `dump` with the state it was handed, so a layout written by
    /// a build that knows the panel survives a load and save here.
    fn build_placeholder(
        &self,
        state: &PanelState,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn PanelView>> {
        let state = state.clone();
        Some(panel_handle(cx.new(|cx| InvalidPanel::new(state, cx))))
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(TabGroupSkin::new(Rc::clone(self.shared())))
    }
}

impl DockSkin {
    fn render_resize_handle(
        &self,
        dock: &DockContext,
        _: &mut Window,
        _: &mut App,
    ) -> impl IntoElement {
        let placement = dock.placement();
        let open = dock.is_open();
        let shared = Rc::clone(self.shared());

        // One id per placement: the docks all render under the same stateful
        // ancestor, so a shared literal would collapse the handles into one
        // GlobalElementId and GPUI would silently share their element state —
        // a press on the left handle then starts the right handle's drag.
        let id = match placement {
            DockPlacement::Left => "resize-handle-left",
            DockPlacement::Right => "resize-handle-right",
            DockPlacement::Bottom => "resize-handle-bottom",
            DockPlacement::Center => "resize-handle-center",
        };

        // Every dock's handle lives inside the dock, because `dock_frame` clips
        // to the dock's box. The left dock hugs its trailing edge; the right
        // and bottom docks hug their leading one.
        let edge = match placement {
            DockPlacement::Left => HandleEdge::Trailing,
            _ => HandleEdge::Leading,
        };

        resize_handle(id, placement.axis()).inside(edge).on_drag(
            ResizePanel,
            move |info, _, _, cx| {
                cx.stop_propagation();
                shared.resizing_dock().set(Some(placement));
                shared.resize_started_open().set(open);
                cx.new(|_| info.clone())
            },
        )
    }
}

/// Turns the window's mouse stream into dock resizing.
///
/// A resize is driven by pointer moves that land anywhere in the window, not
/// only on the handle, so it cannot be expressed as a listener on the handle
/// itself. This element paints nothing and exists for its `paint` hook, which
/// is the only place a window-level mouse listener can be registered — which
/// is why it stays in the skin rather than moving into base: it is a
/// paint-order concern of this appearance.
struct DockResizeTracker {
    dock: DockContext,
    shared: Rc<SkinShared>,
}

impl Element for DockResizeTracker {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui_kit::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui_kit::GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui_kit::LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui_kit::GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        _: gpui_kit::Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _: Option<&gpui_kit::GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        _: gpui_kit::Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let placement = self.dock.placement();

        window.on_mouse_event({
            let dock = self.dock.clone();
            let shared = Rc::clone(&self.shared);
            move |event: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() || shared.resizing_dock().get() != Some(placement) {
                    return;
                }
                // Base opens, closes and sizes the dock from the pointer, so a
                // drag that started on a closed dock reopens it too.
                dock.resize_to(event.position, window, cx);
            }
        });

        window.on_mouse_event({
            let dock = self.dock.clone();
            let shared = Rc::clone(&self.shared);
            move |_: &MouseUpEvent, phase, window, cx| {
                if !phase.bubble() || shared.resizing_dock().get() != Some(placement) {
                    return;
                }
                shared.resizing_dock().set(None);
                dock.end_resize(window, cx);
                let was_open = shared.resize_started_open().get();
                // The size lives on the dock, not in the layout tree, so
                // nothing else tells a subscriber to persist it.
                _ = shared.area().update(cx, |area, cx| {
                    cx.emit(DockEvent::LayoutChanged);
                    report_user_toggle(area, placement, was_open, cx);
                });
            }
        });
    }
}
