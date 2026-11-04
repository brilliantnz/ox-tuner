//! The primary sidebar: navigation.
//!
//! It starts **below** the main bar — the bar spans the whole window and carries
//! the traffic lights, the menus and the panel toggles — so this column draws no
//! title strip of its own. The panel toggle in the bar is the one control it
//! needs.
//!
//! The navigation tree is built by [`super::explorer`] from the definition the
//! core owns; this view only lays it out and decorates its rows. A definition
//! that declares no `[Menu]` sections says so instead of showing an empty tree.
//!
//! Project handling (current project, recent folders, choosing another folder)
//! lives in the Projects dialog, not here: the sidebar is for navigation.

use std::collections::HashMap;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEntry, tree};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div, px,
};
use oxtuner_core::{MenuNode, MenuNodeKind};

use super::explorer::{self, Explorer};
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
                    .px_2()
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
                        let explorer = self.explorer.clone();
                        this.child(tree(&state, move |_ix, entry, selected, _window, cx| {
                            tree_row(entry, selected, &nodes, &explorer, cx)
                        }))
                    }),
            )
    }
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
        node.map_or(IconName::File, |node| explorer::icon(node.kind))
    };
    let missing = node.is_some_and(|node| node.kind == MenuNodeKind::Missing);
    let detail = node.and_then(explorer::detail_label);
    let muted = cx.theme().muted_foreground;

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
        .when(!selected, |this| {
            this.hover(|this| {
                this.bg(cx.theme().sidebar_accent.opacity(0.8))
                    .text_color(cx.theme().sidebar_accent_foreground)
            })
        })
        .child(Icon::new(icon).small().text_color(muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(is_folder, |this| this.font_weight(FontWeight::SEMIBOLD))
                .when(missing, |this| this.text_color(muted))
                .child(entry.item().label.clone()),
        )
        .when_some(detail, |this, detail| {
            this.child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(detail),
            )
        });

    if is_folder {
        let explorer = explorer.clone();
        let id = entry.item().id.to_string();
        surface = surface.on_mouse_down(gpui_kit::MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            explorer.update(cx, |explorer, cx| explorer.toggle_expanded(&id, cx));
        });
    } else {
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
                .pl(px(entry.depth() as f32 * 14.0))
                .pr_1()
                .child(surface),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement, Render,
        TestAppContext, Window, WindowBounds, WindowOptions, div, px, size,
    };
    use oxtuner_core::{MenuNode, MenuNodeKind};

    struct Host {
        explorer: Entity<Explorer>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            children: vec![MenuNode {
                id: "menu:0/0".to_owned(),
                label: "VE Table".to_owned(),
                kind: MenuNodeKind::Table,
                detail: None,
                target: Some("veTableTbl".to_owned()),
                embedded: None,
                children: Vec::new(),
            }],
        }]
    }

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Explorer>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let explorer = cx.new(|cx| Explorer::for_test(menu(), cx));
            let host = cx.new(|_| Host {
                explorer: explorer.clone(),
            });
            let (handle, _) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
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
                .is_some_and(|entry| entry.is_expanded())
        })
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
}
