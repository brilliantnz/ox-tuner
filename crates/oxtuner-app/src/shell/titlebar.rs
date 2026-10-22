//! The title bar, drawn by the app instead of the OS.
//!
//! The window opens with `appears_transparent: true` and
//! `app_owns_titlebar_drag: true`, so the OS draws no caption and hands window
//! dragging back to us. gpui-kit's `TitleBar` component is not used: it draws
//! its own gradient strip and a fixed 80 pt lead, and this layout has no
//! full-width band to put it in — every column runs to the top edge of the
//! window, so the bar is one region per column at the same height:
//!
//! - [`DragTop`] — an empty drag region for the navigation column.
//! - [`MainBar`] — the editor column's first 35 px: menus where the OS has no
//!   global menu bar, the title, panel buttons, and the window controls off
//!   macOS (this column is the window's right edge).
//! - [`UtilityBar`] — the same two-region shape for a utility window (Settings):
//!   a drag strip over the settings sidebar plus a titled bar with controls.
//!
//! All are built from [`drag_surface`], so a drag or double click behaves the
//! same wherever the user grabs the window. All vertical values derive from
//! [`HEIGHT`]; a hand-written offset drifts away from the traffic lights the
//! moment that height changes.

use gpui_kit::base::{InteractiveElementExt as _, h_flex};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, menu::AppMenuBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, Entity, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, MouseMoveEvent, ParentElement as _, Pixels, Point, RenderOnce, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, TitlebarOptions, Window,
    WindowControlArea, div, point, px,
};

use crate::constants::APP_NAME;

/// Height of the title bar, shared by both regions so they line up across the
/// sidebar's border.
pub(crate) const HEIGHT_PX: f32 = 35.;

pub(crate) const HEIGHT: Pixels = px(HEIGHT_PX);

/// The line the bar's contents and the traffic lights share.
const CONTENT_CENTRE_PX: f32 = HEIGHT_PX / 2.;

/// Height of AppKit's standard window-button frame. The lights are drawn inside
/// it; it does not follow our bar's height.
const BUTTON_H_PX: f32 = 14.;

/// Inset before the first light. Comfortably clear of the window's left edge.
const LIGHT_INSET_PX: f32 = 11.;

/// Where the bar's own content starts on macOS: after the traffic lights.
///
/// The single bar spans the whole window, so its leading edge sits under the
/// lights. Three buttons at 14 px, the inset before them and a gap after: the
/// same cluster geometry AppKit lays out, so menus and the title never hide
/// behind a light.
const MACOS_LEAD_PX: f32 = LIGHT_INSET_PX + 54. + 12.;

/// Where AppKit should hang the traffic lights.
///
/// AppKit positions them by the *top* of their frame, so this subtracts half a
/// button from the content centre. The window options and the sidebar region must
/// agree on the number, which is why it comes from one function.
pub(crate) fn traffic_light_position() -> Point<Pixels> {
    point(px(LIGHT_INSET_PX), px(CONTENT_CENTRE_PX - BUTTON_H_PX / 2.))
}

/// The platform title-bar options every app window shares.
///
/// Centralising them keeps the traffic lights on the same line as our own
/// content rows in every window, including the Settings window.
pub(crate) fn titlebar_options(title: impl Into<SharedString>) -> TitlebarOptions {
    TitlebarOptions {
        title: Some(title.into()),
        appears_transparent: true,
        traffic_light_position: Some(traffic_light_position()),
    }
}

fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

/// State for one drag region.
#[derive(Default)]
struct DragState {
    armed: bool,
}

/// A band that moves the window on drag and zooms it on double click.
///
/// `id` keys the state: every region needs its own, or the two would share one
/// armed flag and a press in the sidebar could start from the main bar.
///
/// The armed flag is the whole protocol, so two details matter:
///
/// - a move only starts while the left button is **still held**
///   (`MouseMoveEvent::pressed_button`). `start_window_move` hands the drag to
///   AppKit through `performWindowDragWithEvent:`, which uses the *current*
///   event; calling it after release makes AppKit log "Window move completed
///   without beginning" and can leave the window in a half-dragged state;
/// - anything interactive inside the band must stop its own mouse-down, or the
///   press arms the band and the next move starts a window drag from a button.
///   [`drag_child`] is that guard.
fn drag_surface(id: &'static str, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let state = window.use_keyed_state(id, cx, |_, _| DragState::default());

    div()
        .id(id)
        .flex()
        .items_center()
        .h(HEIGHT)
        .flex_shrink_0()
        // Windows: let the OS own the drag, so snap layouts keep working.
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(
            MouseButton::Left,
            window.listener_for(&state, |state, _, _, _| state.armed = true),
        )
        // Any release ends the press, whether the pointer is still over the band
        // or not.
        .on_mouse_up(
            MouseButton::Left,
            window.listener_for(&state, |state, _, _, _| state.armed = false),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            window.listener_for(&state, |state, _, _, _| state.armed = false),
        )
        .on_mouse_move(
            window.listener_for(&state, |state, event: &MouseMoveEvent, window, _| {
                if !state.armed {
                    return;
                }
                if event.pressed_button != Some(MouseButton::Left) {
                    // The button came up somewhere the release did not reach us.
                    state.armed = false;
                    return;
                }
                // One move per press: AppKit takes the drag over from here.
                state.armed = false;
                window.start_window_move();
            }),
        )
        .on_double_click(window.listener_for(&state, |_, _: &ClickEvent, window, _| {
            if is_macos() {
                // Follows the user's "double-click a title bar" setting.
                window.titlebar_double_click();
            } else {
                window.zoom_window();
            }
        }))
}

/// Keeps a band's drag protocol away from interactive content.
///
/// Wraps menus, toggle buttons and window controls so a press on them never arms
/// the drag surface they sit on. Without it the press arms the band, the next
/// pointer move starts a window drag from a button, and AppKit logs
/// "Window move completed without beginning".
///
/// Mouse **moves** are stopped as well: the band listens for moves to begin a
/// drag, and a move over a button should not begin one.
pub(crate) fn drag_child(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h_full()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
}

/// A drag region for one column of a **split** title bar.
///
/// Two layouts exist, and both stay in the code:
///
/// - **One bar across the window** (the main window). The bar is a single
///   [`MainBar`] above every column, so no column reaches the top edge and no
///   `DragTop` is mounted.
/// - **One strip per column** (the Settings window). The sidebar column reaches
///   the top edge of the window — on macOS the traffic lights float in it — so
///   it needs its own draggable strip beside the titled one. [`UtilityBar`]
///   builds exactly that; mounting `DragTop` directly is the same thing for a
///   column that is not part of a bar.
///
/// The `id` must be unique within the window: keyed state means two regions
/// sharing an id would share one armed flag.
#[allow(
    dead_code,
    reason = "kept for windows that let a column reach the top edge; the current \
              layouts draw one bar across the window, and this is the helper those \
              layouts return to"
)]
#[derive(IntoElement)]
pub(crate) struct DragTop {
    id: &'static str,
    width: Option<Pixels>,
}

#[allow(dead_code, reason = "see `DragTop`")]
impl DragTop {
    pub(crate) const fn new(id: &'static str) -> Self {
        Self { id, width: None }
    }

    /// Fix the strip to one column's width.
    pub(crate) fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }
}

impl RenderOnce for DragTop {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        drag_surface(self.id, window, cx)
            .map(|this| match self.width {
                Some(width) => this.w(width).flex_shrink_0(),
                None => this.w_full(),
            })
            .bg(cx.theme().tokens.sidebar)
    }
}

/// The two-region bar of a utility window (Settings).
///
/// Without the application menu and without a menu bar: a utility window has one
/// task. The leading region is a drag strip exactly as wide as the settings
/// sidebar, in the sidebar's colour, so the sidebar reads as one column that
/// reaches the top edge of the window — and on macOS the traffic lights float
/// there, as they do in the main window.
#[allow(
    dead_code,
    reason = "the split-bar layout the Settings window is designed around; its call \
              site is currently commented out while the component's own bar is used"
)]
#[derive(IntoElement)]
pub(crate) struct UtilityBar {
    title: SharedString,
    sidebar_width: Pixels,
}

#[allow(dead_code, reason = "see `UtilityBar`")]
impl UtilityBar {
    pub(crate) fn new(title: impl Into<SharedString>, sidebar_width: Pixels) -> Self {
        Self {
            title: title.into(),
            sidebar_width,
        }
    }
}

impl RenderOnce for UtilityBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let controls_drawn = !is_macos() && !window.is_fullscreen();

        h_flex()
            .h(HEIGHT)
            .w_full()
            .flex_shrink_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                // The sidebar column of a split bar: a strip of exactly the
                // sidebar's width, so the two read as one surface.
                DragTop::new("utility-sidebar-titlebar").width(self.sidebar_width),
            )
            .child(
                drag_surface("utility-titlebar", window, cx)
                    .flex_1()
                    .pl_3()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title),
                    ),
            )
            .when(controls_drawn, |this| {
                this.child(window_controls(window, cx))
            })
    }
}

/// The window's title bar.
///
/// One bar across the whole window: menus at the leading edge, the panel toggles
/// at the trailing edge, and the window controls outside it off macOS.
///
/// The title is centred on the **window**, not on whatever space the menus and
/// buttons leave over. Leftover space is not equal — macOS reserves a lead for
/// the traffic lights, and Windows/Linux reserve room for the window controls —
/// so a flex row of `[menus][title][buttons]` puts the title off-centre by half
/// of each difference. The title is therefore positioned across the full bar, and
/// the menus and toggles sit on top of it at the ends.
#[derive(IntoElement)]
pub(crate) struct MainBar {
    menu_bar: Entity<AppMenuBar>,
    actions: Vec<AnyElement>,
}

impl MainBar {
    pub(crate) fn new(menu_bar: Entity<AppMenuBar>) -> Self {
        Self {
            menu_bar,
            actions: Vec::new(),
        }
    }

    /// Panel toggles, rendered at the trailing edge of the bar.
    ///
    /// The bar is one drag surface, so a button inside it must stop its click
    /// from bubbling: otherwise a second click on a toggle arrives at the
    /// surface as a double click and zooms the window instead of toggling the
    /// panel again.
    pub(crate) fn actions(mut self, actions: Vec<AnyElement>) -> Self {
        self.actions = actions;
        self
    }
}

impl RenderOnce for MainBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let controls_drawn = !is_macos() && !window.is_fullscreen();
        // Room the window controls occupy at the trailing edge. The toggles stop
        // before it, and the title does not care: it is centred across the bar.
        let controls_width = if controls_drawn {
            px(HEIGHT_PX * 3.)
        } else {
            px(0.)
        };

        h_flex()
            .relative()
            .h(HEIGHT)
            .w_full()
            .flex_shrink_0()
            .bg(cx.theme().background)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                drag_surface("main-titlebar", window, cx)
                    .flex_1()
                    .relative()
                    // Drawn first, under the menus and toggles: this covers the
                    // whole bar, so clicks on empty space reach the drag
                    // surface, while the controls above it get their own.
                    .child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .left(px(0.))
                            .right(px(0.))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(APP_NAME),
                            ),
                    )
                    .child(
                        drag_child("main-titlebar-menu")
                            .h_full()
                            .min_w_0()
                            // Clear of the traffic lights, which float over the
                            // bar's leading edge on macOS.
                            .pl(px(if is_macos() { MACOS_LEAD_PX } else { 12. }))
                            .when(!is_macos(), |this| this.child(self.menu_bar.clone())),
                    )
                    .child(
                        drag_child("main-titlebar-actions")
                            .absolute()
                            .top(px(0.))
                            .right(controls_width + px(8.))
                            .gap_1()
                            .children(self.actions),
                    ),
            )
            .when(controls_drawn, |this| {
                this.child(
                    window_controls(window, cx)
                        .absolute()
                        .top(px(0.))
                        .right(px(0.))
                        .h_full(),
                )
            })
    }
}

/// Minimize, maximize/restore and close, drawn by us because a transparent
/// title bar removes the system caption.
fn window_controls(window: &Window, cx: &App) -> Div {
    let maximized = window.is_maximized();

    h_flex()
        .h_full()
        .flex_shrink_0()
        .child(
            control("window-minimize", IconName::WindowMinimize, cx)
                .on_click(|_, window, _| window.minimize_window()),
        )
        .child(if maximized {
            control("window-restore", IconName::WindowRestore, cx)
                .on_click(|_, window, _| window.zoom_window())
        } else {
            control("window-maximize", IconName::WindowMaximize, cx)
                .on_click(|_, window, _| window.zoom_window())
        })
        .child(
            control("window-close", IconName::WindowClose, cx)
                .hover(|this| {
                    this.bg(cx.theme().danger)
                        .text_color(cx.theme().danger_foreground)
                })
                .on_click(|_, window, _| window.remove_window()),
        )
}

fn control(id: &'static str, icon: IconName, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .justify_center()
        .items_center()
        .w(HEIGHT)
        .h_full()
        .flex_shrink_0()
        .text_color(cx.theme().foreground)
        .hover(|this| {
            this.bg(cx.theme().secondary_hover)
                .text_color(cx.theme().secondary_foreground)
        })
        .active(|this| this.bg(cx.theme().secondary_active))
        .child(Icon::new(icon).small())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AppKit hangs the lights from the top of their frame, so their centre is
    /// half a button below the number we hand it. That centre is the line the
    /// bar's own rows are centred on; a hand-tuned offset drifts away from it the
    /// moment the height changes.
    #[test]
    fn traffic_lights_sit_on_the_content_line() {
        let top = traffic_light_position().y.as_f32();
        assert_eq!(top + BUTTON_H_PX / 2., CONTENT_CENTRE_PX);
    }

    /// The lights are drawn over the sidebar region, so they have to fit inside
    /// it; otherwise a drag started on a light lands outside the drag surface.
    #[test]
    fn sidebar_region_contains_the_traffic_lights() {
        let bottom = traffic_light_position().y.as_f32() + BUTTON_H_PX;
        assert!(bottom <= HEIGHT_PX);
    }
}
