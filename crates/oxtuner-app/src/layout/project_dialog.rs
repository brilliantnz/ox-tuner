//! The Projects dialog: what is open, what was open, what to open next.
//!
//! Project handling lives here and nowhere else: the primary sidebar is for
//! navigation and the editor column is for the selected node. The title bar
//! names the open project; this dialog is where it is changed.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FontWeight, IntoElement as _, ParentElement as _, Styled as _, Window, div, px,
};

use crate::core::CoreService;
use crate::{i18n::t, project, settings::preferences::AppSettings};

/// Opens the Projects dialog, or focuses it when it is already open.
///
/// A plain dialog: the action buttons close it themselves after running, and
/// the standard close button and Escape dismiss it without doing anything.
pub(crate) fn show(window: &mut Window, cx: &mut App) {
    // The dialog is a lifecycle boundary: a folder moved since the project
    // opened is noticed here, not by a `stat` in the editor's render.
    crate::core::refresh_project_availability(cx);

    let status = CoreService::status(cx).clone();
    let recent: Vec<std::path::PathBuf> = cx
        .global::<AppSettings>()
        .settings()
        .recent_projects()
        .to_vec();
    let auto_reopen = AppSettings::auto_reopen(cx);

    crate::app::dialog::open(window, cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;

        let current = match status.project.as_ref() {
            Some(project) => v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(project::display_name(&project.root)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(project.root.display().to_string()),
                )
                .when(project.read_only, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(danger)
                            .child(t!("app.project.read_only")),
                    )
                }),
            None => v_flex().child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t!("app.project.none_open")),
            ),
        };

        let recent_list =
            if recent.is_empty() {
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t!("app.project.no_recent"))
                    .into_any_element()
            } else {
                v_flex()
                    .gap_1()
                    .children(recent.clone().into_iter().take(8).enumerate().map(
                        |(index, path)| {
                            let label = project::display_name(&path);
                            let detail = path.display().to_string();
                            Button::new(("project-recent", index))
                                .label(label)
                                .tooltip(detail)
                                .on_click(move |_, window, cx| {
                                    project::open(path.clone(), cx);
                                    window.close_dialog(cx);
                                })
                                .into_any_element()
                        },
                    ))
                    .into_any_element()
            };

        dialog
            .title(t!("app.project.title"))
            .width(px(480.))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(muted)
                                    .child(t!("app.project.current")),
                            )
                            .child(current),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(muted)
                                    .child(t!("app.project.recent")),
                            )
                            .child(recent_list),
                    )
                    .child(div().text_xs().text_color(muted).child(if auto_reopen {
                        t!("app.project.auto_reopen_on")
                    } else {
                        t!("app.project.auto_reopen_off")
                    })),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("project-close-current")
                            .label(t!("app.project.close_project"))
                            .disabled(status.project.is_none())
                            .on_click(|_, window, cx| {
                                crate::core::close_project(cx);
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("project-choose-folder")
                            .icon(IconName::FolderOpen)
                            .label(t!("app.project.open_folder"))
                            .primary()
                            .on_click(|_, window, cx| {
                                project::choose(cx);
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}
