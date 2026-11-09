//! The primary sidebar: the definition's menu, as the navigation column.
//!
//! It starts **below** the main bar — the bar spans the whole window and carries
//! the traffic lights, the menus and the panel toggles — so this column draws no
//! title strip of its own. The panel toggle in the bar is the one control it
//! needs.
//!
//! [`explorer`] holds the menu tree and the selection, [`menu_node`] names and
//! pictures one node, and this module lays the rows out. The three live
//! together because the tree has no other home: a row click is a sidebar
//! gesture, and the document it opens is the workspace's business.
//!
//! A definition that declares no `[Menu]` sections says so instead of showing an
//! empty tree. Project handling (current project, recent folders, choosing
//! another folder) lives in the Projects dialog, not here: the sidebar is for
//! navigation.

mod explorer;
pub(crate) mod menu_node;

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::tree::{TreeEntry, TreeState, tree};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use oxtuner_core::{MenuNode, MenuNodeKind};

pub(crate) use self::explorer::Explorer;
use crate::{core::CoreService, i18n::t};

/// The navigation column.
#[derive(IntoElement)]
pub(crate) struct PrimarySidebar {
    explorer: Entity<Explorer>,
}

impl PrimarySidebar {
    pub(crate) fn new(explorer: Entity<Explorer>) -> Self {
        Self { explorer }
    }
}

impl RenderOnce for PrimarySidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let status = CoreService::status(cx);
        let has_definition = status.definition().is_some();
        let pending = status.pending_project.is_some();

        let explorer = self.explorer.read(cx);
        let state = explorer.tree().clone();
        let nodes = explorer.nodes();
        let has_menu = explorer.has_menu();

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .pt_2()
            .child(section_label(t!("app.primary_sidebar.navigation"), cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .pt_1()
                    .when(!has_definition && pending, |this| {
                        this.child(empty_state(t!("app.editor.opening_project"), cx))
                    })
                    .when(!has_definition && !pending, |this| {
                        this.child(empty_state(t!("app.primary_sidebar.no_definition"), cx))
                    })
                    .when(has_definition && !has_menu, |this| {
                        this.child(empty_state(t!("app.primary_sidebar.no_menu"), cx))
                    })
                    .when(has_menu, |this| {
                        this.child(navigation_tree(&state, nodes, self.explorer.clone()))
                    }),
            )
    }
}

/// The scrolling tree, spanning the column so its scrollbar sits at the edge.
///
/// The rows carry their own horizontal inset. Padding here would inset the
/// scrollbar too, because the component paints it inside the scroll area's
/// bounds, which is the difference between this and the editor's scrollbars.
fn navigation_tree(
    state: &Entity<TreeState>,
    nodes: Rc<HashMap<String, MenuNode>>,
    explorer: Entity<Explorer>,
) -> impl IntoElement {
    div()
        .debug_selector(|| "primary-sidebar-tree-area".to_string())
        .size_full()
        .min_h_0()
        .child(tree(state, move |_ix, entry, selected, _window, cx| {
            tree_row(entry, selected, &nodes, &explorer, cx)
        }))
}

/// Whether `node` or anything under it has a target that differs from the
/// file baseline.
///
/// A folder has no target of its own, so it lights up when one of the documents
/// inside it does, the way a source-control tree rolls status up.
fn touched(node: &MenuNode, dirty: &BTreeSet<String>) -> bool {
    if node.children.is_empty() {
        return node
            .target
            .as_deref()
            .is_some_and(|target| dirty.contains(target))
            || node
                .embedded
                .as_deref()
                .is_some_and(|target| dirty.contains(target));
    }

    node.children.iter().any(|child| touched(child, dirty))
}

/// The unsaved mark on a navigation row.
fn unsaved_dot(cx: &App) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .size(px(7.))
        .rounded_full()
        .bg(cx.theme().warning)
}

fn section_label(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .px_3()
        .pt_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

fn empty_state(text: SharedString, cx: &App) -> impl IntoElement {
    div()
        .px_3()
        .py_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// One tree row: icon, label, and the size/count when the definition has one.
///
/// Folders draw an open or closed folder rather than the kind's icon, because
/// expansion is the state a folder has and the icon is where it is visible.
/// The row is keyed by the node id rather than its index, so expanding a folder
/// (which renumbers the rows) does not move an element's state to another row.
///
/// A folder row is its own disclosure control. The tree component disables
/// folder entries so a click cannot select them; this row's click toggles the
/// folder and leaves the selected leaf alone.
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "tree depth is a small row indent converted to pixels"
)]
fn tree_row(
    entry: &TreeEntry,
    selected: bool,
    nodes: &HashMap<String, MenuNode>,
    explorer: &Entity<Explorer>,
    cx: &App,
) -> ListItem {
    let node = nodes.get(entry.item().id.as_ref());
    let is_folder = entry.is_folder();
    let icon = if is_folder {
        if entry.is_expanded() {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        }
    } else {
        node.map_or(IconName::File, |node| menu_node::icon(node.kind))
    };
    let missing = node.is_some_and(|node| node.kind == MenuNodeKind::Missing);
    let unavailable = node.is_some_and(|node| !node.gates.visible || !node.gates.enabled);
    let touched = node.is_some_and(|node| touched(node, CoreService::dirty(cx)));
    let muted = cx.theme().muted_foreground;
    // A folder's icon is its disclosure state, not a kind, so only leaves take
    // the kind's colour.
    let icon_color = match node {
        Some(node) if !is_folder && !unavailable => menu_node::icon_color(node.kind, cx),
        _ => muted,
    };
    let unavailable_reason =
        unavailable.then(|| t!("app.primary_sidebar.unavailable_for_settings").to_string());

    let row_id: ElementId = SharedString::from(format!("tree-row-{}", entry.item().id)).into();
    let mut surface = h_flex()
        .id(row_id)
        .w_full()
        .h_7()
        .min_w_0()
        .items_center()
        .gap_2()
        .rounded(cx.theme().radius)
        .pl_2()
        .pr_2()
        .text_xs()
        .text_color(cx.theme().sidebar_foreground)
        .when(selected, |this| {
            this.bg(cx.theme().tokens.sidebar_accent)
                .text_color(cx.theme().sidebar_accent_foreground)
                .font_weight(FontWeight::MEDIUM)
        })
        .when(!selected && !unavailable, |this| {
            this.hover(|this| {
                this.bg(cx.theme().sidebar_accent.opacity(0.8))
                    .text_color(cx.theme().sidebar_accent_foreground)
            })
        })
        .child(Icon::new(icon).small().text_color(icon_color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(is_folder, |this| this.font_weight(FontWeight::SEMIBOLD))
                .when(missing || unavailable, |this| this.text_color(muted))
                .child(entry.item().label.clone()),
        )
        .when(touched, |this| this.child(unsaved_dot(cx)));

    if let Some(reason) = unavailable_reason {
        surface = surface.tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx));
    }

    if is_folder {
        let explorer = explorer.clone();
        let id = entry.item().id.to_string();
        surface = surface.on_mouse_down(gpui_kit::MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            explorer.update(cx, |explorer, cx| explorer.toggle_expanded(&id, cx));
        });
    } else if !unavailable {
        let explorer = explorer.clone();
        let id = entry.item().id.to_string();
        surface = surface.on_mouse_down(gpui_kit::MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            explorer.update(cx, |explorer, cx| explorer.select_node(&id, cx));
        });
    }

    ListItem::new(entry.item().id.clone())
        .px_0()
        .py_0p5()
        .accessibility_label(entry.item().label.clone())
        .child(
            div()
                .w_full()
                .pl(px(8.0 + entry.depth() as f32 * 14.0))
                .pr_3()
                .child(surface),
        )
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "UI tests may unwrap; a failed frame is a failed test"
    )]

    use super::*;
    use gpui_kit::Point;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement, Render,
        TestAppContext, Window, WindowBounds, WindowOptions, div, px, size,
    };
    use oxtuner_core::GateState;
    use oxtuner_core::{MenuNode, MenuNodeKind};

    struct Host {
        explorer: Entity<Explorer>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            let explorer = self.explorer.read(cx);
            let state = explorer.tree().clone();
            let nodes = explorer.nodes();
            let entity = self.explorer.clone();
            div()
                .size_full()
                .child(tree(&state, move |_ix, entry, selected, _window, cx| {
                    tree_row(entry, selected, &nodes, &entity, cx)
                }))
        }
    }

    fn menu() -> Vec<MenuNode> {
        vec![MenuNode {
            id: "menu:0".to_owned(),
            label: "Tuning".to_owned(),
            kind: MenuNodeKind::Menu,
            detail: None,
            target: None,
            embedded: None,
            enable_expression: None,
            visibility_expression: None,
            gates: GateState::default(),
            children: vec![MenuNode {
                id: "menu:0/0".to_owned(),
                label: "VE Table".to_owned(),
                kind: MenuNodeKind::Table,
                detail: None,
                target: Some("veTableTbl".to_owned()),
                embedded: None,
                enable_expression: None,
                visibility_expression: None,
                gates: GateState::default(),
                children: Vec::new(),
            }],
        }]
    }

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Explorer>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            // Rows read the core's unsaved set; a view test needs the global.
            crate::core::CoreService::install_without_core(cx);

            let explorer = cx.new(|cx| Explorer::for_test(menu(), cx));
            let host = cx.new(|_| Host {
                explorer: explorer.clone(),
            });
            let (handle, _) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(420.), px(600.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |_, _| host,
            )
            .expect("open test window");
            (handle, explorer)
        })
    }

    fn is_expanded(explorer: &Entity<Explorer>, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            explorer
                .read(cx)
                .tree()
                .read(cx)
                .entry(0)
                .is_some_and(TreeEntry::is_expanded)
        })
    }

    #[test]
    fn a_folder_is_touched_through_its_documents() {
        let table = MenuNode {
            id: "menu:0/0".to_owned(),
            label: "VE Table".to_owned(),
            kind: MenuNodeKind::Table,
            detail: None,
            target: Some("veTableTbl".to_owned()),
            embedded: None,
            enable_expression: None,
            visibility_expression: None,
            gates: GateState::default(),
            children: Vec::new(),
        };
        let folder = MenuNode {
            id: "menu:0".to_owned(),
            label: "Tuning".to_owned(),
            kind: MenuNodeKind::Menu,
            detail: None,
            target: None,
            embedded: None,
            enable_expression: None,
            visibility_expression: None,
            gates: GateState::default(),
            children: vec![table.clone()],
        };
        let dialog = MenuNode {
            id: "menu:0/1".to_owned(),
            label: "Idle".to_owned(),
            kind: MenuNodeKind::Dialog,
            detail: None,
            target: Some("idleDialog".to_owned()),
            embedded: Some("veTableTbl".to_owned()),
            enable_expression: None,
            visibility_expression: None,
            gates: GateState::default(),
            children: Vec::new(),
        };

        let mut dirty = BTreeSet::new();
        dirty.insert("veTableTbl".to_owned());

        assert!(touched(&table, &dirty), "the table itself is marked");
        assert!(touched(&dialog, &dirty), "a wrapped table counts too");
        assert!(
            touched(&folder, &dirty),
            "a folder lights up through its documents"
        );
        assert!(!touched(&dialog, &BTreeSet::new()));
    }

    #[gpui_kit::test]
    fn clicking_a_folder_only_expands_it(cx: &mut TestAppContext) {
        let (handle, explorer) = open(cx);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(ElementId::from("menu:0"), cx);
        })
        .unwrap();

        assert!(
            cx.update(|cx| explorer.read(cx).selected().is_none()),
            "a folder click must not select the folder"
        );
        assert!(is_expanded(&explorer, cx));
    }

    #[gpui_kit::test]
    fn clicking_a_leaf_selects_it(cx: &mut TestAppContext) {
        let (handle, explorer) = open(cx);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(ElementId::from(0usize), cx);
            window.render_frame(cx);
            window.click(ElementId::from(1usize), cx);
        })
        .unwrap();

        let selected = cx.update(|cx| explorer.read(cx).selected().cloned());
        assert_eq!(selected.map(|node| node.label), Some("VE Table".to_owned()));
    }

    #[gpui_kit::test]
    fn collapsing_a_folder_keeps_the_open_leaf(cx: &mut TestAppContext) {
        let (handle, explorer) = open(cx);

        for id in [0usize, 1, 0] {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.click(ElementId::from(id), cx);
            })
            .unwrap();
        }

        assert!(!is_expanded(&explorer, cx));
        let selected = cx.update(|cx| explorer.read(cx).selected().cloned());
        assert_eq!(
            selected.map(|node| node.label),
            Some("VE Table".to_owned()),
            "collapsing the parent must not close the table"
        );
    }

    struct TreeAreaHost {
        explorer: Entity<Explorer>,
    }

    impl Render for TreeAreaHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            let explorer = self.explorer.read(cx);
            let state = explorer.tree().clone();
            let nodes = explorer.nodes();
            // Mirrors the sidebar's parent: a plain block container with a
            // definite height, not a flex column.
            div()
                .size_full()
                .child(navigation_tree(&state, nodes, self.explorer.clone()))
        }
    }

    /// The selector gpui-component puts on a scroll area's bar layer
    /// (`render_scrollbar`). The bar is the only way to measure its edge
    /// without owning the scroll area, so a rename shows up here first.
    const SCROLLBAR_LAYER: &str = "scrollbar-overlay";

    #[gpui_kit::test]
    fn the_scroll_area_reaches_the_column_edge(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let leaves: Vec<MenuNode> = (0..40)
            .map(|ix| MenuNode {
                id: format!("menu:0/{ix}"),
                label: format!("Table {ix}"),
                kind: MenuNodeKind::Table,
                detail: None,
                target: Some(format!("table{ix}")),
                embedded: None,
                enable_expression: None,
                visibility_expression: None,
                gates: GateState::default(),
                children: Vec::new(),
            })
            .collect();

        let (handle, _) = cx.update(|cx| {
            crate::core::CoreService::install_without_core(cx);
            let explorer = cx.new(|cx| Explorer::for_test(leaves, cx));
            let host = cx.new(|_| TreeAreaHost { explorer });
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(320.), px(400.)),
                    ))),
                    ..Default::default()
                },
                cx,
                move |_, _| host,
            )
            .expect("open test window")
        });

        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.update(gpui_kit::test::TestWindowExt::render_frame);

        let area = visual
            .debug_bounds("primary-sidebar-tree-area")
            .expect("the tree area is drawn");
        let bar = visual.debug_bounds(SCROLLBAR_LAYER).expect(
            "the tree draws its scrollbar layer; if gpui-kit renamed that layer, \
                 update SCROLLBAR_LAYER and the tree finding",
        );

        assert!(
            (area.right().as_f32() - 320.0).abs() < 1.0,
            "the tree must span the column, scrollbar included: {}",
            area.right().as_f32()
        );
        assert!(
            (area.bottom().as_f32() - 400.0).abs() < 1.0,
            "the tree must fill the column's height: {}",
            area.bottom().as_f32()
        );
        assert!(
            (bar.right().as_f32() - area.right().as_f32()).abs() < 1.0,
            "the scrollbar layer must sit at the tree's right edge"
        );
    }
}
