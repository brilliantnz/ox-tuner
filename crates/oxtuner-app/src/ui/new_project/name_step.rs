//! Step 1: the project name and the folder it lives in.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Entity, IntoElement as _, ParentElement as _, PathPromptOptions, Styled as _,
    Window, div,
};
use oxtuner_core::{ensure_target_free, split_destination, validate_project_name};
use rust_i18n::t;

use super::state::SharedWizard;

/// Step 1: the name and the folder.
pub(super) fn name_body(
    model: &SharedWizard,
    name: &Entity<InputState>,
    path: &Entity<InputState>,
    cx: &App,
    creating: bool,
) -> AnyElement {
    let picker_model = std::rc::Rc::clone(model);
    let name = name.clone();
    let path = path.clone();
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    v_flex()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.new_project.name_field_label")),
        )
        .child(Input::new(&name).w_full().disabled(creating))
        .when_some(model.borrow().name_error.clone(), |this, message| {
            this.child(div().text_xs().text_color(danger).child(message))
        })
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.new_project.folder_field_label")),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(Input::new(&path).w_full().disabled(creating))
                .child(
                    Button::new("new-project-choose-folder")
                        .label(t!("app.new_project.choose_folder"))
                        .icon(IconName::FolderOpen)
                        .outline()
                        .disabled(creating)
                        .on_click(move |_, window, cx| {
                            choose_folder(
                                std::rc::Rc::clone(&picker_model),
                                &name,
                                &path,
                                window,
                                cx,
                            );
                        }),
                ),
        )
        .into_any_element()
}

/// Rewrites the path field from the folder and the project name.
fn sync_from_name(
    model: &SharedWizard,
    name: &Entity<InputState>,
    path: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) {
    // Editing the name starts a new attempt: the old refusal no longer holds,
    // and the target it named is not what the field describes any more.
    let failure_serial = crate::core::CoreService::status(cx).failure_serial;
    {
        let mut wizard = model.borrow_mut();
        wizard.name_error = None;
        wizard.retarget(failure_serial);
    }
    let name_text = name.read(cx).value().trim().to_owned();
    let parent = model.borrow().parent.clone();

    path.update(cx, |path, cx| {
        path.set_value(parent.join(&name_text).display().to_string(), window, cx);
    });
}

/// Splits the path the user typed into folder and project name.
///
/// Everything before the last component is the folder; that last component is
/// the project name and follows the name field, so both keep describing one
/// project.
fn sync_from_path(
    model: &SharedWizard,
    path: &Entity<InputState>,
    name: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) {
    let typed = PathBuf::from(path.read(cx).value().trim());
    let (folder, file_name) = split_destination(&typed);

    if let Some(folder) = folder {
        let failure_serial = crate::core::CoreService::status(cx).failure_serial;
        let mut wizard = model.borrow_mut();
        if wizard.parent != folder {
            // The path field describes another folder: the create target and
            // the failure it produced belong to the old one.
            wizard.retarget(failure_serial);
        }
        wizard.parent = folder;
    }

    let current_name = name.read(cx).value().trim().to_owned();
    if let Some(file_name) = file_name.filter(|file_name| *file_name != current_name) {
        name.update(cx, |name, cx| name.set_value(file_name, window, cx));
    }
}

/// Asks for the folder projects are created inside.
fn choose_folder(
    model: SharedWizard,
    name: &Entity<InputState>,
    path: &Entity<InputState>,
    window: &Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    let name = name.clone();
    let path = path.clone();
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(t!("app.new_project.choose_folder").to_string().into()),
    });

    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(folder) = paths.into_iter().next() {
                let failure_serial =
                    cx.update(|cx| crate::core::CoreService::status(cx).failure_serial);
                let mut wizard = model.borrow_mut();
                if wizard.closed {
                    return;
                }
                wizard.parent = folder;
                // A new folder is a new target and therefore a new attempt.
                wizard.retarget(failure_serial);
                drop(wizard);

                // The path field follows the folder, and rewriting an input
                // needs the window the field lives in.
                let _ = handle.update(cx, |_, window, cx| {
                    sync_from_name(&model, &name, &path, window, cx);
                    cx.refresh_windows();
                });
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the folder picker failed"),
        Err(error) => tracing::warn!(%error, "the folder picker channel closed"),
    })
    .detach();
}

/// The reason the name step cannot advance, if any.
///
/// One predicate for the Next button and the click: the button reads it
/// through `is_none`, the click reports it. The same project-crate rules
/// creation applies decide it, so the wizard cannot accept a name or a folder
/// that Create would refuse.
pub(super) fn name_target_problem(name: &str, parent: &Path) -> Option<String> {
    validate_project_name(name)
        .and_then(|name| ensure_target_free(&parent.join(name)))
        .err()
        .map(|error| error.to_string())
}

/// Advances past the name step once the name and the target are usable.
///
/// The name enables the Next button; the target lives on disk, so its check
/// runs here on the click, not on every frame. The same project-crate rules
/// creation applies decide it, so the wizard cannot accept a name or a
/// folder that Create would refuse.
pub(super) fn advance_from_name(model: &SharedWizard, name: &str, parent: &Path, cx: &mut App) {
    match name_target_problem(name, parent) {
        None => {
            let failure_serial = crate::core::CoreService::status(cx).failure_serial;
            let mut wizard = model.borrow_mut();
            wizard.name_error = None;
            // Leaving the step with a new target is a new create attempt.
            wizard.retarget(failure_serial);
            wizard.step = super::state::Step::Definition;
        }
        Some(error) => {
            model.borrow_mut().name_error = Some(error);
        }
    }

    cx.refresh_windows();
}

/// The two fields describe one project: typing in either one updates the
/// other, so the path always shows the folder with the name appended.
///
/// The handles are kept on the model (not detached) so closing the dialog
/// unsubscribes them; otherwise every open would leak a handler pair that
/// keeps the dead model and both inputs alive with the window.
pub(super) fn subscribe_fields(
    model: &SharedWizard,
    name: &Entity<InputState>,
    path: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) {
    let name_subscription = window.subscribe(name, cx, {
        let model = std::rc::Rc::clone(model);
        let path = path.clone();
        move |name, _: &InputEvent, window, cx| sync_from_name(&model, &name, &path, window, cx)
    });

    let path_subscription = window.subscribe(path, cx, {
        let model = std::rc::Rc::clone(model);
        let name = name.clone();
        move |path, _: &InputEvent, window, cx| sync_from_path(&model, &path, &name, window, cx)
    });

    let mut wizard = model.borrow_mut();
    wizard.name_subscription = Some(name_subscription);
    wizard.path_subscription = Some(path_subscription);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_destination_preview_follows_the_name() {
        // The path field and the name field describe one project; the split
        // rule itself belongs to the project crate and is tested there.
        let (folder, name) = split_destination(Path::new("/tmp/OxTunerProjects/Roadster"));

        assert_eq!(
            folder,
            Some(std::path::PathBuf::from("/tmp/OxTunerProjects"))
        );
        assert_eq!(name.as_deref(), Some("Roadster"));
    }

    #[test]
    fn a_name_the_core_refuses_disables_the_step() {
        assert!(validate_project_name("Roadster").is_ok());
        assert!(validate_project_name("a/b").is_err());
        assert!(validate_project_name("").is_err());
    }

    #[test]
    fn an_existing_target_blocks_advance_with_a_reason() {
        let parent =
            std::env::temp_dir().join(format!("oxtuner-wizard-test-{}", std::process::id()));
        std::fs::create_dir_all(&parent).expect("the temp folder is created");
        std::fs::create_dir_all(parent.join("Taken")).expect("the taken folder is created");

        assert!(
            name_target_problem("Roadster", &parent).is_none(),
            "a free target advances"
        );
        let problem = name_target_problem("Taken", &parent);
        assert!(
            problem.is_some(),
            "an existing folder blocks advance with a reason"
        );

        std::fs::remove_dir_all(&parent).ok();
    }
}
