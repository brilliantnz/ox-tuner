//! The definition-driven menu tree behind the primary sidebar.
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

use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState};
use gpui_kit::{App, AppContext as _, Context, Entity, SharedString, Subscription};
use oxtuner_core::MenuNode;

use crate::core::CoreService;

/// The navigation tree and what the user picked in it.
pub(crate) struct Explorer {
    tree: Entity<TreeState>,
    /// The menu the tree was built from; compared to skip no-op rebuilds.
    menu: Vec<MenuNode>,
    /// The core definition revision the current menu belongs to.
    definition_revision: u64,
    /// The core menu revision the current gates belong to.
    menu_revision: u64,
    /// Every node by id, for row decoration and selection lookup.
    nodes: Rc<HashMap<String, MenuNode>>,
    /// Ids of the folders the user opened. The tree component's own state is
    /// authoritative while it runs; this copy rebuilds the same view when the
    /// menu is replaced or a folder is clicked.
    expanded: HashSet<String>,
    /// The selected node, cloned so the editor can render it without holding a
    /// borrow on this entity.
    selected: Option<MenuNode>,
    /// Bumped every time the user picks a leaf.
    ///
    /// The tree notifies for expansion and for a menu rebuild too, and the
    /// workspace should raise a tab only on a real pick. A repeated click on the
    /// same leaf still counts: it is a request to show that tab.
    selection_serial: u64,
    /// Keeps the core observer alive.
    _core: Option<Subscription>,
    /// Keeps the selection observer alive.
    _selection: Subscription,
    /// Keeps the expansion observer alive.
    _expansion: Subscription,
}

impl Explorer {
    /// Builds the tree from the definition the core already owns, if any.
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        let status = CoreService::status(cx);
        let menu = current_menu(cx);
        let definition_revision = status.definition_revision;
        let menu_revision = status.menu_revision;
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
        let core = cx.observe_global::<CoreService>(Explorer::sync);

        Self {
            tree,
            menu,
            definition_revision,
            menu_revision,
            nodes: Rc::new(nodes),
            expanded,
            selected: None,
            selection_serial: 0,
            _core: Some(core),
            _selection: selection,
            _expansion: expansion,
        }
    }

    /// Builds an explorer from a menu without touching the core, for UI tests.
    #[cfg(test)]
    pub(crate) fn for_test(menu: Vec<MenuNode>, cx: &mut Context<'_, Self>) -> Self {
        let expanded = HashSet::new();
        let (items, nodes) = build(&menu, &expanded);
        let tree = cx.new(|cx| TreeState::new(cx).items(items));
        let selection = cx.observe(&tree, |this, tree, cx| this.follow_selection(&tree, cx));
        let expansion = cx.subscribe(&tree, |this, _, event, cx| this.on_tree_event(event, cx));

        Self {
            tree,
            menu,
            definition_revision: 0,
            menu_revision: 0,
            nodes: Rc::new(nodes),
            expanded,
            selected: None,
            selection_serial: 0,
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
        Rc::clone(&self.nodes)
    }

    /// The node the user selected, if any.
    pub(crate) fn selected(&self) -> Option<&MenuNode> {
        self.selected.as_ref()
    }

    /// How many times the user has picked a leaf.
    ///
    /// The workspace compares this to the value it last acted on, so a repaint
    /// caused by expansion or a menu rebuild does not steal the front tab.
    pub(crate) fn selection_serial(&self) -> u64 {
        self.selection_serial
    }

    /// Whether the definition declares any menus at all.
    pub(crate) fn has_menu(&self) -> bool {
        !self.menu.is_empty()
    }

    /// Rebuilds the tree when the core publishes a different menu.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        // Rows paint unsaved marks from the core's dirty set, which moves
        // without the menu changing, so every status change repaints the tree.
        cx.notify();

        // Every core status change notifies this observer. The two revisions
        // are the cheap gate: cloning the menu on every table read or save was
        // work with no possible outcome.
        let status = CoreService::status(cx);
        let definition_revision = status.definition_revision;
        let menu_revision = status.menu_revision;
        if definition_revision == self.definition_revision && menu_revision == self.menu_revision {
            return;
        }
        self.definition_revision = definition_revision;
        self.menu_revision = menu_revision;

        let menu = current_menu(cx);
        if menu == self.menu {
            return;
        }

        self.replace_menu(menu, cx);
    }

    /// Replaces the tree while keeping the open folder set and the highlight.
    fn replace_menu(&mut self, menu: Vec<MenuNode>, cx: &mut Context<'_, Self>) {
        // A gate refresh must not drop the highlight from the document in
        // front. Keep the selection and replace it with the new node state.
        let selected_id = self.selected.as_ref().map(|node| node.id.clone());
        let (items, nodes) = build(&menu, &self.expanded);
        let ids: HashSet<String> = nodes.keys().cloned().collect();
        self.expanded.retain(|id| ids.contains(id));
        self.menu = menu;
        self.nodes = Rc::new(nodes);
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));

        self.selected = selected_id.and_then(|id| self.nodes.get(&id).cloned());
        if let Some(node) = &self.selected {
            let id = SharedString::from(node.id.clone());
            let index = self.tree.read(cx).index_of(&id);
            self.tree
                .update(cx, |tree, cx| tree.set_selected_index(index, cx));
        }
        cx.notify();
    }

    /// Opens or closes a folder without changing the selected leaf.
    ///
    /// Folder rows are disabled in the tree component, so its own click handler
    /// cannot select them; this is the one place their expansion changes.
    pub(crate) fn toggle_expanded(&mut self, id: &str, cx: &mut Context<'_, Self>) {
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
    pub(crate) fn select_node(&mut self, id: &str, cx: &mut Context<'_, Self>) {
        let Some(node) = self.nodes.get(id).cloned() else {
            return;
        };
        if !node.children.is_empty() || !node.gates.visible || !node.gates.enabled {
            return;
        }

        self.selected = Some(node);
        self.selection_serial = self.selection_serial.wrapping_add(1);
        let id = SharedString::from(id.to_owned());
        let index = self.tree.read(cx).index_of(&id);
        self.tree
            .update(cx, |tree, cx| tree.set_selected_index(index, cx));
        cx.notify();
    }

    /// Points the highlight at the document the workspace put in front.
    ///
    /// This is the reverse of a click: the dock decides which tab is showing,
    /// and the tree follows. It bumps no selection serial, because nobody
    /// picked a row: the tab was already raised, and opening it again would
    /// only ask the dock to show what it is already showing.
    pub(crate) fn follow_active(&mut self, id: Option<&str>, cx: &mut Context<'_, Self>) {
        let node = id.and_then(|id| self.nodes.get(id)).cloned();

        // A tab can outlive the definition it was opened from. Its node is not
        // in this menu, so there is nothing to point at and the last highlight
        // stays as it is.
        if id.is_some() && node.is_none() {
            return;
        }

        if node.as_ref().is_some_and(|node| !node.children.is_empty()) {
            return;
        }

        if node.is_none() && self.selected.is_none() {
            return;
        }

        let unchanged = node == self.selected;
        self.selected = node;

        let mut index = self.selected.as_ref().and_then(|node| {
            let id = SharedString::from(node.id.clone());
            self.tree.read(cx).index_of(&id)
        });
        // A click inside the document already in front repeats what the tree
        // says; only a highlight the reader cannot see needs work.
        if unchanged && index.is_some() {
            return;
        }

        // A document inside a closed folder has no row to highlight. Open the
        // folders that hold it, so the tree shows the document the tab shows.
        if index.is_none() {
            let active = self.selected.as_ref().map(|node| node.id.clone());
            if let Some(id) = active {
                index = self.reveal_folders(&id, cx);
            }
        }

        self.tree
            .update(cx, |tree, cx| tree.set_selected_index(index, cx));
        cx.notify();
    }

    /// Rebuilds the tree with the folders that hold `id` open.
    ///
    /// Returns the row index of `id` once the rebuild is done, or `None` when
    /// the node is not in the menu at all.
    fn reveal_folders(&mut self, id: &str, cx: &mut Context<'_, Self>) -> Option<usize> {
        let mut path = Vec::new();
        if !find_path(&self.menu, id, &mut path) {
            return None;
        }

        // The last entry is the node itself; only its parents are folders.
        path.pop();
        let closed: Vec<String> = path
            .into_iter()
            .filter(|folder| !self.expanded.contains(folder))
            .collect();
        if closed.is_empty() {
            return None;
        }

        self.expanded.extend(closed);

        let (items, nodes) = build(&self.menu, &self.expanded);
        self.nodes = Rc::new(nodes);
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));

        let selected = SharedString::from(id.to_owned());
        self.tree.read(cx).index_of(&selected)
    }

    /// Mirrors keyboard expansion from the tree into [`Self::expanded`].
    fn on_tree_event(&mut self, event: &TreeEvent, _: &mut Context<'_, Self>) {
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
    fn follow_selection(&mut self, tree: &Entity<TreeState>, cx: &mut Context<'_, Self>) {
        let selected = tree
            .read(cx)
            .selected_item()
            .and_then(|item| self.nodes.get(item.id.as_ref()).cloned());
        // A folder is a disclosure control. Mouse clicks never select one, and
        // keyboard focus on one must not replace the open table with a folder
        // placeholder.
        if selected.as_ref().is_some_and(|node| {
            !node.children.is_empty() || !node.gates.visible || !node.gates.enabled
        }) {
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

/// The ids from the root to `id`, or `false` when the menu does not hold it.
fn find_path(menu: &[MenuNode], id: &str, path: &mut Vec<String>) -> bool {
    for node in menu {
        path.push(node.id.clone());

        if node.id == id || find_path(&node.children, id, path) {
            return true;
        }

        path.pop();
    }

    false
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

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;
    use oxtuner_core::{GateState, MenuNodeKind};

    use super::*;

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
            embedded: None,
            enable_expression: None,
            visibility_expression: None,
            gates: GateState::default(),
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

    #[gpui_kit::test]
    fn a_disabled_node_is_not_selectable(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let mut disabled = node("menu:0/0", "Disabled", Vec::new());
        disabled.gates.enabled = false;
        let explorer = cx.update(|cx| {
            cx.new(|cx| Explorer::for_test(vec![node("menu:0", "Tuning", vec![disabled])], cx))
        });

        cx.update(|cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.select_node("menu:0/0", cx);
            });
        });

        assert!(
            cx.update(|cx| explorer.read(cx).selected().is_none()),
            "a disabled leaf must not open"
        );
    }

    #[gpui_kit::test]
    fn a_gate_refresh_keeps_the_selected_document(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let explorer = cx.update(|cx| {
            cx.new(|cx| {
                Explorer::for_test(
                    vec![node(
                        "menu:0",
                        "Tuning",
                        vec![node("menu:0/0", "VE Table", Vec::new())],
                    )],
                    cx,
                )
            })
        });

        cx.update(|cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.select_node("menu:0/0", cx);
            });
        });

        let mut updated = node("menu:0/0", "VE Table", Vec::new());
        updated.gates.enabled = false;
        cx.update(|cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.replace_menu(vec![node("menu:0", "Tuning", vec![updated])], cx);
            });
        });

        let selected = cx.update(|cx| explorer.read(cx).selected().cloned());
        let selected = selected.expect("the document stays selected");
        assert_eq!(selected.label, "VE Table");
        assert!(
            !selected.gates.enabled,
            "the highlight carries the refreshed gate state"
        );
    }

    #[gpui_kit::test]
    fn the_highlight_follows_the_document_in_front(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let explorer = cx.update(|cx| {
            cx.new(|cx| {
                Explorer::for_test(
                    vec![node(
                        "menu:0",
                        "Tuning",
                        vec![node("menu:0/0", "VE Table", Vec::new())],
                    )],
                    cx,
                )
            })
        });

        cx.update(|cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.follow_active(Some("menu:0/0"), cx);
            });
        });

        let selected = cx.update(|cx| explorer.read(cx).selected().cloned());
        assert_eq!(selected.map(|node| node.label), Some("VE Table".to_owned()));
        assert_eq!(
            cx.update(|cx| explorer.read(cx).selection_serial()),
            0,
            "the dock moved the highlight, so no reader pick happened"
        );

        cx.update(|cx| {
            explorer.update(cx, |explorer, cx| explorer.follow_active(None, cx));
        });
        assert!(
            cx.update(|cx| explorer.read(cx).selected().is_none()),
            "the charts tab is not a document, so nothing is highlighted"
        );
    }

    #[gpui_kit::test]
    fn a_document_in_a_closed_folder_opens_its_folder(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);

        let explorer = cx.update(|cx| {
            cx.new(|cx| {
                Explorer::for_test(
                    vec![node(
                        "menu:0",
                        "Tuning",
                        vec![node("menu:0/0", "VE Table", Vec::new())],
                    )],
                    cx,
                )
            })
        });

        cx.update(|cx| {
            explorer.update(cx, |explorer, cx| {
                explorer.follow_active(Some("menu:0/0"), cx);
            });
        });

        assert!(
            cx.update(|cx| explorer.read(cx).expanded.contains("menu:0")),
            "the folder holding the active document has to be open"
        );
        assert!(
            cx.update(|cx| explorer
                .read(cx)
                .tree()
                .read(cx)
                .index_of(&"menu:0/0".into())
                .is_some()),
            "the highlighted row must exist in the tree"
        );
    }
}
