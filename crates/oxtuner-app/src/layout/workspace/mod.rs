//! The editor workspace: the dock that holds the cluster, the realtime charts
//! and every open document.
//!
//! The center region is one dock. Its first two tabs are the cluster and the
//! realtime charts: neither is a document, both are pinned, and neither can be
//! closed. Documents open to their right and close when the reader is done with
//! them. Splits come from the dock itself (drag a tab to an edge), so this
//! module only decides what opens and where a new tab lands.
//!
//! Tabs are per document, not per selection: [`open_node`](Workspace::open_node)
//! finds the tab already showing a node before it creates one, and each tab owns
//! its editors so two tables never share a cursor or an edit in progress.
//!
//! Whichever tab the dock raises is reported as [`ActiveDocument`], so the
//! navigation tree can point at the same document instead of its own last click.
//!
//! Switching to another project keeps the open tabs. A tab whose target the new
//! definition does not declare shows its own failure, which is the honest
//! answer: the document the reader opened is gone, not silently replaced.

mod cluster;
mod editor;
mod realtime;

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::v_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, Entity, EntityId, EventEmitter, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, px,
};
use oxtuner_core::MenuNode;
use oxtuner_ui::dock::{
    BasePanelView, DockArea, DockEvent, DockLayout, DockPlacement, DockSkin, InsertTarget, Panel,
    PanelId, PanelStyle, panel_handle,
};
use oxtuner_ui::tabs::TabVariant;

use self::{cluster::ClusterPanel, editor::EditorPanel, realtime::RealtimePanel};
use crate::{core::CoreService, i18n::t};

/// Tabs that belong to the workspace itself rather than to a document.
///
/// The cluster and the realtime charts are pinned, and they are always the
/// first two. A new document lands immediately after them, so opening one never
/// pushes them out of the way and never buries the new tab at the far end of a
/// long strip.
const PINNED_TABS: usize = 2;

/// The tab the workspace starts on cluster page.
const STARTUP_TAB: usize = 0;

/// The document the dock just raised.
///
/// A panel emits it when the dock displays it, and the workspace relays it, so
/// the window hears about a tab click without reading the dock's private state.
/// `None` means the front tab is the cluster or the charts: neither is a
/// document, so nothing in the navigation tree is showing in front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveDocument(pub(crate) Option<String>);

/// The dock of open documents, plus the two tabs it is always pinned to.
pub(crate) struct Workspace {
    /// The dock area draws the tabs, the splits and the drag behavior.
    area: Entity<DockArea>,
    /// The window the dock needs for a layout edit. Stored rather than captured
    /// per call so a command handled from the menu can still reach a window.
    window: AnyWindowHandle,
    /// The pinned cluster tab.
    cluster: Entity<ClusterPanel>,
    /// The pinned charts tab.
    realtime: Entity<RealtimePanel>,
    /// Open document tabs, in the order they were opened. The dock is the
    /// authority on what is on screen; this list only remembers the entity
    /// behind each tab and the subscription that follows it, and it is pruned
    /// when the dock reports a close.
    editors: Vec<OpenDocument>,
    /// Keeps the core observer alive so a project or definition change repaints
    /// the tab strip.
    _core: Subscription,
    /// Keeps the dock observer alive so a closed tab's editor is dropped.
    _layout: Subscription,
    /// The pinned tabs' activation subscriptions.
    _pinned: Vec<Subscription>,
}

/// One open document and the subscription that follows it.
struct OpenDocument {
    panel: Entity<EditorPanel>,
    /// Kept alive so the workspace hears when this tab takes the front.
    _active: Subscription,
}

impl EventEmitter<ActiveDocument> for Workspace {}

impl Workspace {
    /// Builds the dock with the cluster and the charts as its only tabs.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let handle = window.window_handle();
        let cluster = cx.new(ClusterPanel::new);
        let realtime = cx.new(RealtimePanel::new);

        let workspace = cx.weak_entity();
        let (area, skin) = DockSkin::dock_area("workspace", None, window, cx);

        // Every group keeps its strip, even with a single document in it: a
        // split puts one document per group, and a plain title would give no
        // way back to the other tabs of that group.
        skin.set_panel_style(PanelStyle::TabBar, cx);
        skin.set_tab_variant(TabVariant::Folder, cx);
        skin.set_close_button_visible(true, cx);

        // The skin reports every pick, because a split shows one document per
        // group and picking the one already displayed changes nothing the dock
        // would announce.
        skin.set_tab_picked(Rc::new(move |panel, cx| {
            workspace
                .update(cx, |workspace, cx| workspace.follow_panel(panel, cx))
                .ok();
        }));

        // A tab with unsaved work asks first; every other tab closes at once.
        let closer = cx.weak_entity();
        skin.set_close_request(Rc::new(move |panel, window, cx| {
            closer
                .update(cx, |workspace, cx| {
                    workspace.confirm_close(panel, window, cx)
                })
                .unwrap_or(true)
        }));

        let layout = DockLayout::tabs()
            .panel_view(panel_handle(cluster.clone()), cx)
            .panel_view(panel_handle(realtime.clone()), cx)
            .active_index(STARTUP_TAB);
        area.update(cx, |area, cx| area.set_center(layout, window, cx));

        // A panel tells the dock which document is in front; the workspace
        // repeats it for the window.
        let cluster_active = cx.subscribe(&cluster, |_, _, event, cx| relay(event, cx));
        let realtime_active = cx.subscribe(&realtime, |_, _, event, cx| relay(event, cx));

        // The workspace repaints when the core publishes a definition or a
        // read-only reason; a frame repaint is the window's job, not this one.
        // The dock is a child entity, so the tab strip needs its own nudge to
        // pick up a modified flag that moved.
        let core = cx.observe_global::<CoreService>(|this, cx| {
            this.area.update(cx, |_, cx| cx.notify());
            cx.notify();
        });

        // The dock reports a layout change from inside its own update, so the
        // borrow it holds on the area is still alive when the event fires.
        // Pruning is deferred past that update instead of reading the area now.
        let layout = cx.subscribe(&area, |_, _, event, cx| {
            if !matches!(event, DockEvent::LayoutChanged) {
                return;
            }

            let workspace = cx.entity().downgrade();
            cx.defer(move |cx| {
                workspace.update(cx, Workspace::prune).ok();
            });
        });

        Self {
            area,
            window: handle,
            cluster,
            realtime,
            editors: Vec::new(),
            _core: core,
            _layout: layout,
            _pinned: vec![cluster_active, realtime_active],
        }
    }

    /// Opens `node` in a tab, or shows the tab it already has.
    pub(crate) fn open_node(&mut self, node: MenuNode, cx: &mut Context<'_, Self>) {
        self.prune(cx);

        if let Some(existing) = self
            .editors
            .iter()
            .find(|document| document.panel.read(cx).node_id() == node.id)
        {
            let panel = existing.panel.entity_id();
            self.reveal(panel, cx);
            return;
        }

        let panel = cx.new(|cx| EditorPanel::new(node, cx));
        let handle = panel_handle(panel.clone());
        self.add_tab(handle, PINNED_TABS + self.editors.len(), cx);

        // Every open tab keeps its own listener; the workspace relays what the
        // dock tells each panel so the window can follow the front document.
        let active = cx.subscribe(&panel, |_, _, event, cx| relay(event, cx));
        self.editors.push(OpenDocument {
            panel,
            _active: active,
        });
        cx.notify();
    }

    /// Asks before closing a tab that holds unsaved work.
    ///
    /// Returns whether the dock may close the panel now. A modified document
    /// gets a Save / Don't Save / Cancel dialog instead; the two closing
    /// choices close this tab themselves, after sending the command.
    fn confirm_close(
        &mut self,
        panel: PanelId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(document) = self
            .editors
            .iter()
            .find(|document| PanelId::from(document.panel.entity_id()) == panel)
        else {
            return true;
        };

        if !document.panel.read(cx).modified(cx) {
            return true;
        }

        let label = SharedString::from(document.panel.read(cx).label().to_owned());
        let target = document.panel.read(cx).target().map(str::to_owned);
        let workspace = cx.weak_entity();

        crate::app::dialog::open(window, cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let save = workspace.clone();
            let discard = workspace.clone();
            let target = target.clone();

            dialog
                .title(t!("app.close_unsaved.title"))
                .width(px(460.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .child(t!("app.close_unsaved.body", name = label.clone())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(t!("app.close_unsaved.detail")),
                        ),
                )
                .footer(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            Button::new("close-unsaved-save")
                                .label(t!("app.close_unsaved.save"))
                                .primary()
                                .w_full()
                                .on_click(move |_, window, cx| {
                                    crate::core::save_tune(cx);
                                    window.close_dialog(cx);
                                    save.update(cx, |workspace, cx| {
                                        workspace.close_panel(panel, window, cx);
                                    })
                                    .ok();
                                }),
                        )
                        .child(
                            Button::new("close-unsaved-discard")
                                .label(t!("app.close_unsaved.discard"))
                                .secondary()
                                .w_full()
                                .on_click(move |_, window, cx| {
                                    match &target {
                                        Some(target) => crate::core::revert_target(target, cx),
                                        None => crate::core::revert_tune(cx),
                                    }
                                    window.close_dialog(cx);
                                    discard
                                        .update(cx, |workspace, cx| {
                                            workspace.close_panel(panel, window, cx);
                                        })
                                        .ok();
                                }),
                        )
                        .child(
                            Button::new("close-unsaved-cancel")
                                .label(t!("app.close_unsaved.cancel"))
                                .ghost()
                                .w_full()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                )
        });

        false
    }

    /// Closes one document tab after its confirmation answered.
    ///
    /// The window is the one the dialog button was clicked in, so the dock edit
    /// runs inside it rather than round-tripping through the stored handle.
    fn close_panel(&mut self, panel: PanelId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(document) = self
            .editors
            .iter()
            .find(|document| PanelId::from(document.panel.entity_id()) == panel)
        else {
            return;
        };

        let document = document.panel.clone();
        let area = self.area.clone();

        area.update(cx, |area, cx| area.remove_panel(document, window, cx));
    }

    /// Reports the document behind `panel` as the one in front.
    ///
    /// The dock only announces a tab change within one group, and a split has a
    /// displayed tab in every group, so picking the tab or the body of the
    /// document a group already shows raises nothing. Both report the pick
    /// instead of waiting for the dock to notice a change.
    pub(super) fn follow_panel(&mut self, panel: PanelId, cx: &mut Context<'_, Self>) {
        let node = self
            .editors
            .iter()
            .find(|document| PanelId::from(document.panel.entity_id()) == panel)
            .map(|document| document.panel.read(cx).node_id().to_owned());

        cx.emit(ActiveDocument(node));
    }

    /// Drops tabs the reader closed.
    ///
    /// The dock removes a closed panel's handle, so membership there is the
    /// only honest answer. Dropping the entity drops its editors and its
    /// listener too, which is why this runs when the dock reports a close
    /// rather than when the next tab opens: a closed document must stop
    /// reading the core.
    fn prune(&mut self, cx: &mut Context<'_, Self>) {
        let area = self.area.clone();
        let held = |panel: PanelId| {
            area.read(cx)
                .layout(DockPlacement::Center)
                .is_some_and(|tree| tree.contains_panel(panel))
        };

        self.editors
            .retain(|document| held(PanelId::from(document.panel.entity_id())));

        debug_assert!(
            held(PanelId::from(self.realtime.entity_id())),
            "the charts tab is pinned and never closes"
        );
    }

    /// Adds a tab and moves it to `ix` in the pinned tabs' group.
    ///
    /// `add_panel_view` appends to the first tab group, which would put a new
    /// document at the far end of the strip. The follow-up move places it, so
    /// documents keep the order they were opened in and the pinned tabs stay
    /// first. An `ix` past the group's length is clamped, so a reader who
    /// dragged the tabs elsewhere still gets the new one beside them.
    fn add_tab(&self, handle: Arc<dyn BasePanelView>, ix: usize, cx: &mut Context<'_, Self>) {
        let id = handle.panel_id(cx);
        let area = self.area.clone();
        let pinned = PanelId::from(self.cluster.entity_id());
        let window = self.window;

        let _ = window.update(cx, |_, window, cx| {
            area.update(cx, |area, cx| {
                area.add_panel_view(handle, DockPlacement::Center, None, window, cx);

                let Some(node) = area
                    .layout(DockPlacement::Center)
                    .and_then(|tree| tree.find_panel_node(pinned))
                else {
                    return;
                };

                area.move_panel(
                    id,
                    InsertTarget::Tabs {
                        node,
                        ix: Some(ix),
                        activate: true,
                    },
                    window,
                    cx,
                );
            });
        });
    }

    /// Displays a panel in the tab group that already holds it.
    fn reveal(&self, panel: EntityId, cx: &mut Context<'_, Self>) {
        let area = self.area.clone();
        let id = PanelId::from(panel);
        let window = self.window;

        let _ = window.update(cx, |_, window, cx| {
            area.update(cx, |area, cx| area.select_panel(id, window, cx));
        });
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let read_only = CoreService::status(cx)
            .project
            .as_ref()
            .filter(|project| project.read_only)
            .and_then(|project| project.read_only_reason.as_deref())
            .map(|reason| t!("app.editor.read_only", reason = reason));
        let area = self.area.clone();

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // A read-only tune is a property of the workspace, not of the tab
            // in front, so it stays visible whichever tab is showing.
            .when_some(read_only, |this, reason| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .px_4()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(reason),
                )
            })
            .child(div().flex_1().min_h_0().child(area))
    }
}

/// Repeats a panel's activation so the window can follow the front document.
fn relay(event: &ActiveDocument, cx: &mut Context<'_, Workspace>) {
    cx.emit(event.clone());
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "UI tests may unwrap; a failed frame is a failed test"
    )]

    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, App, Bounds, Context, Entity, IntoElement, Point, Render, TestAppContext,
        Window, WindowBounds, WindowOptions, div, px, size,
    };
    use oxtuner_core::{MenuNode, MenuNodeKind};
    use oxtuner_ui::dock::{BasePanel as _, PaneRef};

    use super::*;

    struct Host {
        workspace: Entity<Workspace>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
            div().size_full().child(self.workspace.clone())
        }
    }

    fn node(id: &str, label: &str) -> MenuNode {
        MenuNode {
            id: id.to_owned(),
            label: label.to_owned(),
            kind: MenuNodeKind::Table,
            detail: None,
            target: Some(id.to_owned()),
            embedded: None,
            children: Vec::new(),
        }
    }

    /// Every document the workspace reported as in front.
    type Seen = Rc<RefCell<Vec<Option<String>>>>;

    /// Opens the workspace and records every document it reports as in front.
    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>, Seen) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            CoreService::install_without_core(cx);

            let seen: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
            let sink = Rc::clone(&seen);
            let opened: Rc<RefCell<Option<Entity<Workspace>>>> = Rc::default();
            let slot = Rc::clone(&opened);

            let (handle, _) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(1200.), px(800.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    let workspace = cx.new(|cx| Workspace::new(window, cx));
                    slot.replace(Some(workspace.clone()));

                    cx.subscribe(&workspace, move |_, event: &ActiveDocument, _| {
                        sink.borrow_mut().push(event.0.clone());
                    })
                    .detach();

                    cx.new(|_| Host {
                        workspace: workspace.clone(),
                    })
                },
            )
            .expect("open test window");

            let workspace = opened
                .borrow()
                .clone()
                .expect("the window built the workspace");
            (handle, workspace, seen)
        })
    }

    /// The panels of the center region's first tab group, in tab order.
    fn tabs(workspace: &Entity<Workspace>, cx: &App) -> Vec<PanelId> {
        let area = workspace.read(cx).area.clone();
        let area = area.read(cx);
        let tree = area
            .layout(DockPlacement::Center)
            .expect("the center region is installed");

        let mut panels = Vec::new();
        tree.root().walk(&mut |node| {
            if let PaneRef::Tabs { panels: held, .. } = node.kind() {
                panels = held.to_vec();
            }
        });
        panels
    }

    fn pinned(workspace: &Entity<Workspace>, cx: &App) -> Vec<PanelId> {
        let workspace = workspace.read(cx);
        vec![
            PanelId::from(workspace.cluster.entity_id()),
            PanelId::from(workspace.realtime.entity_id()),
        ]
    }

    #[gpui_kit::test]
    fn the_pinned_tabs_are_first_and_cannot_be_closed(cx: &mut TestAppContext) {
        let (handle, workspace, _) = open(cx);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();

        assert_eq!(
            cx.update(|cx| tabs(&workspace, cx)),
            cx.update(|cx| pinned(&workspace, cx))
        );

        assert!(
            !cx.update(|cx| workspace.read(cx).cluster.read(cx).closable(cx)),
            "the cluster is not a document and must not offer a close command"
        );
        assert!(
            !cx.update(|cx| workspace.read(cx).realtime.read(cx).closable(cx)),
            "the charts are not a document and must not offer a close command"
        );
    }

    #[gpui_kit::test]
    fn the_tab_strip_draws_one_tab_per_panel(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(CoreService::install_without_core);

        let (_, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(window, cx));
            Host { workspace }
        });

        cx.run_until_parked();

        assert!(
            cx.debug_bounds("oxtuner-tab-0").is_some(),
            "the workspace draws a folder tab strip"
        );
        assert!(
            cx.debug_bounds("oxtuner-tab-1").is_some(),
            "the charts are the second pinned tab"
        );
    }

    #[gpui_kit::test]
    fn a_modified_document_shows_a_dot_instead_of_its_close_button(cx: &mut TestAppContext) {
        let (handle, workspace, _) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });

            // The tab asks the core; a view test can plant the answer.
            CoreService::mark_dirty(cx, "menu:0/0");
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();

        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.update(gpui_kit::test::TestWindowExt::render_frame);

        assert!(
            visual.debug_bounds("oxtuner-tab-dirty").is_some(),
            "an unsaved document marks its tab"
        );
    }

    #[gpui_kit::test]
    fn closing_an_unsaved_document_asks_first(cx: &mut TestAppContext) {
        let (handle, workspace, _) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });
            CoreService::mark_dirty(cx, "menu:0/0");
        });

        let panel = cx.update(|cx| PanelId::from(workspace.read(cx).editors[0].panel.entity_id()));
        let proceed = cx
            .update_window(handle, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.confirm_close(panel, window, cx)
                })
            })
            .unwrap();

        assert!(!proceed, "unsaved work must not be dropped unasked");
        assert!(
            cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
                .unwrap(),
            "the confirmation is the answer to a close the dock asked for"
        );
        assert_eq!(
            cx.update(|cx| workspace.read(cx).editors.len()),
            1,
            "the document stays open until the dialog is answered"
        );
    }

    #[gpui_kit::test]
    fn closing_a_clean_document_does_not_ask(cx: &mut TestAppContext) {
        let (handle, workspace, _) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });
        });

        let panel = cx.update(|cx| PanelId::from(workspace.read(cx).editors[0].panel.entity_id()));
        let proceed = cx
            .update_window(handle, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.confirm_close(panel, window, cx)
                })
            })
            .unwrap();

        assert!(proceed, "a tab with nothing to lose closes at once");
    }

    #[gpui_kit::test]
    fn a_reopened_document_keeps_its_unsaved_mark(cx: &mut TestAppContext) {
        let (handle, workspace, _) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });
            CoreService::mark_dirty(cx, "menu:0/0");
        });

        let panel = cx.update(|cx| workspace.read(cx).editors[0].panel.clone());
        cx.update_window(handle, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                let area = workspace.area.clone();
                area.update(cx, |area, cx| area.remove_panel(panel, window, cx));
            });
        })
        .unwrap();

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();

        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.update(gpui_kit::test::TestWindowExt::render_frame);

        assert!(
            visual.debug_bounds("oxtuner-tab-dirty").is_some(),
            "reopening a document must restore its unsaved mark"
        );
    }

    #[gpui_kit::test]
    fn a_strip_wider_than_the_window_gets_a_scroll_rail(cx: &mut TestAppContext) {
        let (handle, workspace, _) = open(cx);

        let nodes: Vec<MenuNode> = (0..40)
            .map(|ix| node(&format!("menu:0/{ix}"), &format!("Table Number {ix}")))
            .collect();
        cx.update(|cx| {
            for node in nodes {
                workspace.update(cx, |workspace, cx| workspace.open_node(node, cx));
            }
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();

        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        for _ in 0..4 {
            visual.run_until_parked();
            visual.update(gpui_kit::test::TestWindowExt::render_frame);
        }

        assert!(
            visual.debug_bounds("oxtuner-tab-rail").is_some(),
            "a strip wider than the window must show its scroll rail"
        );
    }

    #[gpui_kit::test]
    fn a_document_opens_directly_after_the_pinned_tabs(cx: &mut TestAppContext) {
        let (handle, workspace, _) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();

        let mut expected = cx.update(|cx| pinned(&workspace, cx));
        expected.push(cx.update(|cx| {
            workspace
                .read(cx)
                .editors
                .first()
                .map(|document| PanelId::from(document.panel.entity_id()))
                .expect("the document opened a tab")
        }));

        assert_eq!(cx.update(|cx| tabs(&workspace, cx)), expected);
    }

    #[gpui_kit::test]
    fn documents_keep_the_order_they_were_opened_in(cx: &mut TestAppContext) {
        let (_, workspace, _) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
                workspace.open_node(node("menu:0/1", "Spark Table"), cx);
            });
        });

        let mut expected = cx.update(|cx| pinned(&workspace, cx));
        expected.extend(cx.update(|cx| {
            workspace
                .read(cx)
                .editors
                .iter()
                .map(|document| PanelId::from(document.panel.entity_id()))
                .collect::<Vec<_>>()
        }));

        assert_eq!(cx.update(|cx| tabs(&workspace, cx)), expected);
    }

    #[gpui_kit::test]
    fn closing_a_document_drops_its_editor(cx: &mut TestAppContext) {
        let (handle, workspace, _) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });
        });
        let panel = cx.update(|cx| workspace.read(cx).editors[0].panel.clone());

        cx.update_window(handle, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                let area = workspace.area.clone();
                area.update(cx, |area, cx| area.remove_panel(panel, window, cx));
            });
        })
        .unwrap();

        assert_eq!(
            cx.update(|cx| workspace.read(cx).editors.len()),
            0,
            "a closed document must not keep reading the core"
        );
        assert_eq!(
            cx.update(|cx| tabs(&workspace, cx)),
            cx.update(|cx| pinned(&workspace, cx))
        );
    }

    #[gpui_kit::test]
    fn a_split_reports_the_tab_that_was_picked(cx: &mut TestAppContext) {
        let (handle, workspace, seen) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
                workspace.open_node(node("menu:0/1", "Spark Table"), cx);
            });
        });

        // Put the second document in a group of its own, the way a drag to the
        // right edge would. Each group now displays a document at the same
        // time, which is the case the dock cannot report on its own.
        let second = cx
            .update_window(handle, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let second = workspace.editors[1].panel.entity_id();
                    let area = workspace.area.clone();
                    let pin = PanelId::from(workspace.cluster.entity_id());

                    area.update(cx, |area, cx| {
                        let Some(group) = area
                            .layout(DockPlacement::Center)
                            .and_then(|tree| tree.find_panel_node(pin))
                        else {
                            return;
                        };

                        area.move_panel(
                            PanelId::from(second),
                            InsertTarget::Split {
                                node: group,
                                placement: gpui_kit::base::Placement::Right,
                                size: None,
                            },
                            window,
                            cx,
                        );
                    });

                    second
                })
            })
            .unwrap();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();

        // Picking a tab that its group already displays raises nothing in the
        // dock, so the strip reports the pick itself.
        seen.borrow_mut().clear();
        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.follow_panel(PanelId::from(second), cx);
            });
        });

        assert_eq!(
            seen.borrow().last().cloned(),
            Some(Some("menu:0/1".to_owned())),
            "a split has a displayed document per group, so the pick decides"
        );
    }

    #[gpui_kit::test]
    fn opening_the_same_node_twice_keeps_one_tab(cx: &mut TestAppContext) {
        let (_, workspace, _) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });
        });

        assert_eq!(cx.update(|cx| workspace.read(cx).editors.len()), 1);
        assert_eq!(cx.update(|cx| tabs(&workspace, cx)).len(), PINNED_TABS + 1);
    }

    #[gpui_kit::test]
    fn a_document_tells_the_window_which_node_is_in_front(cx: &mut TestAppContext) {
        let (handle, workspace, seen) = open(cx);

        cx.update(|cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_node(node("menu:0/0", "VE Table"), cx);
            });
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
        // The dock announces activation from a task queued for the end of the
        // frame, so the test has to let that task run.
        cx.run_until_parked();

        assert!(
            seen.borrow()
                .iter()
                .any(|node| node.as_deref() == Some("menu:0/0")),
            "the dock must report the document it raised"
        );
    }
}
