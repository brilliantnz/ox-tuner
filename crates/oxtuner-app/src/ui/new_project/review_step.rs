//! Step 5: what will be written, and the result once it is.

use std::path::Path;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::Button;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _, div};
use rust_i18n::t;

use super::configuration_step::choice_label;
use super::shared::row;
use crate::platform::reveal::show_in_folder;

/// Step 5: what will be written.
#[expect(
    clippy::too_many_arguments,
    reason = "the review reads one part of the wizard and core status each"
)]
pub(super) fn review_body(
    name: &str,
    parent: &Path,
    definition_name: Option<String>,
    groups: &[oxtuner_core::SettingGroupView],
    choices: &std::collections::BTreeMap<String, Option<String>>,
    connection: String,
    port_mismatch: bool,
    cx: &App,
) -> AnyElement {
    let definition_label =
        definition_name.unwrap_or_else(|| t!("app.new_project.definition_none").to_string());

    let mut body = v_flex()
        .gap_2()
        .child(row(
            t!("app.new_project.review_name").to_string(),
            name.to_owned(),
            cx,
        ))
        .child(row(
            t!("app.new_project.review_folder").to_string(),
            parent.join(name.trim()).display().to_string(),
            cx,
        ))
        .child(row(
            t!("app.new_project.review_definition").to_string(),
            definition_label,
            cx,
        ));

    for group in groups {
        body = body.child(row(
            group.label.clone(),
            choice_label(
                group,
                choices
                    .get(&group.name)
                    .and_then(|choice| choice.as_deref()),
            ),
            cx,
        ));
    }

    body = body.child(row(
        t!("app.new_project.review_connection").to_string(),
        connection,
        cx,
    ));

    if port_mismatch {
        body = body.child(
            div()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(t!("app.new_project.review_port_mismatch")),
        );
    }

    body.into_any_element()
}

/// The result panel: the project is open, and here is where it lives.
pub(super) fn success_body(target: Option<&Path>, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .gap_2()
        .child(div().text_sm().child(t!("app.new_project.created")))
        .when_some(target.map(Path::to_path_buf), |this, path| {
            let shown = path.display().to_string();
            this.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().text_xs().text_color(muted).child(shown))
                    .child(
                        Button::new("new-project-show-in-folder")
                            .label(t!("app.new_project.show_in_folder"))
                            .icon(IconName::FolderOpen)
                            .outline()
                            .on_click(move |_, _, _| show_in_folder(&path)),
                    ),
            )
        })
        .into_any_element()
}
