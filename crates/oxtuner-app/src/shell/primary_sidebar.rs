//! The primary sidebar: project identity, recent folders, navigation.
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
//! A split bar (a drag strip per column, the sidebar reaching the window's top
//! edge) is still available via [`super::titlebar::DragTop`] and is what the
//! Settings window uses.

use std::{collections::HashMap, path::PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::{TreeEntry, tree};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Entity, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div, px,
};
use oxtuner_core::{MenuNode, MenuNodeKind};

use super::explorer::{self, Explorer};
use crate::{core::CoreService, i18n::t, project, settings::preferences::AppSettings};

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
        // The sidebar names what the core actually owns, not the last path the
        // user picked: a failed open must not look like an open project.
        let status = CoreService::status(cx);
        let project = status.project.as_ref().map(|project| project.root.clone());
        let has_definition = status.definition().is_some();
        let has_project = project.is_some();
        let recent: Vec<PathBuf> = cx
            .global::<AppSettings>()
            .settings()
            .recent_projects()
            .iter()
            .take(5)
            .cloned()
            .collect();

        let explorer = self.explorer.read(cx);
        let state = explorer.tree().clone();
        let nodes = explorer.nodes();
        let has_menu = explorer.has_menu();

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .pt_2()
            .child(section_label(t!("app.primary_sidebar.project"), cx))
            .child(
                v_flex()
                    .px_3()
                    .pt_1()
                    .gap_0p5()
                    .when_some(project, |this, path| {
                        this.child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(project::display_name(&path)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(path.display().to_string()),
                        )
                    })
                    .when(!has_project, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("app.primary_sidebar.no_project")),
                        )
                    }),
            )
            .when(!recent.is_empty(), |this| {
                this.child(section_label(t!("app.primary_sidebar.recent"), cx))
                    .child(v_flex().px_2().pt_1().gap_0p5().children(
                        recent.into_iter().enumerate().map(|(index, path)| {
                            Button::new(("recent-project", index))
                                .ghost()
                                .small()
                                .label(project::display_name(&path))
                                .w_full()
                                .on_click(move |_, _, cx| project::open(path.clone(), cx))
                        }),
                    ))
            })
            .child(section_label(t!("app.primary_sidebar.navigation"), cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pt_1()
                    .when(!has_definition, |this| {
                        this.child(empty_state(t!("app.primary_sidebar.no_definition"), cx))
                    })
                    .when(has_definition && !has_menu, |this| {
                        this.child(empty_state(t!("app.primary_sidebar.no_menu"), cx))
                    })
                    .when(has_menu, |this| {
                        this.child(tree(&state, move |_ix, entry, selected, _window, cx| {
                            tree_row(entry, selected, &nodes, cx)
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
fn tree_row(
    entry: &TreeEntry,
    selected: bool,
    nodes: &HashMap<String, MenuNode>,
    cx: &App,
) -> ListItem {
    let node = nodes.get(entry.item().id.as_ref());
    let icon = if entry.is_folder() {
        if entry.is_expanded() {
            IconName::FolderOpen
        } else {
            IconName::Folder
        }
    } else {
        node.map_or(IconName::File, |node| explorer::icon(node.kind))
    };
    let missing = node.is_some_and(|node| node.kind == MenuNodeKind::Missing);
    let detail = node.and_then(explorer::detail_label);
    let muted = cx.theme().muted_foreground;

    ListItem::new(entry.item().id.clone())
        .selected(selected)
        .pl(px(8.0 + entry.depth() as f32 * 14.0))
        .accessibility_label(entry.item().label.clone())
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(Icon::new(icon).small().text_color(muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
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
                }),
        )
}
