//! The navigation tree the UI draws in the primary sidebar.
//!
//! It is built here, from the definition, for two reasons: the menu structure is
//! definition data (the UI never sees `oxtuner-ini` types), and the labels need
//! the `&` mnemonic markers `TunerStudio` writes removed.
//!
//! The tree mirrors the `[Menu]` sections: a top-level `menu =` is a folder, a
//! `groupMenu` is a nested folder, and every other entry is a leaf pointing at a
//! table, a curve, a dialog or one of the built-in `std_*` dialogs. Separators
//! are not nodes. A target the definition does not declare stays visible as
//! [`MenuNodeKind::Missing`] rather than disappearing silently.
//!
//! Sizes travel as [`MenuDetail`], not as text: the core has no language, so the
//! UI phrases them (`16 × 16`, `9 points`, `4 rows`).

use std::collections::{HashMap, HashSet};

use oxtuner_ini::{Curve, Definition, Menu, MenuItem, MenuTargetKind, RowKind, Shape, Table};

use crate::message::{condition_body, order_gates};

/// What a node in the navigation tree is.
#[expect(
    clippy::exhaustive_enums,
    reason = "the navigation tree has a fixed set of node kinds"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuNodeKind {
    /// A top-level `menu =` section.
    Menu,
    /// A `groupMenu` group inside a menu.
    Group,
    /// A 2-D table.
    Table,
    /// A 1-D curve.
    Curve,
    /// A user-defined dialog.
    Dialog,
    /// A help topic declared in a dialog (`help = name, "Title"`).
    Help,
    /// One of `TunerStudio`'s built-in dialogs (`std_*`).
    Standard,
    /// A target the definition does not declare.
    Missing,
}

/// A size or count, left for the UI to phrase in its own language.
#[expect(
    clippy::exhaustive_enums,
    reason = "a menu detail is a table grid, a curve point count or a row count"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuDetail {
    /// A table's fixed grid, columns first.
    Table {
        /// Columns.
        cols: u32,
        /// Rows.
        rows: u32,
    },
    /// A curve's fixed number of points.
    Curve {
        /// Points along the x axis.
        points: u32,
    },
    /// A dialog's row count.
    Dialog {
        /// Rows in the dialog layout.
        rows: usize,
    },
}

/// One node of the navigation tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuNode {
    /// Stable path, unique within the tree (`menu:2/5`).
    pub id: String,
    /// Display label, with `&` mnemonic markers removed.
    pub label: String,
    /// What the node is.
    pub kind: MenuNodeKind,
    /// Size/count, when the definition makes it known.
    pub detail: Option<MenuDetail>,
    /// The definition object this node opens, when it opens one.
    ///
    /// For a table this is the table id (not the menu's map id), for a curve or
    /// dialog its name. The editor asks the core for that id; it never reads the
    /// definition itself.
    pub target: Option<String>,
    /// The table or curve a dialog's panels embed, when it declares one.
    ///
    /// `VE Table` in the Speeduino menu is a dialog whose north panel is the
    /// 16×16 fuel map: opening the dialog name alone would lose the grid, so the
    /// id travels beside the dialog target and the editor can show both.
    pub embedded: Option<String>,
    /// Enable condition from the item (`{ … }`), as written.
    ///
    /// Menu items take `{enable}` first and `{visible}` second; a single gate
    /// is always the enable one. The core carries the expressions raw.
    pub enabled: Option<String>,
    /// Visibility condition from the item (`{ … }`), as written.
    pub visible: Option<String>,
    /// Child nodes, in definition order.
    pub children: Vec<MenuNode>,
}

/// Builds the tree from the definition's `[Menu]` sections.
///
/// The order is the definition's order. When it declares no menus the result is
/// empty; that is a definition without navigation, not an error.
pub(crate) fn build(definition: &Definition) -> Vec<MenuNode> {
    definition
        .menus
        .iter()
        .enumerate()
        .map(|(index, menu)| build_menu(index, menu, definition))
        .collect()
}

fn build_menu(index: usize, menu: &Menu, definition: &Definition) -> MenuNode {
    let mut children: Vec<MenuNode> = Vec::new();
    // Where each group header landed in `children`, keyed by its index in
    // `menu.items`, so a child finds its group without scanning the tree.
    let mut groups: HashMap<usize, usize> = HashMap::new();

    for (position, item) in menu.items.iter().enumerate() {
        let Some(node) = build_item(index, position, item, definition) else {
            continue;
        };

        let owner = item.group.and_then(|owner| groups.get(&owner).copied());
        if let Some(owner) = owner {
            children[owner].children.push(node);
        } else {
            let is_group = item.kind == MenuTargetKind::Group;
            children.push(node);
            if is_group {
                groups.insert(position, children.len() - 1);
            }
        }
    }

    MenuNode {
        id: format!("menu:{index}"),
        label: clean_label(&menu.title),
        kind: MenuNodeKind::Menu,
        detail: None,
        target: None,
        embedded: None,
        enabled: None,
        visible: None,
        children,
    }
}

/// One item as a node, or `None` for a divider (a node with no content).
fn build_item(
    menu: usize,
    position: usize,
    item: &MenuItem,
    definition: &Definition,
) -> Option<MenuNode> {
    if item.kind == MenuTargetKind::Separator || is_separator(item) {
        return None;
    }

    let id = format!("menu:{menu}/{position}");
    let label = clean_label(&item.label);
    let label = if label.is_empty() {
        item.target.clone()
    } else {
        label
    };

    let node = match item.kind {
        MenuTargetKind::Group => MenuNode {
            id,
            label,
            kind: MenuNodeKind::Group,
            detail: None,
            target: None,
            embedded: None,
            enabled: None,
            visible: None,
            children: Vec::new(),
        },
        MenuTargetKind::Standard => MenuNode {
            id,
            label,
            kind: MenuNodeKind::Standard,
            detail: None,
            target: None,
            embedded: None,
            enabled: None,
            visible: None,
            children: Vec::new(),
        },
        MenuTargetKind::Panel => resolve(id, label, &item.target, definition),
        // Filtered above; a divider never becomes a node.
        MenuTargetKind::Separator => return None,
    };

    let (visible, enabled) = item_gates(item);
    let mut node = node;
    node.visible = visible;
    node.enabled = enabled;
    Some(node)
}

/// Enable and visibility gates of one menu item, as written.
///
/// Items take `{enable}` first and `{visible}` second; a single gate is always
/// the enable one. Bare numbers (`0`) are placeholders and skipped; any other
/// bare token travels on as written.
fn item_gates(item: &MenuItem) -> (Option<String>, Option<String>) {
    let mut found = Vec::new();

    for condition in &item.conditions {
        let trimmed = condition.trim();

        if let Some(body) = condition_body(trimmed) {
            found.push(body);
            continue;
        }

        if trimmed.is_empty() || trimmed.parse::<f64>().is_ok() {
            continue;
        }

        found.push(trimmed.to_owned());
    }

    order_gates(found)
}

/// Resolves a `subMenu` target to the editor it opens.
///
/// A target is looked up as a table first, then a curve, then a dialog: those
/// are the three things a `subMenu` can point at, and a definition that reuses
/// one id for two of them has no unambiguous answer. An unknown target is kept
/// visible as [`MenuNodeKind::Missing`].
fn resolve(id: String, label: String, target: &str, definition: &Definition) -> MenuNode {
    if let Some(table) = definition.tables.get_ci(target) {
        return MenuNode {
            id,
            label,
            kind: MenuNodeKind::Table,
            detail: table_detail(table, definition),
            target: Some(table.id.clone()),
            embedded: None,
            enabled: None,
            visible: None,
            children: Vec::new(),
        };
    }
    // A 3-D map is a table's second id; the `3D Tuning Maps` menu uses it.
    if let Some(table) = table_by_target(target, definition) {
        return MenuNode {
            id,
            label,
            kind: MenuNodeKind::Table,
            detail: table_detail(table, definition),
            target: Some(table.id.clone()),
            embedded: None,
            enabled: None,
            visible: None,
            children: Vec::new(),
        };
    }
    if let Some(curve) = definition.curves.get_ci(target) {
        return MenuNode {
            id,
            label,
            kind: MenuNodeKind::Curve,
            detail: curve_detail(curve, definition),
            target: Some(curve.name.clone()),
            embedded: None,
            enabled: None,
            visible: None,
            children: Vec::new(),
        };
    }
    if let Some(dialog) = definition.dialogs.get_ci(target) {
        let embedded = embedded_target(&dialog.name, definition, &mut HashSet::new());
        return MenuNode {
            id,
            label,
            kind: MenuNodeKind::Dialog,
            detail: embedded
                .as_ref()
                .map(|found| found.detail)
                .or(Some(MenuDetail::Dialog {
                    rows: dialog.rows.len(),
                })),
            target: Some(dialog.name.clone()),
            embedded: embedded.map(|found| found.id),
            enabled: None,
            visible: None,
            children: Vec::new(),
        };
    }
    if is_help_topic(definition, target) {
        return MenuNode {
            id,
            label,
            kind: MenuNodeKind::Help,
            detail: None,
            target: Some(target.to_owned()),
            embedded: None,
            enabled: None,
            visible: None,
            children: Vec::new(),
        };
    }
    MenuNode {
        id,
        label,
        kind: MenuNodeKind::Missing,
        detail: None,
        target: Some(target.to_owned()),
        embedded: None,
        enabled: None,
        visible: None,
        children: Vec::new(),
    }
}

/// Whether a dialog declares `target` as a `help = name, "Title"` topic.
fn is_help_topic(definition: &Definition, target: &str) -> bool {
    definition.dialogs.values().any(|dialog| {
        dialog.rows.iter().any(|row| {
            row.kind == RowKind::Help
                && row.fields.first().is_some_and(|field| {
                    field.trim().trim_matches('"').eq_ignore_ascii_case(target)
                })
        })
    })
}

/// The table a target names, by table id or by 3-D map id.
fn table_by_target<'a>(target: &str, definition: &'a Definition) -> Option<&'a Table> {
    definition.tables.get_ci(target).or_else(|| {
        definition.tables.values().find(|table| {
            table
                .map_id
                .as_deref()
                .is_some_and(|map| map.eq_ignore_ascii_case(target))
        })
    })
}

/// A table or curve a dialog's panels carry.
///
/// The id is the one the core reads snapshots by, and the detail is its size,
/// so a menu node can show the grid instead of the dialog's row count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddedTarget {
    pub(crate) id: String,
    pub(crate) detail: MenuDetail,
}

/// Walks a dialog and the dialogs its panels embed, looking for a table or curve.
///
/// Panels nest (a dialog panel may hold another dialog that holds the map), and
/// definitions can reference each other in cycles, so visited names are
/// remembered and cut the walk short. A table wins over a curve in the same
/// dialog because a grid is the larger thing to open; the first one found in
/// row order is used.
pub(crate) fn embedded_target(
    name: &str,
    definition: &Definition,
    visited: &mut HashSet<String>,
) -> Option<EmbeddedTarget> {
    if !visited.insert(name.to_ascii_lowercase()) {
        return None;
    }
    let dialog = definition.dialogs.get_ci(name)?;
    for row in &dialog.rows {
        if row.kind != RowKind::Panel {
            continue;
        }
        let Some(field) = row.fields.first() else {
            continue;
        };
        let target = field.trim().trim_matches('"');
        if let Some(table) = table_by_target(target, definition)
            && let Some(detail) = table_detail(table, definition)
        {
            return Some(EmbeddedTarget {
                id: table.id.clone(),
                detail,
            });
        }
        if let Some(curve) = definition.curves.get_ci(target)
            && let Some(detail) = curve_detail(curve, definition)
        {
            return Some(EmbeddedTarget {
                id: curve.name.clone(),
                detail,
            });
        }
        if let Some(found) = embedded_target(target, definition, visited) {
            return Some(found);
        }
    }
    None
}

/// The cell grid, when the table's array is a fixed 2-D shape.
fn table_detail(table: &Table, definition: &Definition) -> Option<MenuDetail> {
    let shape = constant_shape(definition, table.z_bins.as_deref()?)?;
    match shape {
        Shape::Table { cols, rows } => Some(MenuDetail::Table {
            cols: cols.value()?,
            rows: rows.value()?,
        }),
        _ => None,
    }
}

/// The point count, when the curve's x bins are a fixed list.
fn curve_detail(curve: &Curve, definition: &Definition) -> Option<MenuDetail> {
    let shape = constant_shape(definition, &curve.x_bins.as_ref()?.array)?;
    match shape {
        Shape::List(length) => Some(MenuDetail::Curve {
            points: length.value()?,
        }),
        _ => None,
    }
}

fn constant_shape<'a>(definition: &'a Definition, name: &str) -> Option<&'a Shape> {
    definition
        .constants
        .get_ci(name)
        .map(|constant| &constant.shape)
}

/// `std_separator` in its `subMenu = std_separator` form.
fn is_separator(item: &MenuItem) -> bool {
    item.target.eq_ignore_ascii_case("std_separator")
}

/// A label without `TunerStudio`'s `&` mnemonics.
///
/// `&` before a character marks the keyboard mnemonic and is dropped; `&&`
/// means a literal ampersand and keeps one. Any other `&` is text.
fn clean_label(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '&' {
            cleaned.push(character);
            continue;
        }
        match characters.peek() {
            Some(next) if next.is_alphanumeric() => {}
            Some('&') => {
                characters.next();
                cleaned.push('&');
            }
            _ => cleaned.push('&'),
        }
    }
    cleaned.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;

    const FIXTURE: &str = "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
pageSize = 288
page = 1
veTable = array, U08, 0, [16x16], \"%\", 1, 0, 0, 255, 0
rpmBins = array, U16, 512, [9], \"rpm\", 1, 0, 0, 255, 0

[TableEditor]
table = veTable1Tbl, veTable1Map, \"VE Table\", 1
  xBins = rpmBins, rpm
  yBins = rpmBins, load
  zBins = veTable

[CurveEditor]
curve = idleCurve, \"Idle\"
  xBins = rpmBins, coolant
  yBins = veTable

[UserDefined]
dialog = veTableDialog, \"VE Table\"
panel = veTable1Tbl, Center
dialog = helpDialog, \"Help\"
help = helpGeneral, \"Manual\"

[Menu]
menuDialog = main
menu = \"3D &Tuning Maps\"
  subMenu = std_realtime, \"Realtime Display\"
  groupMenu = \"Protection\"
    groupChildMenu = veTableDialog, \"Common\"
    groupChildMenu = missingPanel, \"Ghost\"
  subMenu = veTable1Tbl, \"VE Table\"
  subMenu = veTable1Map, \"Fuel Map\"
  subMenu = idleCurve, \"Idle Curve\"
  subMenu = helpGeneral, \"Help Topic\"
  subMenu = std_separator
";

    const GATED_FIXTURE: &str = "\
[MegaTune]
signature = \"speeduino 202501\"

[Menu]
menuDialog = main
menu = \"Gated\"
  subMenu = veTableDialog, \"Both\", { engineOn }, { showBoth }
  subMenu = idleCurve, \"Single\", { engineOn }
  subMenu = helpDialog, \"Bare\", 0, { showHelp }
";

    fn parse(text: &str) -> Definition {
        parse_str(text, &SettingsContext::new()).expect("parses")
    }

    fn node<'a>(nodes: &'a [MenuNode], label: &str) -> &'a MenuNode {
        nodes
            .iter()
            .find(|node| node.label == label)
            .unwrap_or_else(|| panic!("no node named {label}"))
    }

    #[test]
    fn menus_groups_and_targets_become_one_tree() {
        let tree = build(&parse(FIXTURE));

        assert_eq!(tree.len(), 1);
        let menu = &tree[0];
        assert_eq!(menu.label, "3D Tuning Maps", "the mnemonic marker is gone");
        assert_eq!(menu.kind, MenuNodeKind::Menu);

        let labels: Vec<&str> = menu
            .children
            .iter()
            .map(|node| node.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec![
                "Realtime Display",
                "Protection",
                "VE Table",
                "Fuel Map",
                "Idle Curve",
                "Help Topic"
            ],
            "separators are not nodes"
        );

        let standard = node(&menu.children, "Realtime Display");
        assert_eq!(standard.kind, MenuNodeKind::Standard);

        let group = node(&menu.children, "Protection");
        assert_eq!(group.kind, MenuNodeKind::Group);
        assert_eq!(group.children.len(), 2);
        assert_eq!(group.children[0].kind, MenuNodeKind::Dialog);
        assert_eq!(
            group.children[0].detail,
            Some(MenuDetail::Table { cols: 16, rows: 16 }),
            "the dialog embeds the table, so its grid is the useful size"
        );
        assert_eq!(group.children[0].target.as_deref(), Some("veTableDialog"));
        assert_eq!(group.children[0].embedded.as_deref(), Some("veTable1Tbl"));
        assert_eq!(group.children[1].kind, MenuNodeKind::Missing);

        let table = node(&menu.children, "VE Table");
        assert_eq!(table.kind, MenuNodeKind::Table);
        assert_eq!(table.detail, Some(MenuDetail::Table { cols: 16, rows: 16 }));
        assert_eq!(table.target.as_deref(), Some("veTable1Tbl"));

        // The 3-D map id resolves to the table that declares it.
        let map = node(&menu.children, "Fuel Map");
        assert_eq!(map.kind, MenuNodeKind::Table);
        assert_eq!(map.detail, Some(MenuDetail::Table { cols: 16, rows: 16 }));
        assert_eq!(
            map.target.as_deref(),
            Some("veTable1Tbl"),
            "the editor opens the table id, not the map id"
        );

        let curve = node(&menu.children, "Idle Curve");
        assert_eq!(curve.kind, MenuNodeKind::Curve);
        assert_eq!(curve.detail, Some(MenuDetail::Curve { points: 9 }));

        let help = node(&menu.children, "Help Topic");
        assert_eq!(help.kind, MenuNodeKind::Help);
    }

    #[test]
    fn every_node_has_a_unique_stable_id() {
        fn collect(nodes: &[MenuNode], ids: &mut Vec<String>) {
            for node in nodes {
                ids.push(node.id.clone());
                collect(&node.children, ids);
            }
        }

        let tree = build(&parse(FIXTURE));
        let mut ids = Vec::new();
        collect(&tree, &mut ids);

        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "ids: {ids:?}");
        assert_eq!(tree[0].id, "menu:0");
        assert_eq!(tree[0].children[1].id, "menu:0/1");
        assert_eq!(tree[0].children[1].children[0].id, "menu:0/2");
    }

    #[test]
    fn a_definition_without_menus_has_an_empty_tree() {
        let definition = parse("[MegaTune]\nsignature = \"x\"\n");
        assert!(
            build(&definition).is_empty(),
            "a definition without menus must build an empty tree"
        );
    }

    #[test]
    fn menu_gates_travel_enable_first() {
        let tree = build(&parse(GATED_FIXTURE));

        assert_eq!(tree.len(), 1);
        let menu = &tree[0];

        let both = node(&menu.children, "Both");
        assert_eq!(both.enabled.as_deref(), Some("engineOn"));
        assert_eq!(both.visible.as_deref(), Some("showBoth"));

        let single = node(&menu.children, "Single");
        assert_eq!(single.enabled.as_deref(), Some("engineOn"));
        assert_eq!(single.visible, None);

        let bare = node(&menu.children, "Bare");
        assert_eq!(
            bare.enabled.as_deref(),
            Some("showHelp"),
            "a bare `0` placeholder is skipped, the brace is the enable gate"
        );
        assert_eq!(bare.visible, None);
    }

    #[test]
    fn a_dialog_reports_the_grid_of_the_table_it_embeds() {
        let definition = parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
page = 1
pageSize = 288
veTable = array, U08, 0, [16x16], \"%\", 1, 0, 0, 255, 0

[TableEditor]
table = veTable1Tbl, veTable1Map, \"VE Table\", 1
  zBins = veTable

[UserDefined]
dialog = north, \"\"
panel = veTable1Tbl, North
dialog = veTableDialog, \"VE Table\"
panel = north, North
panel = loopB, South
dialog = plainDialog, \"Plain\"
field = \"Multiplier\", multiplyMAP
dialog = loopA, \"A\"
panel = loopB, Center
dialog = loopB, \"B\"
panel = loopA, Center

[Menu]
menuDialog = main
menu = \"&Fuel\"
  subMenu = veTableDialog, \"VE Table\"
  subMenu = plainDialog, \"Plain\"
  subMenu = loopA, \"Loop\"
",
        );
        let tree = build(&definition);

        // The table sits one panel deeper than the menu's dialog.
        let ve_table = &tree[0].children[0];
        assert_eq!(ve_table.kind, MenuNodeKind::Dialog);
        assert_eq!(
            ve_table.detail,
            Some(MenuDetail::Table { cols: 16, rows: 16 }),
            "the embedded table's grid is the size the reader cares about"
        );
        assert_eq!(
            ve_table.embedded.as_deref(),
            Some("veTable1Tbl"),
            "the editor can open the grid the dialog wraps"
        );

        let plain = &tree[0].children[1];
        assert_eq!(
            plain.detail,
            Some(MenuDetail::Dialog { rows: 1 }),
            "a dialog without a table keeps its row count"
        );
        assert_eq!(plain.embedded, None);

        let cycle = &tree[0].children[2];
        assert_eq!(
            cycle.detail,
            Some(MenuDetail::Dialog { rows: 1 }),
            "panels that reference each other end the walk, not the program"
        );
        assert_eq!(cycle.embedded, None);
    }

    #[test]
    fn a_literal_ampersand_survives_the_mnemonic_pass() {
        assert_eq!(clean_label("Fuel && Spark"), "Fuel & Spark");
        assert_eq!(clean_label("&Tuning"), "Tuning");
        assert_eq!(clean_label("AFR & Fuel"), "AFR & Fuel");
        assert_eq!(clean_label("  Padded  "), "Padded");
    }
}
