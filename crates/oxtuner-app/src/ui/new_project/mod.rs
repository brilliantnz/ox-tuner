//! The New Project wizard.
//!
//! Five steps in one modal: where the project lives, which definition the ECU
//! is read with, the definition's settings, the detected connection, and a
//! review before the core writes anything.
//!
//! Detection is a scan, not a connection test: the app asks the core for one
//! family probe at a time across the scanned ports. A signature is resolved by
//! the registry; a cached definition is used at once, and a missing one waits
//! for the user's consent before a background download. Manual upload stays
//! available when detection finds nothing.
//!
//! Each step lives in its own module; this one only opens the dialog and picks
//! the body and footer for the current step.

mod actions;
mod configuration_step;
mod connection_step;
mod definition_step;
mod footer;
mod name_step;
mod review_step;
mod shared;
mod state;

use std::rc::Rc;

use gpui_kit::base::v_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, AppContext as _, Entity, ParentElement as _, Styled as _, Window, div, px};
use oxtuner_core::{FailureCode, validate_project_name};
use rust_i18n::t;

use self::configuration_step::{choices, configuration_body, ensure_pickers};
use self::connection_step::{
    connection_body, connection_type, effective_connection, sync_connection_pickers,
};
use self::definition_step::definition_body;
use self::footer::{FooterState, creation_failure, footer};
use self::name_step::{name_body, subscribe_fields};
use self::review_step::{review_body, success_body};
use self::state::{ConnectionType, DEFAULT_NAME, SharedWizard, Step, Wizard, effective_definition};
use crate::core::{CoreService, firmware};

/// Opens the New Project wizard, unless a dialog is already up.
pub(crate) fn show(window: &mut Window, cx: &mut App) {
    // The open call below no-ops when another modal is up, so every side
    // effect here must be guarded the same way: otherwise a stray menu or
    // shortcut activation would wipe another dialog's detection state and
    // leak subscriptions for a wizard that never opens.
    if window.has_active_dialog(cx) {
        return;
    }

    // Detection state lives in the core status, not in the dialog, so every
    // open starts clean: otherwise the next open shows the previous session.
    firmware::reset_detection(cx);

    let model: SharedWizard = Rc::new(std::cell::RefCell::new(Wizard::new(cx)));

    let name = cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_NAME));
    let path = {
        let parent = model.borrow().parent.clone();
        cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(parent.join(DEFAULT_NAME).display().to_string())
        })
    };

    subscribe_fields(&model, &name, &path, window, cx);

    // The dialog renders the core's pushed port list; no scan is requested
    // here because the core publishes its first sample on its own.
    let render_model = Rc::clone(&model);
    let close_model = Rc::clone(&model);
    let opened = crate::app::dialog::open(window, cx, move |dialog, window, cx| {
        // Cloned per open: the inner callback takes ownership, so the outer
        // builder stays a plain Fn.
        let close_model = Rc::clone(&close_model);
        render(dialog, &render_model, &name, &path, window, cx).on_close(move |_, _, cx| {
            // Dropping the handles unsubscribes the field sync; leaving
            // them would leak a handler pair per open. The flag keeps a late
            // picker callback from mutating a dead wizard.
            close_model.borrow_mut().closed = true;
            drop(close_model.borrow_mut().name_subscription.take());
            drop(close_model.borrow_mut().path_subscription.take());
            firmware::reset_detection(cx);
            if cx.has_global::<crate::core::CoreService>() {
                crate::core::CoreService::status_mut(cx).dialog_owner = None;
            }
            crate::layout::write_dialogs::retry_pending_review(cx);
        })
    });
    if opened {
        crate::core::CoreService::status_mut(cx).dialog_owner =
            Some(crate::core::DialogOwner::Wizard);
    }
}

/// One frame of the wizard.
#[expect(
    clippy::too_many_lines,
    reason = "the dialog builds its step bodies and footer in one place"
)]
fn render(
    dialog: Dialog,
    model: &SharedWizard,
    name: &Entity<InputState>,
    path: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) -> Dialog {
    let (step, mode, parent, picked_definition, definition_error, target, failure_serial) = {
        let wizard = model.borrow();
        (
            wizard.step,
            wizard.definition_mode,
            wizard.parent.clone(),
            wizard.definition.clone(),
            wizard.definition_error.clone(),
            wizard.target.clone(),
            wizard.failure_serial,
        )
    };
    // Reads are cheap shared borrows; the clones below are the only
    // per-frame allocations, and each feeds a call that needs `cx` mutably
    // while a borrow of it would still be live. An unsampled port list reads
    // as empty here on purpose: the pickers rebuild when a snapshot arrives,
    // and only the dialog's empty text tells the two states apart.
    let (definition, preview, ports, detected) = {
        let status = CoreService::status(cx);
        // The active source only: detection's answer in Auto, the picked file
        // in Manual. Toggling the mode never leaks the other side in.
        let definition = effective_definition(
            mode,
            picked_definition,
            status
                .detected_firmware
                .as_ref()
                .and_then(|detected| detected.definition.clone()),
        );
        // The preview belongs to the definition currently picked: a late
        // answer for an earlier file is ignored instead of filling the wrong
        // dialog.
        let preview = status
            .inspected
            .as_ref()
            .filter(|inspected| definition.as_deref() == Some(inspected.path.as_path()))
            .map(|inspected| inspected.preview.clone());
        let ports = status.ports.clone();
        let detected = status
            .detected_firmware
            .as_ref()
            .map(|detected| (detected.port.clone(), detected.baud));
        (definition, preview, ports, detected)
    };

    // The first time a preview arrives, build one picker per group. The
    // pickers own their selection; the model only mirrors it.
    if let Some(preview) = &preview {
        ensure_pickers(model, definition.as_ref(), preview, window, cx);
    }

    sync_connection_pickers(
        model,
        &ports,
        detected.as_ref().map(|(port, baud)| (port.as_str(), *baud)),
        window,
        cx,
    );

    // The bodies only need `cx` shared, so they borrow the status again
    // instead of cloning it a second time.
    let status = CoreService::status(cx);
    let creating = status
        .pending_project
        .as_ref()
        .is_some_and(|pending| pending.kind == crate::core::PendingProjectKind::Create);
    // A running download owns the dialog: no cancel, back, next, close or
    // source change until it succeeds or fails.
    let downloading = status.downloading_definition;
    let created = target.as_ref().is_some_and(|target| {
        status
            .project
            .as_ref()
            .is_some_and(|project| project.root == *target)
    });

    let inspect_error = {
        if preview.is_none()
            && definition.is_some()
            && !creating
            && status.failure_serial > status.inspect_serial
        {
            status
                .failure
                .as_ref()
                .filter(|failure| failure.code == FailureCode::DefinitionRejected)
                .map(|failure| failure.message.clone())
        } else {
            None
        }
    };

    let failure = creation_failure(target.is_some(), creating, failure_serial, cx);
    let name_text = name.read(cx).value().trim().to_owned();
    let definition_ready = definition.is_some();

    let body = if created {
        success_body(target.as_deref(), cx)
    } else {
        match step {
            Step::Name => name_body(model, name, path, cx, creating),
            Step::Definition => definition_body(
                model,
                definition.as_deref(),
                definition_error,
                inspect_error.as_deref(),
                preview.is_some(),
                status,
                cx,
                creating,
            ),
            Step::Configuration => configuration_body(
                model,
                preview.as_ref(),
                inspect_error.as_deref(),
                cx,
                creating,
            ),
            Step::Connection => connection_body(model, status, creating, cx),
            Step::Review => {
                let (port, baud) = effective_connection(model, cx);
                let is_serial = connection_type(model, cx) == ConnectionType::Serial;
                let connection = if is_serial {
                    port.clone().map_or_else(
                        || t!("app.new_project.no_connection").to_string(),
                        |port| {
                            t!(
                                "app.new_project.review_connection_value",
                                port = port,
                                baud = baud
                            )
                            .to_string()
                        },
                    )
                } else {
                    t!("app.connect.mode_simulator").to_string()
                };
                // A port that differs from the probed one voids the
                // signature check at Create; it runs on first connect.
                // Simulator projects carry no port, so the warning only
                // applies to an explicit serial pick.
                let port_mismatch = is_serial
                    && status
                        .detected_firmware
                        .as_ref()
                        .is_some_and(|detected| Some(&detected.port) != port.as_ref());
                review_body(
                    &name_text,
                    &parent,
                    preview
                        .as_ref()
                        .map(|preview| preview.signature.clone())
                        .or_else(|| {
                            definition.as_ref().and_then(|path| {
                                path.file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                            })
                        }),
                    &model.borrow().display_groups,
                    &choices(model, cx),
                    connection,
                    port_mismatch,
                    cx,
                )
            }
        }
    };

    let footer = footer(
        model,
        FooterState {
            step,
            created,
            creating,
            downloading,
            retry: failure.is_some(),
            can_advance: match step {
                // The button checks the cheap half; the target lives on
                // disk, so its check runs on the click, not on every frame.
                Step::Name => validate_project_name(&name_text).is_ok(),
                Step::Definition => definition_ready && preview.is_some(),
                Step::Configuration => preview.is_some(),
                Step::Connection => true,
                // Create carries the definition; without one the button
                // below stays disabled and the review names what is missing.
                Step::Review => definition_ready,
            },
        },
        &name_text,
        &parent,
    );

    dialog
        .title(t!("app.new_project.title"))
        .width(px(560.))
        .close_button(!creating && !downloading)
        .child(
            v_flex()
                .gap_3()
                // The dialog body scrolls inside a clipping viewport whose
                // bottom edge lands exactly on the last row. A focused input
                // paints its ring 3px outside its border, so without room
                // here the bottom of the ring is cut off.
                .pb_1()
                .child(body)
                .when(creating, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("app.new_project.creating")),
                    )
                })
                .when_some(failure, |this, message| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(t!("app.new_project.failed", message = message)),
                    )
                }),
        )
        .footer(footer)
}
