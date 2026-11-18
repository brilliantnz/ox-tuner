//! The footer buttons for the current step.

use std::path::Path;
use std::rc::Rc;

use gpui_kit::base::h_flex;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, WindowExt as _};
use gpui_kit::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _};
use oxtuner_core::FailureCode;
use rust_i18n::t;

use super::actions::create;
use super::name_step::advance_from_name;
use super::state::{SharedWizard, Step};
use crate::core::CoreService;

/// The footer's inputs, grouped so the button rules read once.
#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent rule the footer applies"
)]
pub(super) struct FooterState {
    pub(super) step: Step,
    pub(super) created: bool,
    pub(super) creating: bool,
    pub(super) retry: bool,
    pub(super) can_advance: bool,
}

/// The create failure that belongs to this wizard, if one arrived.
///
/// A failure from before Create was pressed is not this create's answer, which
/// is what the serial comparison decides.
pub(super) fn creation_failure(
    creation_started: bool,
    creating: bool,
    failure_serial: u64,
    cx: &App,
) -> Option<String> {
    if creating || !creation_started {
        return None;
    }

    let status = CoreService::status(cx);
    if status.failure_serial <= failure_serial {
        return None;
    }

    status
        .failure
        .as_ref()
        .filter(|failure| failure.code == FailureCode::ProjectRejected)
        .map(|failure| failure.message.clone())
}

/// The footer buttons for the current step.
pub(super) fn footer(
    model: &SharedWizard,
    state: FooterState,
    name: &str,
    parent: &Path,
) -> AnyElement {
    let FooterState {
        step,
        created,
        creating,
        retry,
        can_advance,
    } = state;
    let can_leave = !creating;

    let cancel = if created {
        Button::new("new-project-done")
            .label(t!("app.new_project.done"))
            .primary()
            .on_click(|_, window, cx| window.close_dialog(cx))
    } else {
        Button::new("new-project-cancel")
            .label(t!("app.new_project.cancel"))
            .outline()
            .disabled(!can_leave)
            .on_click(|_, window, cx| window.close_dialog(cx))
    };

    let mut buttons = h_flex().gap_2();

    if created {
        // Done alone sits on the left, where the other steps keep Cancel.
        return h_flex().justify_between().child(cancel).into_any_element();
    }

    let back_model = Rc::clone(model);
    buttons = buttons.child(
        Button::new("new-project-back")
            .label(t!("app.new_project.back"))
            .outline()
            .disabled(!can_leave || step == Step::Name)
            .on_click(move |_, _, cx| {
                let previous = match back_model.borrow().step {
                    Step::Review => Some(Step::Connection),
                    Step::Connection => Some(Step::Configuration),
                    Step::Configuration => Some(Step::Definition),
                    Step::Definition => Some(Step::Name),
                    Step::Name => None,
                };
                if let Some(previous) = previous {
                    let mut wizard = back_model.borrow_mut();
                    wizard.step = previous;
                    // A failure belongs to the request that produced it; going
                    // back to change the inputs starts a new attempt.
                    wizard.retarget();
                    drop(wizard);
                    cx.refresh_windows();
                }
            }),
    );

    if step == Step::Review {
        let create_model = Rc::clone(model);
        let parent = parent.to_path_buf();
        let name = name.to_owned();
        buttons = buttons.child(
            Button::new("new-project-create")
                .label(if retry {
                    t!("app.new_project.retry")
                } else {
                    t!("app.new_project.create")
                })
                .primary()
                .disabled(creating || !can_advance)
                .on_click(move |_, _, cx| {
                    create(&create_model, &parent, &name, cx);
                }),
        );
    } else {
        let next_model = Rc::clone(model);
        let name_source = name.to_owned();
        let parent = parent.to_path_buf();
        buttons = buttons.child(
            Button::new("new-project-next")
                .label(t!("app.new_project.next"))
                .primary()
                .disabled(!can_advance || creating)
                .on_click(move |_, _, cx| {
                    let step = next_model.borrow().step;
                    if step == Step::Name {
                        advance_from_name(&next_model, &name_source, &parent, cx);
                        return;
                    }

                    let next = match step {
                        Step::Definition => Step::Configuration,
                        Step::Configuration => Step::Connection,
                        Step::Connection | Step::Review => Step::Review,
                        Step::Name => return,
                    };
                    next_model.borrow_mut().step = next;
                    cx.refresh_windows();
                }),
        );
    }

    // Cancel stays on the left; Back and the step's forward action sit
    // together on the right.
    h_flex()
        .justify_between()
        .child(cancel)
        .child(buttons)
        .into_any_element()
}
