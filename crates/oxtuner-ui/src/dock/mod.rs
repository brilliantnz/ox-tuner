// Vendored from gpui-component 0.9.0 (Apache-2.0). See NOTICE in this crate.

//! The dock's appearance: frames, panel chrome and the tab strip.
//!
//! The layout tree, the persisted schema, the drag geometry, the active-panel
//! state machine and the container entities all live in
//! [`gpui_kit::base::dock`]. This module is the skin over them: it re-exports the
//! types a consumer needs, adds the presentation half of the panel traits
//! (see [`panel`]), and implements base's two renderer traits.
//!
//! ```ignore
//! let area = cx.new(|cx| {
//!     DockArea::new("main", Some(1), window, cx).with_renderer(DockSkin::new(cx))
//! });
//! ```
//!
//! A [`DockArea`] built without [`DockSkin`] still docks, drags and persists —
//! it simply draws no chrome at all.

#![expect(
    missing_docs,
    missing_debug_implementations,
    clippy::all,
    clippy::pedantic,
    reason = "vendored gpui-component code stays close to upstream"
)]

mod dock;
mod invalid_panel;
mod panel;
mod tab_panel;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, SharedString, WeakEntity, Window, actions,
};

use crate::tabs::TabVariant;

/// The behavior half of the panel traits, which every panel implements
/// alongside [`Panel`]. Exported under this name because `Panel` in this
/// module is the presentation half that extends it.
pub use gpui_kit::base::dock::Panel as BasePanel;
/// The object-safe counterpart of [`BasePanel`], for the same reason.
pub use gpui_kit::base::dock::PanelView as BasePanelView;
/// Everything [`gpui_kit::base::dock`] exports, so a consumer never has to depend
/// on the foundation crate directly to write a skin or read a container's
/// state. Kept in step with base's own list by
/// `every_base_dock_export_is_reachable_from_here`.
///
/// Two names are handled elsewhere and one is deliberately absent:
/// base's `Panel` and `PanelView` arrive as [`BasePanel`] and [`BasePanelView`]
/// because this module's `Panel`/`PanelView` are the presentation halves that
/// extend them, and base's `Dock` — a plain state struct holding one dock's
/// open, collapsible, size and resizing flags — is not re-exported at all,
/// because the name meant a panel container in every released version of this
/// crate and handing it back with a different meaning is worse than dropping
/// it. A skin reads a dock through [`DockContext`].
pub use gpui_kit::base::dock::{
    AnyDrag, DockArea, DockAreaRenderer, DockAreaState, DockContext, DockEvent, DockLayout,
    DockPlacement, DockSizing, DockState, DragPanel, DropIndicator, DropPlaceholderBounds,
    DropTarget, EditResult, InsertTarget, NodeId, PaneNode, PaneRef, PaneTree, PanelBuildContext,
    PanelBuilder, PanelEvent, PanelId, PanelInfo, PanelRegistry, PanelSource, PanelState, RootKind,
    TabGroup, TabGroupConstraints, TabGroupContext, TabGroupEvent, TabGroupRenderer,
    register_panel,
};
pub use panel::*;
pub use tab_panel::DragPanelPreview;

// Named apart from gpui-component's dock actions, which share the process
// when the application also links that crate.
actions!(oxtuner_dock, [ToggleZoom, ClosePanel]);

/// A dock that the user opened or closed with one of [`DockSkin`]'s controls:
/// the collapse button in a tab bar, the strip of a collapsed bottom dock, or
/// a resize handle dragged past the closed strip and released.
///
/// A [`DockArea`] drawn by [`DockSkin`] emits it after the dock has changed.
/// [`DockArea::toggle_dock`] and the other dock setters do not, so an
/// application can tell a user's toggle from one its own code made.
pub struct DockToggleEvent {
    placement: DockPlacement,
    open: bool,
}

impl DockToggleEvent {
    /// The dock that was toggled.
    pub fn placement(&self) -> DockPlacement {
        self.placement
    }

    /// Whether the dock is open now.
    pub fn is_open(&self) -> bool {
        self.open
    }
}

impl EventEmitter<DockToggleEvent> for DockArea {}

/// Toggle a dock on the user's behalf and report it with [`DockToggleEvent`].
/// A dock that refuses to close emits nothing.
pub(crate) fn toggle_dock_by_user(
    area: &mut DockArea,
    placement: DockPlacement,
    window: &mut Window,
    cx: &mut Context<'_, DockArea>,
) {
    let was_open = area.is_dock_open(placement);
    area.toggle_dock(placement, window, cx);
    report_user_toggle(area, placement, was_open, cx);
}

/// Emit [`DockToggleEvent`] if a user action moved the dock away from
/// `was_open`.
pub(crate) fn report_user_toggle(
    area: &DockArea,
    placement: DockPlacement,
    was_open: bool,
    cx: &mut Context<'_, DockArea>,
) {
    let open = area.is_dock_open(placement);
    if open != was_open {
        cx.emit(DockToggleEvent { placement, open });
    }
}

/// Told which panel the reader picked, whenever a tab or a tab body is
/// clicked.
///
/// A group announces a changed tab, but a split has a displayed panel in every
/// group, so picking the tab or the body of the one already displayed changes
/// nothing a container would report. A host that follows the reader — a
/// navigation tree, a status line — needs the pick itself.
pub type TabPicked = Rc<dyn Fn(PanelId, &mut App)>;

/// Asked before a panel closes.
///
/// Return `true` to let the dock close it, `false` when the host took over —
/// a confirmation, for instance. The host is the only side that knows whether
/// the panel holds unsaved work.
pub type CloseRequest = Rc<dyn Fn(PanelId, &mut Window, &mut App) -> bool>;

/// What every part of the skin reads, and the dock area it belongs to.
///
/// The renderer is the only skin-owned object in the picture, so the settings
/// the old `DockArea` carried — the panel style, whether dock collapse
/// affordances are offered at all — live here. It is shared by reference with
/// the per-container renderers, which are built once each and outlive any one
/// frame.
pub(crate) struct SkinShared {
    area: WeakEntity<DockArea>,
    panel_style: Cell<PanelStyle>,
    /// The shape the tab strip draws. The tabs share it, so one setting covers
    /// every group in the area.
    tab_variant: Cell<TabVariant>,
    toggle_button_visible: Cell<bool>,
    close_button_visible: Cell<bool>,
    /// The dock whose resize handle is being dragged, if any. Only one can be.
    resizing_dock: Cell<Option<DockPlacement>>,
    /// Whether that dock was open when the drag began, so the release can
    /// tell whether the drag opened or closed it.
    resize_started_open: Cell<bool>,
    /// What to tell when the reader picks a tab or a tab body.
    tab_picked: RefCell<Option<TabPicked>>,
    /// Asked before a panel closes.
    close_request: RefCell<Option<CloseRequest>>,
}

impl SkinShared {
    pub(crate) fn area(&self) -> &WeakEntity<DockArea> {
        &self.area
    }

    pub(crate) fn panel_style(&self) -> PanelStyle {
        self.panel_style.get()
    }

    pub(crate) fn tab_variant(&self) -> TabVariant {
        self.tab_variant.get()
    }

    pub(crate) fn is_toggle_button_visible(&self) -> bool {
        self.toggle_button_visible.get()
    }

    pub(crate) fn resizing_dock(&self) -> &Cell<Option<DockPlacement>> {
        &self.resizing_dock
    }

    pub(crate) fn resize_started_open(&self) -> &Cell<bool> {
        &self.resize_started_open
    }

    /// Whether the host allows this panel to close. No host answer means yes.
    pub(crate) fn request_close(&self, panel: PanelId, window: &mut Window, cx: &mut App) -> bool {
        let Some(request) = self.close_request.borrow().clone() else {
            return true;
        };

        request(panel, window, cx)
    }

    /// Reports a pick, when a host asked for one.
    pub(crate) fn report_tab_picked(&self, panel: PanelId, cx: &mut App) {
        let Some(picked) = self.tab_picked.borrow().clone() else {
            return;
        };

        picked(panel, cx);
    }

    /// Redraw the area after a setting changed. The skin is not an entity, so
    /// nothing else would notice.
    fn notify(&self, cx: &mut App) {
        _ = self.area.update(cx, |_, cx| cx.notify());
    }
}

/// The appearance for a [`DockArea`], and the handle its
/// settings are changed through.
///
/// Install it at construction, where the area's own weak handle is available:
///
/// ```ignore
/// let skin = DockSkin::new(cx);
/// DockArea::new("main", None, window, cx).with_renderer(skin)
/// ```
///
/// Keep the returned handle to change a setting later; it is an `Rc`, so a
/// clone and the installed renderer are the same skin.
pub struct DockSkin {
    shared: Rc<SkinShared>,
}

impl DockSkin {
    /// Build a [`DockArea`] wearing this appearance, together with the handle
    /// its settings are changed through.
    ///
    /// The skin needs the area's own weak handle, so it can only be built
    /// while the area is being constructed; this is that dance done once.
    pub fn dock_area(
        id: impl Into<SharedString>,
        version: Option<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> (Entity<DockArea>, Rc<Self>) {
        let mut skin = None;
        let area = cx.new(|cx| {
            let this = Self::new(cx);
            skin = Some(Rc::clone(&this));
            DockArea::new(id, version, window, cx).with_renderer(this)
        });
        // The closure above runs before `cx.new` returns.
        (
            area,
            skin.expect("DockSkin::new ran inside the constructor"),
        )
    }

    pub fn new(cx: &mut Context<'_, DockArea>) -> Rc<Self> {
        Rc::new(Self {
            shared: Rc::new(SkinShared {
                area: cx.weak_entity(),
                panel_style: Cell::new(PanelStyle::default()),
                tab_variant: Cell::new(TabVariant::default()),
                toggle_button_visible: Cell::new(true),
                close_button_visible: Cell::new(false),
                resizing_dock: Cell::new(None),
                resize_started_open: Cell::new(false),
                tab_picked: RefCell::new(None),
                close_request: RefCell::new(None),
            }),
        })
    }

    pub(crate) fn shared(&self) -> &Rc<SkinShared> {
        &self.shared
    }

    /// Whether a single-panel tab group draws a plain title or a full tab bar.
    pub fn panel_style(&self) -> PanelStyle {
        self.shared.panel_style()
    }

    /// The shape every tab strip in the area draws.
    pub fn tab_variant(&self) -> TabVariant {
        self.shared.tab_variant()
    }

    /// Tells `picked` which panel the reader picks, whether by clicking its tab
    /// or by clicking inside it.
    pub fn set_tab_picked(&self, picked: TabPicked) {
        *self.shared.tab_picked.borrow_mut() = Some(picked);
    }

    /// Asks `request` before a panel closes.
    ///
    /// A host that answers `false` keeps the panel open and owns what happens
    /// next; the dock does not remember the request.
    pub fn set_close_request(&self, request: CloseRequest) {
        *self.shared.close_request.borrow_mut() = Some(request);
    }

    /// Sets the shape every tab strip in the area draws.
    pub fn set_tab_variant(&self, variant: TabVariant, cx: &mut App) {
        self.shared.tab_variant.set(variant);
        self.shared.notify(cx);
    }

    pub fn set_panel_style(&self, style: PanelStyle, cx: &mut App) {
        self.shared.panel_style.set(style);
        self.shared.notify(cx);
    }

    /// Whether tab bars offer the affordance that collapses a neighbouring
    /// dock.
    pub fn is_toggle_button_visible(&self) -> bool {
        self.shared.is_toggle_button_visible()
    }

    pub fn set_toggle_button_visible(&self, visible: bool, cx: &mut App) {
        self.shared.toggle_button_visible.set(visible);
        self.shared.notify(cx);
    }

    /// Show close buttons on closable tabs. Hidden by default; a panel's
    /// own close constraints still decide whether its button appears.
    pub fn set_close_button_visible(&self, visible: bool, cx: &mut App) {
        self.shared.close_button_visible.set(visible);
        self.shared.notify(cx);
    }
}
