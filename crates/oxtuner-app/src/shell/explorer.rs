//! The definition-driven navigation tree in the primary sidebar.
//!
//! The core publishes the menu tree with every definition ([`MenuNode`]); this
//! entity turns it into gpui-kit's [`TreeState`] and keeps the selection. It
//! owns no copy of the definition: a core status change is the only thing that
//! rebuilds the tree, and an unchanged menu is left alone so expansion state
//! survives unrelated events (a save, a failure message).
//!
//! Selection is not a `TreeEvent`: gpui-base selects on a click and notifies
//! its own entity, so the tree is observed and [`TreeState::selected_item`] is
//! read after the fact.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::assets::IconName;
use gpui_kit::component::tree::{TreeItem, TreeState};
use gpui_kit::{App, AppContext as _, Context, Entity, SharedString, Subscription};
use oxtuner_core::{MenuDetail, MenuNode, MenuNodeKind};

use crate::{core::CoreService, i18n::t};

/// The navigation tree and what the user picked in it.
pub(crate) struct Explorer {
    tree: Entity<TreeState>,
    /// The menu the tree was built from; compared to skip no-op rebuilds.
    menu: Vec<MenuNode>,
    /// Every node by id, for row decoration and selection lookup.
    nodes: Rc<HashMap<String, MenuNode>>,
    /// The selected node, cloned so the editor can render it without holding a
    /// borrow on this entity.
    selected: Option<MenuNode>,
    /// Keeps the core observer alive.
    _core: Subscription,
    /// Keeps the selection observer alive.
    _selection: Subscription,
}

impl Explorer {
    /// Builds the tree from the definition the core already owns, if any.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let menu = current_menu(cx);
        let (items, nodes) = build(&menu);
        let tree = cx.new(|cx| TreeState::new(cx).items(items));

        // A click selects and expands; the tree notifies, and the selection is
        // read back here.
        let selection = cx.observe(&tree, |this, tree, cx| this.follow_selection(&tree, cx));

        // `global_mut` pushes a notify effect, so this runs after the status
        // has the new definition.
        let core = cx.observe_global::<CoreService>(|this, cx| this.sync(cx));

        Self {
            tree,
            menu,
            nodes: Rc::new(nodes),
            selected: None,
            _core: core,
            _selection: selection,
        }
    }

    /// The tree state the sidebar renders.
    pub(crate) fn tree(&self) -> &Entity<TreeState> {
        &self.tree
    }

    /// Every node by id, for row icons and details.
    pub(crate) fn nodes(&self) -> Rc<HashMap<String, MenuNode>> {
        self.nodes.clone()
    }

    /// The node the user selected, if any.
    pub(crate) fn selected(&self) -> Option<&MenuNode> {
        self.selected.as_ref()
    }

    /// Whether the definition declares any menus at all.
    pub(crate) fn has_menu(&self) -> bool {
        !self.menu.is_empty()
    }

    /// Rebuilds the tree when the core publishes a different menu.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let menu = current_menu(cx);
        if menu == self.menu {
            return;
        }

        let (items, nodes) = build(&menu);
        self.menu = menu;
        self.nodes = Rc::new(nodes);
        self.selected = None;
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
        cx.notify();
    }

    /// Mirrors the tree's selection into the node the editor shows.
    fn follow_selection(&mut self, tree: &Entity<TreeState>, cx: &mut Context<Self>) {
        let selected = tree
            .read(cx)
            .selected_item()
            .and_then(|item| self.nodes.get(item.id.as_ref()).cloned());
        if selected == self.selected {
            return;
        }

        self.selected = selected;
        cx.notify();
    }
}

/// The menu the core currently publishes.
fn current_menu(cx: &App) -> Vec<MenuNode> {
    CoreService::status(cx)
        .definition()
        .map(|definition| definition.menu.clone())
        .unwrap_or_default()
}

/// The tree items, plus every node by id.
fn build(menu: &[MenuNode]) -> (Vec<TreeItem>, HashMap<String, MenuNode>) {
    let mut nodes = HashMap::new();
    let items = menu
        .iter()
        .map(|node| tree_item(node, &mut nodes))
        .collect();
    (items, nodes)
}

fn tree_item(node: &MenuNode, nodes: &mut HashMap<String, MenuNode>) -> TreeItem {
    nodes.insert(node.id.clone(), node.clone());

    let item = TreeItem::new(node.id.clone(), node.label.clone());
    if node.children.is_empty() {
        item
    } else {
        item.children(node.children.iter().map(|child| tree_item(child, nodes)))
    }
}

/// The kind word shown next to a node.
pub(crate) fn kind_label(kind: MenuNodeKind) -> SharedString {
    match kind {
        MenuNodeKind::Menu => t!("app.explorer.kind_menu"),
        MenuNodeKind::Group => t!("app.explorer.kind_group"),
        MenuNodeKind::Table => t!("app.explorer.kind_table"),
        MenuNodeKind::Curve => t!("app.explorer.kind_curve"),
        MenuNodeKind::Dialog => t!("app.explorer.kind_dialog"),
        MenuNodeKind::Help => t!("app.explorer.kind_help"),
        MenuNodeKind::Standard => t!("app.explorer.kind_standard"),
        MenuNodeKind::Missing => t!("app.explorer.kind_missing"),
    }
}

/// The size or child count shown at the trailing edge of a row.
pub(crate) fn detail_label(node: &MenuNode) -> Option<SharedString> {
    if let Some(detail) = node.detail {
        return Some(match detail {
            MenuDetail::Table { cols, rows } => {
                t!("app.explorer.detail_table", cols = cols, rows = rows)
            }
            MenuDetail::Curve { points } => t!("app.explorer.detail_curve", points = points),
            MenuDetail::Dialog { rows } => t!("app.explorer.detail_dialog", rows = rows),
        });
    }

    (!node.children.is_empty()).then(|| {
        let count = node.children.len();
        if count == 1 {
            t!("app.explorer.one_item")
        } else {
            t!("app.explorer.items", count = count)
        }
    })
}

/// `Table · 16 × 16`, or just the kind when the definition gives no size.
pub(crate) fn summary(node: &MenuNode) -> SharedString {
    match detail_label(node) {
        Some(detail) => format!("{} · {detail}", kind_label(node.kind)).into(),
        None => kind_label(node.kind),
    }
}

/// The line under a selected node, saying what can happen next.
pub(crate) fn hint(kind: MenuNodeKind) -> SharedString {
    match kind {
        MenuNodeKind::Menu | MenuNodeKind::Group => t!("app.editor.folder_hint"),
        MenuNodeKind::Missing => t!("app.editor.missing_hint"),
        _ => t!("app.editor.unbuilt_hint"),
    }
}

/// The icon for a node that is not drawn as an open/closed folder.
///
/// Only names in gpui-kit's default bundle are used: `Assets` embeds that
/// subset, and a name outside it fails to load at render time.
pub(crate) fn icon(kind: MenuNodeKind) -> IconName {
    match kind {
        // A menu is a folder in the tree; the editor header shows the same.
        MenuNodeKind::Menu | MenuNodeKind::Group => IconName::Folder,
        MenuNodeKind::Table => IconName::LayoutDashboard,
        MenuNodeKind::Curve => IconName::ChartPie,
        MenuNodeKind::Dialog => IconName::Frame,
        MenuNodeKind::Help => IconName::BookOpen,
        MenuNodeKind::Standard => IconName::Settings2,
        MenuNodeKind::Missing => IconName::TriangleAlert,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtuner_core::MenuNodeKind;

    fn node(id: &str, label: &str, children: Vec<MenuNode>) -> MenuNode {
        MenuNode {
            id: id.to_owned(),
            label: label.to_owned(),
            kind: if children.is_empty() {
                MenuNodeKind::Table
            } else {
                MenuNodeKind::Menu
            },
            detail: None,
            children,
        }
    }

    #[test]
    fn every_node_is_indexed_and_the_tree_keeps_its_shape() {
        let menu = vec![node(
            "menu:0",
            "Tuning",
            vec![node("menu:0/0", "VE Table", Vec::new())],
        )];

        let (items, nodes) = build(&menu);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].children.len(), 1);
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes.get("menu:0/0").map(|node| node.label.as_str()),
            Some("VE Table")
        );
    }
}
