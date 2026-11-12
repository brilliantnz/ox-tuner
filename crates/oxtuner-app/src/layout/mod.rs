//! The main window's layout view.
//!
//! Three resizable panels under one title bar, named by role:
//!
//! | Panel | Where | Chrome |
//! | --- | --- | --- |
//! | **Primary sidebar** | left | section labels only; no header row |
//! | **Secondary sidebar** | right | header row with its close button |
//! | **Bottom panel** | under the workspace only | header row with its close button |
//!
//! The bar spans the whole window: menus, title, the three panel toggles and (off
//! macOS) the window controls. Because of that no column reaches the window's top
//! edge, and no column needs its own drag strip. The split-bar arrangement — a
//! drag strip per column — is still in `titlebar.rs` ([`titlebar::DragTop`]) and
//! is what the Settings window uses. A status bar closes the window's bottom
//! edge.
//!
//! The middle column is the [`Workspace`]: a dock whose tabs hold the cluster,
//! the realtime charts and every open document. Selecting a node in the project
//! tree opens its tab, and whichever tab is in front is what the tree
//! highlights.
//!
//! Sizes, widths and open state live in `state.json`; the view reads them when it
//! renders and writes them back on a drag or a toggle.
//!
//! Views live in this crate next to the state they render; they move to
//! `oxtuner-ui` only when a widget is reused or the split earns its keep (crate
//! boundary: ADR-0001).

use std::{cell::Cell, rc::Rc};

use gpui_kit::assets::IconName;
use gpui_kit::base::v_flex;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::resizable::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, Theme, ThemeMode, menu::AppMenuBar,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Window, div, px,
};

pub mod bottom_panel;
pub mod connect_dialog;
pub(crate) mod primary_sidebar;
pub mod project_dialog;
pub mod secondary_sidebar;
pub mod status_bar;
pub mod titlebar;
pub(crate) mod workspace;
pub mod write_dialogs;

use self::{
    bottom_panel::BottomPanel,
    primary_sidebar::{Explorer, PrimarySidebar},
    secondary_sidebar::SecondarySidebar,
    status_bar::StatusBarView,
    titlebar::MainBar,
    workspace::{ActiveDocument, Workspace},
};
use crate::{
    app::{self, menu},
    i18n::t,
    settings::preferences::AppSettings,
    state::session::{
        AppState, BOTTOM_PANEL_HEIGHT_RANGE, PRIMARY_SIDEBAR_WIDTH_RANGE,
        SECONDARY_SIDEBAR_WIDTH_RANGE, capture_window, schedule_save,
    },
};

/// The application's main view.
pub(crate) struct AppLayout {
    /// Keeps the OS-appearance observer alive; dropping it stops following the
    /// system theme.
    _appearance: Subscription,
    /// Keeps the window-bounds observer alive so geometry changes are saved.
    _bounds: Subscription,
    /// Re-checks project availability when the window comes back to the front.
    _activation: Subscription,
    /// The in-window menu bar. Rendered only where the OS has no global menu.
    menu_bar: Entity<AppMenuBar>,
    /// The definition-driven navigation tree. The sidebar renders it; picking a
    /// leaf opens its tab in the workspace.
    explorer: Entity<Explorer>,
    /// The dock of open documents, with the cluster and the charts pinned
    /// first.
    workspace: Entity<Workspace>,
    /// The last selection the workspace was told about.
    ///
    /// The explorer notifies on expansion as well as selection, and a core
    /// status change can rebuild it; only a new pick should raise a tab.
    selection_serial: u64,
    /// The output panel; one instance so its selected category survives a
    /// hide and show.
    bottom_panel: Entity<BottomPanel>,
    /// Keeps the selection observer alive so a click opens its tab.
    _explorer: Subscription,
    /// Keeps the workspace observer alive so the tree follows the front tab.
    _workspace: Subscription,
}

impl AppLayout {
    #[expect(
        clippy::used_underscore_binding,
        reason = "the underscore marks observers kept only for their lifetime"
    )]
    fn new(window: &mut Window, menu_bar: Entity<AppMenuBar>, cx: &mut Context<'_, Self>) -> Self {
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
        let first_bounds_callback = Rc::clone(&first_bounds);
        let _bounds = cx.observe_window_bounds(window, move |_this, window, cx| {
            if first_bounds_callback.replace(false) {
                return;
            }

            capture_window(window, cx);
            schedule_save(cx);
        });

        // A project folder can be moved or unmounted while it is open. The
        // render path must not `stat` it every frame, so the check runs when
        // the window is activated — the moment the user comes back to the app
        // — and the result is cached in `CoreStatus`.
        let _activation = cx.observe_window_activation(window, |_, _, cx| {
            crate::core::refresh_project_availability(cx);
        });

        // The tree is its own entity: it observes the core, owns the gpui-kit
        // tree state and keeps the selection. The layout watches it only to
        // open the picked node's tab.
        let explorer = cx.new(Explorer::new);
        let workspace = cx.new(|cx| Workspace::new(window, cx));
        let diagnostics = cx.new(|cx| bottom_panel::DiagnosticsView::new(window, cx));
        let bottom_panel = cx.new(|_| BottomPanel::new(diagnostics));
        let _explorer = cx.observe(&explorer, |this, explorer, cx| {
            let (selected, serial) = {
                let explorer = explorer.read(cx);
                (explorer.selected().cloned(), explorer.selection_serial())
            };
            if serial == this.selection_serial {
                return;
            }
            this.selection_serial = serial;

            let Some(node) = selected else {
                return;
            };

            this.workspace
                .update(cx, |workspace, cx| workspace.open_node(node, cx));
        });

        // The dock is the authority on which document is in front. Clicking a
        // tab, or arriving at one by any other route, moves the tree there.
        let _workspace = cx.subscribe(&workspace, |this, _, event: &ActiveDocument, cx| {
            this.explorer.update(cx, |explorer, cx| {
                explorer.follow_active(event.0.as_deref(), cx);
            });
        });

        Self {
            _appearance,
            _bounds,
            _activation,
            menu_bar,
            explorer,
            workspace,
            selection_serial: 0,
            bottom_panel,
            _explorer,
            _workspace,
        }
    }

    /// Opens Diagnostics with a notification's filter.
    ///
    /// The panel is made visible in the session document, then the view is
    /// updated once so the tab switch and the filter land in the same frame.
    pub(crate) fn show_diagnostics(
        &mut self,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.bottom_panel
            .update(cx, |panel, cx| panel.show_diagnostics(filter, window, cx));
        cx.notify();
    }

    /// The panel toggles at the trailing edge of the main bar.
    /// Each reads its state from the session document, so the
    /// pressed appearance and the panel itself cannot disagree.
    fn render_panel_buttons(cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let layout = &cx.global::<AppState>().data().layout;
        let (primary, secondary, bottom) = (
            layout.primary_sidebar_visible,
            layout.secondary_sidebar_visible,
            layout.bottom_panel_visible,
        );
        let pending = crate::core::CoreService::status(cx)
            .pending_project
            .is_some();

        vec![
            Button::new("toggle-primary-sidebar")
                .icon(if primary {
                    IconName::PanelLeftClose
                } else {
                    IconName::PanelLeft
                })
                .ghost()
                .small()
                .disabled(pending)
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
                    IconName::PanelBottomClose
                } else {
                    IconName::PanelBottom
                })
                .ghost()
                .small()
                .disabled(pending)
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
                .disabled(pending)
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
}

/// The opening splash over the workspace.
///
/// It sits between the title bar and the status bar, covers the sidebars, the
/// workspace and the bottom panel, and blocks mouse interaction with everything
/// underneath. The window chrome stays visible: the title bar names the folder
/// and the status bar says what is happening, while their controls are
/// disabled until the open finishes.
fn render_opening_splash(name: &gpui_kit::SharedString, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;

    v_flex()
        .id("opening-project-splash")
        .occlude()
        .absolute()
        .inset_0()
        .items_center()
        .justify_center()
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        .child(
            v_flex()
                .items_center()
                .gap_3()
                .max_w(px(420.))
                .px_6()
                .text_center()
                .child(
                    ProgressCircle::new("opening-project-ring")
                        .loading(true)
                        .with_size(px(160.))
                        .color(cx.theme().primary)
                        .accessibility_label(t!("app.editor.opening_project"))
                        .child(
                            Icon::new(IconName::Cpu)
                                .with_size(px(40.))
                                .text_color(muted),
                        ),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::MEDIUM)
                        .child(t!("app.editor.opening_project")),
                )
                .child(
                    div()
                        .max_w_full()
                        .truncate()
                        .text_sm()
                        .text_color(muted)
                        .child(name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(muted)
                        .child(t!("app.editor.opening_hint")),
                ),
        )
        .into_any_element()
}

impl Render for AppLayout {
    #[expect(
        clippy::too_many_lines,
        reason = "the main window layout composes every persistent region in one pass"
    )]
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let layout = &cx.global::<AppState>().data().layout;
        let (primary, secondary, bottom) = (
            layout.primary_sidebar(),
            layout.secondary_sidebar(),
            layout.bottom_panel(),
        );
        let primary_sidebar_visible = layout.primary_sidebar_visible;
        let secondary_sidebar_visible = layout.secondary_sidebar_visible;
        let bottom_panel_visible = layout.bottom_panel_visible;
        let pending = crate::core::CoreService::status(cx)
            .pending_project
            .as_ref()
            .map(|pending| pending.name.clone());

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // One bar across the whole window: menus, title, panel toggles and
            // the window controls. Both sidebars therefore start below it and
            // keep their own header rows, so the layout reads as one workspace
            // rather than three columns with separate chrome.
            .child(MainBar::new(self.menu_bar.clone()).actions(Self::render_panel_buttons(cx)))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        h_resizable("layout-primary")
                            .on_resize(move |state, _, cx| {
                                // Only a visible sidebar has a handle, and a hidden
                                // one would report the editor's width instead.
                                if !primary_sidebar_visible {
                                    return;
                                }
                                let sizes = state.read(cx).sizes().clone();
                                AppState::update_global(cx, |data| {
                                    data.layout.apply_primary_sidebar(&sizes);
                                });
                                schedule_save(cx);
                            })
                            // Panels are left out while closed rather than hidden:
                            // a hidden panel still renders its resize handle, which
                            // would leave a stray divider on the window's edge.
                            .when(primary_sidebar_visible, |this| {
                                this.child(
                                    resizable_panel()
                                        .size(primary)
                                        .size_range(
                                            px(PRIMARY_SIDEBAR_WIDTH_RANGE.start)
                                                ..px(PRIMARY_SIDEBAR_WIDTH_RANGE.end),
                                        )
                                        .fixed()
                                        .child(PrimarySidebar::new(self.explorer.clone())),
                                )
                            })
                            .child(
                                resizable_panel().child(
                                    h_resizable("layout-panels")
                                        .on_resize(move |state, _, cx| {
                                            if !secondary_sidebar_visible {
                                                return;
                                            }
                                            let sizes = state.read(cx).sizes().clone();
                                            AppState::update_global(cx, |data| {
                                                data.layout.apply_secondary_sidebar(&sizes);
                                            });
                                            schedule_save(cx);
                                        })
                                        .child(
                                            resizable_panel().child(
                                                v_resizable("layout-editor")
                                                    .on_resize(move |state, _, cx| {
                                                        if !bottom_panel_visible {
                                                            return;
                                                        }
                                                        let sizes = state.read(cx).sizes().clone();
                                                        AppState::update_global(cx, |data| {
                                                            data.layout.apply_bottom_panel(&sizes);
                                                        });
                                                        schedule_save(cx);
                                                    })
                                                    .child(
                                                        resizable_panel()
                                                            .child(self.workspace.clone()),
                                                    )
                                                    // The editor keeps the space it
                                                    // has when the panel opens or
                                                    .when(bottom_panel_visible, |this| {
                                                        this.child(
                                                            resizable_panel()
                                                                .size(bottom)
                                                                .size_range(
                                                                    px(BOTTOM_PANEL_HEIGHT_RANGE.start)
                                                                        ..px(
                                                                            BOTTOM_PANEL_HEIGHT_RANGE.end
                                                                        ),
                                                                )
                                                                // A panel keeps the
                                                                // height the user gave
                                                                // it when the window
                                                                // resizes; the editor
                                                                // absorbs the change.
                                                                .fixed()
                                                                .child(self.bottom_panel.clone()),
                                                        )
                                                    }),
                                            ),
                                        )
                                        .when(secondary_sidebar_visible, |this| {
                                            this.child(
                                                resizable_panel()
                                                    .size(secondary)
                                                    .size_range(
                                                        px(SECONDARY_SIDEBAR_WIDTH_RANGE.start)
                                                            ..px(SECONDARY_SIDEBAR_WIDTH_RANGE.end),
                                                    )
                                                    .fixed()
                                                    .child(SecondarySidebar),
                                            )
                                        }),
                                ),
                            ),
                    )
                    .when_some(pending.as_ref(), |this, name| {
                        this.child(render_opening_splash(name, cx))
                    }),
            )
            .child(StatusBarView)
    }
}

/// Opens the main window and remembers its handle for window-dependent commands.
///
/// Called by `main` after the settings and session documents are installed;
/// everything the layout needs from them is read here or during its first render.
///
/// # Panics
///
/// Panics when the platform refuses to open the main window.
pub fn open_main_window(cx: &mut App) {
    // The menu must be installed after the keymap, so item shortcuts come from
    // the bindings `init` applied.
    let menu_bar = menu::init(cx);

    let (handle, view) = gpui_kit::open_window(app::window::options(cx), cx, |window, cx| {
        // Before the view exists: closing the window must hide the app on macOS
        // rather than leave it running with no window and no way back.
        app::window::install_close_behavior(window, cx);
        // The window opens windowed; the platform state the last session left is
        // applied here, where the window exists.
        app::window::restore(window, cx);
        cx.new(|cx| AppLayout::new(window, menu_bar.clone(), cx))
    })
    .expect("failed to open the main window");

    app::window::set(cx, handle, &view);
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
        layout.primary_sidebar_visible = !layout.primary_sidebar_visible;
    });
}

/// Shows or hides the inspector sidebar.
pub fn toggle_secondary_sidebar(cx: &mut App) {
    toggle_panel(cx, |layout| {
        layout.secondary_sidebar_visible = !layout.secondary_sidebar_visible;
    });
}

/// Shows or hides the output panel.
pub fn toggle_bottom_panel(cx: &mut App) {
    toggle_panel(cx, |layout| {
        layout.bottom_panel_visible = !layout.bottom_panel_visible;
    });
}

/// Opens the panel on Diagnostics and puts `filter` in its search box.
///
/// The notification surface calls this when the user asks for details. The
/// panel is forced visible first, because a notification can arrive while it
/// was hidden; the filter and the tab then change in the same update.
pub(crate) fn show_diagnostics(window: &mut Window, cx: &mut App, filter: Option<String>) {
    AppState::update_global(cx, |data| data.layout.bottom_panel_visible = true);
    schedule_save(cx);

    match app::window::main_view(cx) {
        Some(view) => {
            view.update(cx, |this, cx| {
                this.show_diagnostics(filter, window, cx);
            });
        }
        None => app::window::notify_main(cx),
    }
}

/// Flips one panel's visibility, saves, and repaints.
///
/// All three toggles — toolbar button, in-panel close button and shortcut — go
/// through here, so they cannot disagree about what the panel's state is.
fn toggle_panel(cx: &mut App, mutate: impl FnOnce(&mut crate::state::session::LayoutState)) {
    AppState::update_global(cx, |data| mutate(&mut data.layout));
    schedule_save(cx);
    // Only the main layout draws panels; the Settings window has nothing to update.
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

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]

    use super::*;
    use gpui_kit::Point;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, Bounds, Context, IntoElement, Render, TestAppContext, Window,
        WindowBounds, WindowOptions, div, size,
    };

    use crate::core::PendingProject;

    struct Host {
        pending: PendingProject,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            div()
                .relative()
                .size_full()
                .child(render_opening_splash(&self.pending.name, cx))
        }
    }

    fn open(cx: &mut TestAppContext) -> AnyWindowHandle {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let pending = PendingProject {
                path: std::path::PathBuf::from("/projects/roadster"),
                name: "roadster".into(),
                op: None,
            };
            let host = cx.new(|_| Host { pending });
            let (handle, _) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(1200.), px(800.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |_, _| host,
            )
            .expect("open test window");
            handle
        })
    }

    /// The loading state must render with the real gpui-kit progress ring,
    /// over the workspace, and survive a frame of animation.
    #[gpui_kit::test]
    fn the_opening_splash_renders(cx: &mut TestAppContext) {
        let handle = open(cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
    }
}
