/// What a menu item points at.
#[expect(
    clippy::exhaustive_enums,
    reason = "the menu grammar has a fixed set of target kinds"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuTargetKind {
    /// Another dialog / table / curve.
    DefinitionTarget,
    /// A divider.
    Separator,
    /// A group header declared with `groupMenu`.
    Group,
    /// One of the application's standard dialogs (`std_*`).
    Standard,
}

/// One menu item.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// Item kind.
    pub kind: MenuTargetKind,
    /// Target id, empty for a separator and for a group header.
    pub target: String,
    /// Label as written (may contain `&`).
    pub label: String,
    /// Visibility/enable expressions, as written.
    pub conditions: Vec<String>,
    /// Index in the owning menu's `items` of the `groupMenu` this entry was
    /// declared under with `groupChildMenu`.
    pub group: Option<usize>,
    /// Source line.
    pub line: u32,
}

/// One top-level menu.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    /// Menu label without the surrounding quotes.
    pub title: String,
    /// Items in document order.
    pub items: Vec<MenuItem>,
    /// Window the menu belongs to (`main` when unset).
    pub dialog: Option<String>,
    /// Enable/visible expressions on the `menu =` line, as written.
    pub conditions: Vec<String>,
}
