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

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState};
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
    /// Ids of the folders the user opened. The tree component's own state is
    /// authoritative while it runs; this copy rebuilds the same view when the
    /// menu is replaced or a folder is clicked.
    expanded: HashSet<String>,
    /// The selected node, cloned so the editor can render it without holding a
    /// borrow on this entity.
    selected: Option<MenuNode>,
    /// Keeps the core observer alive.
    _core: Option<Subscription>,
    /// Keeps the selection observer alive.
    _selection: Subscription,
    /// Keeps the expansion observer alive.
    _expansion: Subscription,
}

impl Explorer {
    /// Builds the tree from the definition the core already owns, if any.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let menu = current_menu(cx);
        let expanded = HashSet::new();
        let (items, nodes) = build(&menu, &expanded);
        let tree = cx.new(|cx| TreeState::new(cx).items(items));

        // A click selects and expands; the tree notifies, and the selection is
        // read back here.
        let selection = cx.observe(&tree, |this, tree, cx| this.follow_selection(&tree, cx));
        // Keyboard expansion is handled inside the tree. Mirror it so a later
        // menu rebuild keeps the folders the user opened.
        let expansion = cx.subscribe(&tree, |this, _, event, cx| this.on_tree_event(event, cx));

        // `global_mut` pushes a notify effect, so this runs after the status
        // has the new definition.
        let core = cx.observe_global::<CoreService>(|this, cx| this.sync(cx));

        Self {
            tree,
            menu,
            nodes: Rc::new(nodes),
            expanded,
            selected: None,
            _core: Some(core),
            _selection: selection,
            _expansion: expansion,
        }
    }

    /// Builds an explorer from a menu without touching the core, for UI tests.
    #[cfg(test)]
    pub(crate) fn for_test(menu: Vec<MenuNode>, cx: &mut Context<Self>) -> Self {
        let expanded = HashSet::new();
        let (items, nodes) = build(&menu, &expanded);
        let tree = cx.new(|cx| TreeState::new(cx).items(items));
        let selection = cx.observe(&tree, |this, tree, cx| this.follow_selection(&tree, cx));
        let expansion = cx.subscribe(&tree, |this, _, event, cx| this.on_tree_event(event, cx));

        Self {
            tree,
            menu,
            nodes: Rc::new(nodes),
            expanded,
            selected: None,
            _core: None,
            _selection: selection,
            _expansion: expansion,
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

        let (items, nodes) = build(&menu, &self.expanded);
        let ids: HashSet<String> = nodes.keys().cloned().collect();
        self.expanded.retain(|id| ids.contains(id));
        self.menu = menu;
        self.nodes = Rc::new(nodes);
        self.selected = None;
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
        cx.notify();
    }

    /// Opens or closes a folder without changing the selected leaf.
    ///
    /// Folder rows are disabled in the tree component, so its own click handler
    /// cannot select them; this is the one place their expansion changes.
    pub(crate) fn toggle_expanded(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_owned());
        }

        let selected_id = self.selected.as_ref().map(|node| node.id.clone());
        let (items, nodes) = build(&self.menu, &self.expanded);
        self.nodes = Rc::new(nodes);
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
            if let Some(id) = selected_id {
                let id = SharedString::from(id);
                let index = tree.index_of(&id);
                tree.set_selected_index(index, cx);
            }
        });
        cx.notify();
    }

    /// Opens a leaf from a row click. Folders are handled by
    /// [`Self::toggle_expanded`] instead.
    pub(crate) fn select_node(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(node) = self.nodes.get(id).cloned() else {
            return;
        };
        if !node.children.is_empty() {
            return;
        }

        self.selected = Some(node);
        let id = SharedString::from(id.to_owned());
        let index = self.tree.read(cx).index_of(&id);
        self.tree
            .update(cx, |tree, cx| tree.set_selected_index(index, cx));
        cx.notify();
    }

    /// Mirrors keyboard expansion from the tree into [`Self::expanded`].
    fn on_tree_event(&mut self, event: &TreeEvent, _: &mut Context<Self>) {
        match event {
            TreeEvent::Expanded(id) => {
                self.expanded.insert(id.to_string());
            }
            TreeEvent::Collapsed(id) => {
                self.expanded.remove(id.as_ref());
            }
        }
    }

    /// Mirrors the tree's selection into the node the editor shows.
    fn follow_selection(&mut self, tree: &Entity<TreeState>, cx: &mut Context<Self>) {
        let selected = tree
            .read(cx)
            .selected_item()
            .and_then(|item| self.nodes.get(item.id.as_ref()).cloned());
        // A folder is a disclosure control. Mouse clicks never select one, and
        // keyboard focus on one must not replace the open table with a folder
        // placeholder.
        if selected
            .as_ref()
            .is_some_and(|node| !node.children.is_empty())
        {
            return;
        }
        // A rebuild clears the tree's selected index while the folder is
        // collapsed. Keep the editor on the last leaf; it is still the open
        // document even when its parent is closed.
        if selected.is_none() {
            return;
        }
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
fn build(
    menu: &[MenuNode],
    expanded: &HashSet<String>,
) -> (Vec<TreeItem>, HashMap<String, MenuNode>) {
    let mut nodes = HashMap::new();
    let items = menu
        .iter()
        .map(|node| tree_item(node, &mut nodes, expanded))
        .collect();
    (items, nodes)
}

fn tree_item(
    node: &MenuNode,
    nodes: &mut HashMap<String, MenuNode>,
    expanded: &HashSet<String>,
) -> TreeItem {
    nodes.insert(node.id.clone(), node.clone());

    let item = TreeItem::new(node.id.clone(), node.label.clone());
    if node.children.is_empty() {
        // Leaves are disabled in the tree component too: every row's click and
        // hover are handled by its own surface, so the list's default hover and
        // selected colours can never stack under the sidebar styling.
        item.disabled(true)
    } else {
        // A folder is a disclosure control, not a page: disabling it removes
        // the tree's select-and-toggle click, and the row's own click toggles
        // expansion without touching the selection.
        item.expanded(expanded.contains(&node.id))
            .disabled(true)
            .children(
                node.children
                    .iter()
                    .map(|child| tree_item(child, nodes, expanded)),
            )
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
        MenuNodeKind::Table | MenuNodeKind::Curve | MenuNodeKind::Dialog => {
            t!("app.editor.unbuilt_hint")
        }
        MenuNodeKind::Standard => t!("app.editor.standard_hint"),
        MenuNodeKind::Help => t!("app.editor.help_hint"),
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
            target: None,
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

        let (items, nodes) = build(&menu, &HashSet::new());

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].children.len(), 1);
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            nodes.get("menu:0/0").map(|node| node.label.as_str()),
            Some("VE Table")
        );
    }

    #[test]
    fn tree_rows_are_disabled_surfaces_and_folders_keep_their_open_state() {
        let menu = vec![node(
            "menu:0",
            "Tuning",
            vec![node("menu:0/0", "VE Table", Vec::new())],
        )];

        let (items, _) = build(&menu, &HashSet::new());
        assert!(items[0].is_folder());
        assert!(
            items[0].is_disabled(),
            "the tree must not select a folder on click"
        );
        assert!(!items[0].is_expanded());
        assert!(
            items[0].children[0].is_disabled(),
            "the row surface owns leaf clicks and hover, not the tree"
        );

        let expanded = HashSet::from(["menu:0".to_owned()]);
        let (items, _) = build(&menu, &expanded);
        assert!(items[0].is_expanded());
    }
}
