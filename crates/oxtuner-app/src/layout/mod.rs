//! The main window's layout view.
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
//! is what the Settings window uses. A status bar closes the window's bottom
//! edge.
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
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::resizable::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, Theme, ThemeMode,
    menu::AppMenuBar,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Window, div, px,
};

pub mod bottom_panel;
pub mod connect_dialog;
mod explorer;
pub mod primary_sidebar;
pub mod project_dialog;
pub mod secondary_sidebar;
pub mod status_bar;
pub mod titlebar;
pub mod write_dialogs;

use oxtuner_core::MenuNodeKind;

use self::{
    bottom_panel::BottomPanel, explorer::Explorer, primary_sidebar::PrimarySidebar,
    secondary_sidebar::SecondarySidebar, status_bar::StatusBarView, titlebar::MainBar,
};
use crate::{
    app::{self, menu},
    editor::{dialog::DialogEditor, table::TableEditor},
    i18n::t,
    realtime::RealtimeView,
    settings::preferences::AppSettings,
    state::session::{
        AppState, BOTTOM_HEIGHT_RANGE, PRIMARY_WIDTH_RANGE, SECONDARY_WIDTH_RANGE, WorkView,
        capture_window, schedule_save,
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
    /// The definition-driven navigation tree. The sidebar renders it; the
    /// editor column shows what is selected.
    explorer: Entity<Explorer>,
    /// The table editor; one instance so the Y-axis preference and cursor
    /// survive switching between nodes.
    table_editor: Entity<TableEditor>,
    /// The dialog editor; one instance so the edit-in-progress survives a
    /// core echo.
    dialog_editor: Entity<DialogEditor>,
    /// The live charts workspace, kept alive so its view state survives a
    /// switch back to the tune.
    realtime: Entity<RealtimeView>,
    /// The output panel; one instance so its selected category survives a
    /// hide and show.
    bottom_panel: Entity<BottomPanel>,
    /// Which view a dialog-backed node shows: its grid or its constants.
    tab: EditorTab,
    /// The node the tab belongs to, so a new selection starts on the grid.
    tab_node: Option<String>,
    /// Keeps the selection observer alive so the editor repaints on a click.
    _explorer: Subscription,
}

/// The two views a dialog that wraps a table offers.
///
/// `Tuning ▸ VE Table` is a dialog, not a table: its north panel holds the
/// 16×16 map and its south panel holds a few options. The grid is what the
/// reader came for, so it opens first; the constants are one click away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    /// The grid of the table or curve the dialog embeds.
    Table,
    /// The dialog's own rows.
    Constants,
}

impl AppLayout {
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

        // A project folder can be moved or unmounted while it is open. The
        // render path must not `stat` it every frame, so the check runs when
        // the window is activated — the moment the user comes back to the app
        // — and the result is cached in `CoreStatus`.
        let _activation = cx.observe_window_activation(window, |_, _, cx| {
            crate::core::refresh_project_availability(cx);
        });

        // The tree is its own entity: it observes the core, owns the gpui-kit
        // tree state and keeps the selection. The layout watches it only to
        // repaint the editor column when the selection changes.
        let explorer = cx.new(Explorer::new);
        let table_editor = cx.new(TableEditor::new);
        let dialog_editor = cx.new(DialogEditor::new);
        let realtime = cx.new(RealtimeView::new);
        let diagnostics = cx.new(|cx| bottom_panel::DiagnosticsView::new(window, cx));
        let bottom_panel = cx.new(|_| BottomPanel::new(diagnostics));
        let _explorer = cx.observe(&explorer, |this, _, cx| {
            this.sync_editors(cx);
            cx.notify();
        });

        Self {
            _appearance,
            _bounds,
            _activation,
            menu_bar,
            explorer,
            table_editor,
            dialog_editor,
            realtime,
            bottom_panel,
            tab: EditorTab::Table,
            tab_node: None,
            _explorer,
        }
    }

    /// Shows the selected node's editor. Tables and curves share the grid: a
    /// curve resolves as a one-row table, so its bins and values edit the same
    /// way. Dialogs get the grid they embed, with their constants behind a tab,
    /// or the constants alone. Every other kind keeps the placeholder.
    fn sync_editors(&mut self, cx: &mut Context<Self>) {
        let selected = self.explorer.read(cx).selected().cloned();
        let node_id = selected.as_ref().map(|node| node.id.clone());
        if node_id != self.tab_node {
            self.tab_node = node_id;
            self.tab = EditorTab::Table;
            if cx.global::<AppState>().data().layout.work_view == WorkView::Charts {
                set_work_view(WorkView::Tune, cx);
            }
        }

        let (table_target, dialog_target, label) = match selected {
            Some(node) => {
                let open_table =
                    node.kind == MenuNodeKind::Table || node.kind == MenuNodeKind::Curve;
                let dialog_has_grid = node.kind == MenuNodeKind::Dialog && node.embedded.is_some();
                let table_target = if open_table {
                    node.target.clone()
                } else if dialog_has_grid && self.tab == EditorTab::Table {
                    node.embedded.clone()
                } else {
                    None
                };
                let dialog_target = if node.kind == MenuNodeKind::Dialog
                    && !(dialog_has_grid && self.tab == EditorTab::Table)
                {
                    node.target.clone()
                } else {
                    None
                };
                (
                    table_target,
                    dialog_target,
                    gpui_kit::SharedString::from(node.label),
                )
            }
            None => (None, None, t!("app.editor.nothing_selected")),
        };

        self.table_editor.update(cx, |editor, cx| {
            editor.show(label.clone(), table_target.as_deref(), cx);
        });
        self.dialog_editor.update(cx, |editor, cx| {
            editor.show(label, dialog_target.as_deref(), cx);
        });
    }

    /// The editor column: the node selected in the navigation tree (or an
    /// honest empty workspace).
    ///
    /// Project identity lives in the title bar and the Projects dialog; the
    /// editor shows only the selected node, plus a read-only banner when the
    /// workspace itself is read-only.
    fn render_editor_column(&self, cx: &mut Context<Self>) -> AnyElement {
        // The editor names what the core owns. `AppState::last_project` only
        // remembers the path between launches.
        let status = crate::core::CoreService::status(cx);

        // The opening splash covers the workspace. Leave a blank surface
        // underneath it rather than the empty-project call to action, which
        // would be the wrong thing to show for the half-frame before the
        // overlay paints.
        if status.pending_project.is_some() {
            return div()
                .size_full()
                .bg(cx.theme().background)
                .into_any_element();
        }

        if cx.global::<AppState>().data().layout.work_view == WorkView::Charts {
            return div()
                .size_full()
                .min_h_0()
                .child(self.realtime.clone())
                .into_any_element();
        }

        let has_definition = status.definition().is_some();
        let has_project = status.project.is_some();
        // Built once here rather than joined in the banner closure: the
        // reasons are already joined in the project summary at open time, and
        // a writable project allocates nothing at all.
        let read_only_banner = status
            .project
            .as_ref()
            .filter(|project| project.read_only)
            .and_then(|project| project.read_only_reason.as_deref())
            .map(|reason| {
                div()
                    .flex_shrink_0()
                    .px_4()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(t!("app.editor.read_only", reason = reason))
                    .into_any_element()
            });
        // `project_missing` is refreshed on window activation and project
        // open, never by a filesystem call in this render.
        let available = !status.project_missing;
        let selected = self.explorer.read(cx).selected().cloned();
        let has_selection = selected.is_some();
        let is_table = selected.as_ref().is_some_and(|node| {
            node.kind == MenuNodeKind::Table || node.kind == MenuNodeKind::Curve
        });
        let is_dialog = selected
            .as_ref()
            .is_some_and(|node| node.kind == MenuNodeKind::Dialog);
        let dialog_has_grid = is_dialog
            && selected
                .as_ref()
                .is_some_and(|node| node.embedded.is_some());
        let show_table = is_table || (dialog_has_grid && self.tab == EditorTab::Table);
        let show_dialog = is_dialog && !show_table;

        let heading = if has_definition {
            t!("app.editor.nothing_selected")
        } else {
            t!("app.editor.no_tune")
        };
        let empty_hint = if has_project && !available {
            t!("app.editor.project_missing")
        } else if has_definition {
            t!("app.editor.select_hint")
        } else {
            t!("app.editor.empty_hint")
        };

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // A read-only tune is a property of the workspace, not of the
            // selected node, so it stays visible above the editor body.
            .when_some(read_only_banner, |this, banner| this.child(banner))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .when(dialog_has_grid, |this| this.child(self.render_tabs(cx)))
                    .when(show_table, |this| {
                        this.child(div().flex_1().min_h_0().child(self.table_editor.clone()))
                    })
                    .when(show_dialog, |this| {
                        this.child(div().flex_1().min_h_0().child(self.dialog_editor.clone()))
                    })
                    .when(!show_table && !show_dialog, |this| {
                        this.items_center()
                            .justify_center()
                            .gap_3()
                            .px_6()
                            .when_some(selected, |this, node| {
                                this.child(
                                    Icon::new(explorer::icon(node.kind))
                                        .large()
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(node.label.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(explorer::summary(&node)),
                                )
                                .child(
                                    div()
                                        .max_w(px(440.))
                                        .text_sm()
                                        .text_center()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(explorer::hint(node.kind)),
                                )
                            })
                            .when(!has_selection, |this| {
                                this.child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(heading),
                                )
                                .child(
                                    div()
                                        .max_w(px(440.))
                                        .text_sm()
                                        .text_center()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(empty_hint),
                                )
                                .when(!has_definition, |this| {
                                    this.child(
                                        Button::new("open-project")
                                            .label(t!("app.editor.open_project"))
                                            .primary()
                                            .on_click(|_, window, cx| {
                                                project_dialog::show(window, cx);
                                            }),
                                    )
                                })
                            })
                    }),
            )
            .into_any_element()
    }

    /// The grid/constants switch above a dialog that wraps a table.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h_9()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("editor-tab-table")
                    .label(t!("app.editor.tab_table"))
                    .small()
                    .ghost()
                    .selected(self.tab == EditorTab::Table)
                    .on_click(cx.listener(|this, _, _, cx| this.set_tab(EditorTab::Table, cx))),
            )
            .child(
                Button::new("editor-tab-constants")
                    .label(t!("app.editor.tab_constants"))
                    .small()
                    .ghost()
                    .selected(self.tab == EditorTab::Constants)
                    .on_click(cx.listener(|this, _, _, cx| this.set_tab(EditorTab::Constants, cx))),
            )
    }

    /// Switches which view a dialog-backed node shows.
    fn set_tab(&mut self, tab: EditorTab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.sync_editors(cx);
        cx.notify();
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
        let work_view = layout.work_view;
        let pending = crate::core::CoreService::status(cx)
            .pending_project
            .is_some();

        vec![
            Button::new("show-tune")
                .label(t!("app.view.tune"))
                .small()
                .ghost()
                .selected(work_view == WorkView::Tune)
                .disabled(pending)
                .tooltip(tooltip("show-tune", t!("app.view.tune_tooltip")))
                .on_click(|_, _, cx| {
                    cx.stop_propagation();
                    show_tune(cx);
                })
                .into_any_element(),
            Button::new("show-charts")
                .label(t!("app.view.charts"))
                .small()
                .ghost()
                .selected(work_view == WorkView::Charts)
                .disabled(pending)
                .tooltip(tooltip("show-charts", t!("app.view.charts_tooltip")))
                .on_click(|_, _, cx| {
                    cx.stop_propagation();
                    show_charts(cx);
                })
                .into_any_element(),
            div()
                .w(px(1.))
                .h_4()
                .bg(cx.theme().border)
                .into_any_element(),
            Button::new("toggle-primary-sidebar")
                .icon(if primary {
                    IconName::PanelLeftClose
                } else {
                    IconName::PanelLeft
                })
                .ghost()
                .small()
                .disabled(pending)
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

/// The editor column while a project is being opened.
///
/// The shape follows the real editor — header, toolbar row, grid — so the
/// switch to the tune does not move the frame around. Only the official
/// The opening splash over the workspace.
///
/// It sits between the title bar and the status bar, covers the sidebars, the
/// editor and the bottom panel, and blocks mouse interaction with everything
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = &cx.global::<AppState>().data().layout;
        let (primary, secondary, bottom) = (layout.primary(), layout.secondary(), layout.bottom());
        let primary_visible = layout.primary_visible;
        let secondary_visible = layout.secondary_visible;
        let bottom_visible = layout.bottom_visible;
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
            .child(MainBar::new(self.menu_bar.clone()).actions(self.render_panel_buttons(cx)))
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
                                            px(PRIMARY_WIDTH_RANGE.start)
                                                ..px(PRIMARY_WIDTH_RANGE.end),
                                        )
                                        .fixed()
                                        .child(PrimarySidebar::new(self.explorer.clone())),
                                )
                            })
                            .child(
                                resizable_panel().child(
                                    h_resizable("layout-panels")
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
                                                v_resizable("layout-editor")
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
                                                                        ..px(
                                                                            BOTTOM_HEIGHT_RANGE.end
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

/// Switches the editor column to the tune.
pub fn show_tune(cx: &mut App) {
    set_work_view(WorkView::Tune, cx);
}

/// Switches the editor column to the live charts.
pub fn show_charts(cx: &mut App) {
    set_work_view(WorkView::Charts, cx);
}

fn set_work_view(view: WorkView, cx: &mut App) {
    let current = cx.global::<AppState>().data().layout.work_view;
    if current == view {
        return;
    }
    AppState::update_global(cx, |data| data.layout.work_view = view);
    schedule_save(cx);
    app::window::notify_main(cx);
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
    use super::*;
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
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                        Default::default(),
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
