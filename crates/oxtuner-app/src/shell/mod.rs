//! The main window's shell view.
//!
//! Three resizable panels under one title bar, named by role:
//!
//! | Panel | Where | Chrome |
//! | --- | --- | --- |
//! | **Primary sidebar** | left | section labels only; no header row |
//! | **Secondary sidebar** | right | header row with its close button |
//! | **Bottom panel** | under the editor only | header row with its close button |
//!
//! The bar spans the whole window: menus, title, the three panel toggles and (off
//! macOS) the window controls. Because of that no column reaches the window's top
//! edge, and no column needs its own drag strip. The split-bar arrangement — a
//! drag strip per column — is still in `titlebar.rs` ([`titlebar::DragTop`]) and
//! is what the Settings window uses.
//!
//! Sizes, widths and open state live in `state.json`; the view reads them when it
//! renders and writes them back on a drag or a toggle.
//!
//! Views live in this crate next to the state they render; they move to
//! `oxtuner-ui` only when a widget is reused or the split earns its keep (crate
//! boundary: ADR-0001).

use std::{cell::Cell, rc::Rc};

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::resizable::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, Theme, ThemeMode, menu::AppMenuBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Render, Styled as _, Subscription, Window, div, px, radians,
};

pub mod bottom_panel;
pub mod primary_sidebar;
pub mod secondary_sidebar;
pub mod titlebar;

use self::{
    bottom_panel::BottomPanel, primary_sidebar::PrimarySidebar,
    secondary_sidebar::SecondarySidebar, titlebar::MainBar,
};
use crate::{
    app::{self, menu},
    i18n::t,
    project,
    settings::preferences::AppSettings,
    state::session::{
        AppState, BOTTOM_HEIGHT_RANGE, PRIMARY_WIDTH_RANGE, SECONDARY_WIDTH_RANGE, capture_window,
        schedule_save,
    },
};

/// The application's main view.
pub(crate) struct Shell {
    /// Keeps the OS-appearance observer alive; dropping it stops following the
    /// system theme.
    _appearance: Subscription,
    /// Keeps the window-bounds observer alive so geometry changes are saved.
    _bounds: Subscription,
    /// The in-window menu bar. Rendered only where the OS has no global menu.
    menu_bar: Entity<AppMenuBar>,
}

impl Shell {
    fn new(window: &mut Window, menu_bar: Entity<AppMenuBar>, cx: &mut Context<Self>) -> Self {
        // Start-up applies the stored preference once; this follows live OS
        // changes only while the user chose `System`. A Light or Dark preference
        // must not be overridden when the OS appearance changes.
        let _appearance = window.observe_window_appearance(|window, cx| {
            let follows_system = cx
                .global::<AppSettings>()
                .settings()
                .ui
                .theme
                .mode()
                .is_none();
            let mode = ThemeMode::from(window.appearance());
            if follows_system && cx.theme().mode != mode {
                Theme::sync_system_appearance(Some(window), cx);
            }
        });

        // The observer runs once on registration; that first call is the
        // window's initial geometry, which `main` already read from the file.
        // Skipping it avoids writing the same document back on every launch.
        let first_bounds = Rc::new(Cell::new(true));
        let first_bounds_callback = first_bounds.clone();
        let _bounds = cx.observe_window_bounds(window, move |_this, window, cx| {
            if first_bounds_callback.replace(false) {
                return;
            }

            capture_window(window, cx);
            schedule_save(cx);
        });

        Self {
            _appearance,
            _bounds,
            menu_bar,
        }
    }

    /// The editor column: a header band and the (still empty) tune workspace.
    fn render_editor_column(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let project = cx
            .global::<AppState>()
            .last_project()
            .map(std::path::Path::to_path_buf);
        let has_project = project.is_some();

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // The window's identity lives in the title bar, so this band
            // carries the workspace's own line. It is deliberately not part of
            // the bar: it never drags the window.
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .px_6()
                    .when_some(project, |this, path| {
                        let available = project::is_available(&path);
                        this.child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(project::display_name(&path)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(path.display().to_string()),
                        )
                        .child(
                            div()
                                .max_w(px(420.))
                                .text_sm()
                                .text_center()
                                .text_color(cx.theme().muted_foreground)
                                .child(if available {
                                    t!("app.editor.project_hint")
                                } else {
                                    t!("app.editor.project_missing")
                                }),
                        )
                        .child(
                            Button::new("choose-another-project")
                                .label(t!("app.editor.choose_another"))
                                .on_click(|_, _, cx| project::choose(cx)),
                        )
                    })
                    .when(!has_project, |this| {
                        this.child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::MEDIUM)
                                .child(t!("app.editor.no_tune")),
                        )
                        .child(
                            div()
                                .max_w(px(440.))
                                .text_sm()
                                .text_center()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("app.editor.empty_hint")),
                        )
                        .child(
                            Button::new("open-project")
                                .label(t!("app.editor.open_project"))
                                .on_click(|_, _, cx| project::choose(cx)),
                        )
                    }),
            )
    }

    /// The panel toggles at the trailing edge of the main bar.
    /// Each reads its state from the session document, so the
    /// pressed appearance and the panel itself cannot disagree.
    fn render_panel_buttons(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let layout = &cx.global::<AppState>().data().layout;
        let (primary, secondary, bottom) = (
            layout.primary_visible,
            layout.secondary_visible,
            layout.bottom_visible,
        );

        vec![
            Button::new("toggle-primary-sidebar")
                .icon(if primary {
                    IconName::PanelLeftClose
                } else {
                    IconName::PanelLeft
                })
                .ghost()
                .small()
                // .selected(primary)
                .tooltip(tooltip(
                    "toggle-primary-sidebar",
                    t!("app.panels.toggle_primary_sidebar"),
                ))
                .on_click(|_, _, cx| {
                    // The bar under these buttons is the window's drag surface.
                    // Without this, a second click arrives there as a double
                    // click and zooms the window instead of toggling again.
                    cx.stop_propagation();
                    toggle_primary_sidebar(cx);
                })
                .into_any_element(),
            Button::new("toggle-bottom-panel")
                .icon(if bottom {
                    // The catalog has no bottom-panel "close" icon, so the
                    // right-panel close icon is turned a quarter-turn clockwise,
                    // its close bar then sits over the panel at the bottom.
                    Icon::new(IconName::PanelRightClose)
                        .rotate(radians(std::f32::consts::FRAC_PI_2))
                } else {
                    Icon::new(IconName::PanelBottom)
                })
                .ghost()
                .small()
                // .selected(bottom)
                .tooltip(tooltip(
                    "toggle-bottom-panel",
                    t!("app.panels.toggle_bottom_panel"),
                ))
                .on_click(|_, _, cx| {
                    cx.stop_propagation();
                    toggle_bottom_panel(cx);
                })
                .into_any_element(),
            Button::new("toggle-secondary-sidebar")
                .icon(if secondary {
                    IconName::PanelRightClose
                } else {
                    IconName::PanelRight
                })
                .ghost()
                .small()
                // .selected(secondary)
                .tooltip(tooltip(
                    "toggle-secondary-sidebar",
                    t!("app.panels.toggle_secondary_sidebar"),
                ))
                .on_click(|_, _, cx| {
                    cx.stop_propagation();
                    toggle_secondary_sidebar(cx);
                })
                .into_any_element(),
        ]
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h_8()
            .flex_shrink_0()
            .justify_between()
            .px_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            // The connection, signature, write-state and realtime indicators
            // join this row as the engine lands; until then it states what is
            // actually true.
            .child(t!("app.status.not_connected"))
            .child(concat!("v", env!("CARGO_PKG_VERSION")))
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = &cx.global::<AppState>().data().layout;
        let (primary, secondary, bottom) = (layout.primary(), layout.secondary(), layout.bottom());
        let primary_visible = layout.primary_visible;
        let secondary_visible = layout.secondary_visible;
        let bottom_visible = layout.bottom_visible;

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // One bar across the whole window: menus, title, panel toggles and
            // the window controls. Both sidebars therefore start below it and
            // keep their own header rows, so the layout reads as one workspace
            // rather than three columns with separate chrome.
            .child(MainBar::new(self.menu_bar.clone()).actions(self.render_panel_buttons(cx)))
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("shell-primary")
                        .on_resize(move |state, _, cx| {
                            // Only a visible sidebar has a handle, and a hidden
                            // one would report the editor's width instead.
                            if !primary_visible {
                                return;
                            }
                            let sizes = state.read(cx).sizes().clone();
                            AppState::update_global(cx, |data| {
                                data.layout.apply_primary(&sizes);
                            });
                            schedule_save(cx);
                        })
                        // Panels are left out while closed rather than hidden:
                        // a hidden panel still renders its resize handle, which
                        // would leave a stray divider on the window's edge.
                        .when(primary_visible, |this| {
                            this.child(
                                resizable_panel()
                                    .size(primary)
                                    .size_range(
                                        px(PRIMARY_WIDTH_RANGE.start)..px(PRIMARY_WIDTH_RANGE.end),
                                    )
                                    .fixed()
                                    .child(PrimarySidebar),
                            )
                        })
                        .child(
                            resizable_panel().child(
                                h_resizable("shell-panels")
                                    .on_resize(move |state, _, cx| {
                                        if !secondary_visible {
                                            return;
                                        }
                                        let sizes = state.read(cx).sizes().clone();
                                        AppState::update_global(cx, |data| {
                                            data.layout.apply_secondary(&sizes);
                                        });
                                        schedule_save(cx);
                                    })
                                    .child(
                                        resizable_panel().child(
                                            v_resizable("shell-editor")
                                                .on_resize(move |state, _, cx| {
                                                    if !bottom_visible {
                                                        return;
                                                    }
                                                    let sizes = state.read(cx).sizes().clone();
                                                    AppState::update_global(cx, |data| {
                                                        data.layout.apply_bottom(&sizes);
                                                    });
                                                    schedule_save(cx);
                                                })
                                                .child(
                                                    resizable_panel()
                                                        .child(self.render_editor_column(cx)),
                                                )
                                                // The editor keeps the space it
                                                // has when the panel opens or
                                                .when(bottom_visible, |this| {
                                                    this.child(
                                                        resizable_panel()
                                                            .size(bottom)
                                                            .size_range(
                                                                px(BOTTOM_HEIGHT_RANGE.start)
                                                                    ..px(BOTTOM_HEIGHT_RANGE.end),
                                                            )
                                                            // A panel keeps the
                                                            // height the user gave
                                                            // it when the window
                                                            // resizes; the editor
                                                            // absorbs the change.
                                                            .fixed()
                                                            .child(BottomPanel),
                                                    )
                                                }),
                                        ),
                                    )
                                    .when(secondary_visible, |this| {
                                        this.child(
                                            resizable_panel()
                                                .size(secondary)
                                                .size_range(
                                                    px(SECONDARY_WIDTH_RANGE.start)
                                                        ..px(SECONDARY_WIDTH_RANGE.end),
                                                )
                                                .fixed()
                                                .child(SecondarySidebar),
                                        )
                                    }),
                            ),
                        ),
                ),
            )
            .child(self.render_status_bar(cx))
    }
}

/// Opens the main window and remembers its handle for window-dependent commands.
///
/// Called by `main` after the settings and session documents are installed;
/// everything the shell needs from them is read here or during its first render.
pub fn open_main_window(cx: &mut App) {
    // The menu must be installed after the keymap, so item shortcuts come from
    // the bindings `init` applied.
    let menu_bar = menu::init(cx);

    let (handle, shell) = gpui_kit::open_window(app::window::options(cx), cx, |window, cx| {
        // Before the view exists: closing the window must hide the app on macOS
        // rather than leave it running with no window and no way back.
        app::window::install_close_behavior(window, cx);
        // The window opens windowed; the platform state the last session left is
        // applied here, where the window exists.
        app::window::restore(window, cx);
        cx.new(|cx| Shell::new(window, menu_bar.clone(), cx))
    })
    .expect("failed to open the main window");

    app::window::set(cx, handle, &shell);
    app::window::save_on_quit(cx);

    // The first launch shows the disclaimer until acceptance is recorded. The
    // settings file is written when the user accepts.
    if !cx
        .global::<AppSettings>()
        .settings()
        .safety
        .disclaimer_accepted
    {
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| crate::disclaimer::show(window, cx))
                .ok();
        });
    }
}

/// Shows or hides the navigation sidebar.
pub fn toggle_primary_sidebar(cx: &mut App) {
    toggle_panel(cx, |layout| {
        layout.primary_visible = !layout.primary_visible;
    });
}

/// Shows or hides the inspector sidebar.
pub fn toggle_secondary_sidebar(cx: &mut App) {
    toggle_panel(cx, |layout| {
        layout.secondary_visible = !layout.secondary_visible;
    });
}

/// Shows or hides the output panel.
pub fn toggle_bottom_panel(cx: &mut App) {
    toggle_panel(cx, |layout| {
        layout.bottom_visible = !layout.bottom_visible;
    });
}

/// Flips one panel's visibility, saves, and repaints.
///
/// All three toggles — toolbar button, in-panel close button and shortcut — go
/// through here, so they cannot disagree about what the panel's state is.
fn toggle_panel(cx: &mut App, mutate: impl FnOnce(&mut crate::state::session::LayoutState)) {
    AppState::update_global(cx, |data| mutate(&mut data.layout));
    schedule_save(cx);
    // Only the shell draws panels; the Settings window has nothing to update.
    app::window::notify_main(cx);
}

/// `title` with its shortcut appended, so an icon-only button states both what
/// it does and how to do it from the keyboard.
fn tooltip(shortcut_id: &str, title: gpui_kit::SharedString) -> gpui_kit::SharedString {
    match crate::app::shortcuts::find(shortcut_id) {
        Some(shortcut) => format!(
            "{title} ({})",
            crate::app::shortcuts::key_text(shortcut.keys())
        )
        .into(),
        None => title,
    }
}
