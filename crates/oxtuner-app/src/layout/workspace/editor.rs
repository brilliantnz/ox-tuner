//! One open document: a table, a curve, or a dialog, as its own dock tab.
//!
//! Each panel owns the editors it shows, so two tables open side by side do not
//! share a cursor, a scroll position or an edit in progress. The panel reads the
//! node it was opened for and never changes it: a different node is a different
//! tab.
//!
//! A dialog that wraps a table offers both views — its grid and its constants —
//! and keeps that choice for as long as the tab lives.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window, div,
    px,
};
use oxtuner_core::{MenuNode, MenuNodeKind};
use oxtuner_ui::dock::{BasePanel, Panel, PanelEvent};

use super::ActiveDocument;
use crate::core::CoreService;
use crate::editor::{dialog::DialogEditor, table::TableEditor};
use crate::i18n::t;
use crate::layout::primary_sidebar::menu_node;

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

/// One document in the workspace.
pub(crate) struct EditorPanel {
    focus: FocusHandle,
    /// The node this tab was opened for. Tabs are per node; the node itself
    /// never changes under the panel.
    node: MenuNode,
    /// Which view a dialog-backed node shows.
    tab: EditorTab,
    table: Entity<TableEditor>,
    dialog: Entity<DialogEditor>,
}

impl EditorPanel {
    /// An editor for `node`, with its first read already requested.
    pub(crate) fn new(node: MenuNode, cx: &mut Context<'_, Self>) -> Self {
        let table = cx.new(TableEditor::new);
        let dialog = cx.new(DialogEditor::new);

        let mut panel = Self {
            focus: cx.focus_handle(),
            node,
            tab: EditorTab::Table,
            table,
            dialog,
        };
        panel.sync(cx);
        panel
    }

    /// The node this tab shows.
    pub(crate) fn node_id(&self) -> &str {
        &self.node.id
    }

    /// The menu label, for the close confirmation.
    pub(crate) fn label(&self) -> &str {
        &self.node.label
    }

    /// The definition target this tab edits, for a scoped revert.
    pub(crate) fn target(&self) -> Option<&str> {
        self.node.target.as_deref()
    }

    /// Points the editors at what the tab currently shows.
    ///
    /// Tables and curves share the grid: a curve resolves as a one-row table,
    /// so its bins and values edit the same way. Dialogs get the grid they
    /// embed, with their constants behind a tab, or the constants alone.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let label = SharedString::from(self.node.label.clone());
        let has_grid = self.node.embedded.is_some();

        let table_target = match self.node.kind {
            MenuNodeKind::Table | MenuNodeKind::Curve => self.node.target.clone(),
            MenuNodeKind::Dialog if has_grid && self.tab == EditorTab::Table => {
                self.node.embedded.clone()
            }
            _ => None,
        };
        let dialog_target = match self.node.kind {
            MenuNodeKind::Dialog if !(has_grid && self.tab == EditorTab::Table) => {
                self.node.target.clone()
            }
            _ => None,
        };

        self.table.update(cx, |editor, cx| {
            editor.show(label.clone(), table_target.as_deref(), cx);
        });
        self.dialog.update(cx, |editor, cx| {
            editor.show(label, dialog_target.as_deref(), cx);
        });
    }

    /// Switches which view a dialog-backed node shows.
    fn set_tab(&mut self, tab: EditorTab, cx: &mut Context<'_, Self>) {
        if self.tab == tab {
            return;
        }

        self.tab = tab;
        self.sync(cx);
        cx.notify();
    }

    /// The grid/constants switch above a dialog that wraps a table.
    fn render_switch(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
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

    /// What a node with no editor of its own shows: what it is and what can
    /// happen next.
    fn render_placeholder(&self, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;

        v_flex()
            .size_full()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(
                Icon::new(menu_node::icon(self.node.kind))
                    .large()
                    .text_color(muted),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(self.node.label.clone())),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(menu_node::summary(&self.node)),
            )
            .child(
                div()
                    .max_w(px(440.))
                    .text_sm()
                    .text_center()
                    .text_color(muted)
                    .child(menu_node::hint(self.node.kind)),
            )
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for EditorPanel {}

impl EventEmitter<ActiveDocument> for EditorPanel {}

impl Focusable for EditorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for EditorPanel {
    fn panel_name(&self) -> &'static str {
        "EditorPanel"
    }

    /// The tab strip draws no zoom control, so a zoomed group would have no way
    /// back out.
    fn zoomable(&self, _: &App) -> bool {
        false
    }

    /// The dock raised this tab; tell the workspace which document that is, so
    /// the navigation tree highlights the node the reader is editing.
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<'_, Self>) {
        if active {
            cx.emit(ActiveDocument(Some(self.node.id.clone())));
        }
    }
}

impl Panel for EditorPanel {
    /// Whether this document has cells that differ from the file baseline.
    ///
    /// The core keeps the marks per target; the tab only asks. A dialog that
    /// wraps a table is dirty when either side is.
    fn modified(&self, cx: &App) -> bool {
        let dirty = CoreService::dirty(cx);

        self.node
            .target
            .as_deref()
            .is_some_and(|target| dirty.contains(target))
            || self
                .node
                .embedded
                .as_deref()
                .is_some_and(|target| dirty.contains(target))
    }

    /// The tab's face. The strip falls back to this when a panel does not
    /// provide a short name, which is how the kind icon reaches the tab.
    fn title(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(
                Icon::new(menu_node::icon(self.node.kind))
                    .small()
                    .text_color(menu_node::icon_color(self.node.kind, cx)),
            )
            .child(SharedString::from(self.node.label.clone()))
    }

    /// The tab already names the document, and the editor draws its own toolbar
    /// and padding.
    fn title_bar(&self, _cx: &App) -> bool {
        false
    }

    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for EditorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let is_table = matches!(self.node.kind, MenuNodeKind::Table | MenuNodeKind::Curve);
        let is_dialog = self.node.kind == MenuNodeKind::Dialog;
        let dialog_has_grid = is_dialog && self.node.embedded.is_some();
        let show_table = is_table || (dialog_has_grid && self.tab == EditorTab::Table);
        let show_dialog = is_dialog && !show_table;

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(dialog_has_grid, |this| this.child(self.render_switch(cx)))
            .when(show_table, |this| {
                this.child(div().flex_1().min_h_0().child(self.table.clone()))
            })
            .when(show_dialog, |this| {
                this.child(div().flex_1().min_h_0().child(self.dialog.clone()))
            })
            .when(!show_table && !show_dialog, |this| {
                this.child(self.render_placeholder(cx))
            })
    }
}
