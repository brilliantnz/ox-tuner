//! How a definition menu node is named and pictured.
//!
//! The menu is the sidebar's content, so the naming lives with it. The
//! navigation rows and the editor's placeholder both describe the same
//! [`MenuNode`], so its kind word, size detail and icon live here once. The
//! helpers take the node plus the copy they need, and read no state.

use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla, SharedString};
use oxtuner_core::{MenuDetail, MenuNode, MenuNodeKind};

use crate::i18n::t;

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

/// The colour of a node's icon.
///
/// The kind decides, and nothing else: a table and a dialog are different kinds
/// of document, so the sidebar row and the editor tab that opens from it have
/// to agree about which one is on screen. Folders and plain nodes stay muted so
/// the tree's structure reads before its decoration.
pub(crate) fn icon_color(kind: MenuNodeKind, cx: &App) -> Hsla {
    let theme = cx.theme();

    match kind {
        MenuNodeKind::Table => theme.primary,
        MenuNodeKind::Curve => theme.success,
        MenuNodeKind::Dialog => theme.warning,
        MenuNodeKind::Help => theme.info,
        MenuNodeKind::Missing => theme.danger,
        MenuNodeKind::Menu | MenuNodeKind::Group | MenuNodeKind::Standard => theme.muted_foreground,
    }
}

/// The icon for a node that is not drawn as an open/closed folder.
///
/// Names come from gpui-kit's full catalog (`AllAssets` in `main`), which is
/// what the app installs; a name the bundle does not carry renders empty.
pub(crate) fn icon(kind: MenuNodeKind) -> IconName {
    match kind {
        // A menu is a folder in the tree; the editor header shows the same.
        MenuNodeKind::Menu | MenuNodeKind::Group => IconName::Folder,
        MenuNodeKind::Table => IconName::Table,
        MenuNodeKind::Curve => IconName::ChartLine,
        MenuNodeKind::Dialog => IconName::Frame,
        MenuNodeKind::Help => IconName::BookOpenText,
        MenuNodeKind::Standard => IconName::Settings2,
        MenuNodeKind::Missing => IconName::TriangleAlert,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui_kit::test]
    fn document_kinds_take_distinct_icon_colors(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::init);

        cx.update(|cx| {
            let table = icon_color(MenuNodeKind::Table, cx);
            let curve = icon_color(MenuNodeKind::Curve, cx);
            let dialog = icon_color(MenuNodeKind::Dialog, cx);

            assert_ne!(table, curve, "a curve must not read as a table");
            assert_ne!(table, dialog, "a dialog must not read as a table");
            assert_ne!(curve, dialog, "a dialog must not read as a curve");
            assert_eq!(
                icon_color(MenuNodeKind::Menu, cx),
                icon_color(MenuNodeKind::Group, cx),
                "folders share the muted tone"
            );
            assert_eq!(
                icon_color(MenuNodeKind::Standard, cx),
                cx.theme().muted_foreground,
                "a plain node takes no kind colour"
            );
        });
    }
}
