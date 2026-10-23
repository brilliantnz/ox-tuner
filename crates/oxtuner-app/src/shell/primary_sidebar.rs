//! The primary sidebar: navigation.
//!
//! It starts **below** the main bar — the bar spans the whole window and carries
//! the traffic lights, the menus and the panel toggles — so this column draws no
//! title strip of its own. Unlike the secondary sidebar it also has no header
//! row: navigation is a plain list of sections, and the panel toggle in the bar
//! is the one control it needs.
//!
//! A split bar (a drag strip per column, the sidebar reaching the window's top
//! edge) is still available via [`super::titlebar::DragTop`] and is what the
//! Settings window uses.
//!
//! The dock workspace (see `docs/architecture/ui/`) fills this column with the
//! project and INI tree once the core lands, so today it is its placeholder
//! rather than a second design.

use std::path::{Path, PathBuf};

use gpui_kit::base::v_flex;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use crate::{i18n::t, project, settings::preferences::AppSettings, state::session::AppState};

/// The navigation column.
#[derive(IntoElement)]
pub(crate) struct PrimarySidebar;

impl RenderOnce for PrimarySidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let project = cx
            .global::<AppState>()
            .last_project()
            .map(Path::to_path_buf);
        let recent: Vec<PathBuf> = cx
            .global::<AppSettings>()
            .settings()
            .recent_projects()
            .iter()
            .take(5)
            .cloned()
            .collect();
        let has_project = project.is_some();

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
