//! Sends the create once the wizard has everything it needs.

use std::path::Path;

use gpui_kit::App;
use oxtuner_core::{ConnectionSeed, CreateRequest};
use rust_i18n::t;

use super::connection_step::{effective_connection, test_matches_pickers};
use super::name_step::name_target_problem;
use super::state::{DefinitionMode, SharedWizard, Step};
use crate::core::{self, CoreService};
use crate::state::session::AppState;

/// Sends the create and remembers what the wizard is waiting for.
///
/// Only the active source counts: detection's answer in Auto, the picked file
/// in Manual. The port and baud always come from the connection step's
/// pickers, which default to the detection while the user has not chosen.
pub(super) fn create(model: &SharedWizard, parent: &Path, name: &str, cx: &mut App) {
    // A create already in flight owns the wizard: a second click that lands
    // before the first repaint must not send another command.
    if CoreService::status(cx).pending_project.is_some() {
        return;
    }

    // The target is checked again here: the folder can appear between the
    // Next click that validated it and this one. A refusal belongs to the
    // Name step, which is the only step that renders it — so go back there
    // instead of stranding the review without an explanation.
    if let Some(problem) = name_target_problem(name, parent) {
        let mut wizard = model.borrow_mut();
        wizard.name_error = Some(problem);
        wizard.step = Step::Name;
        drop(wizard);
        cx.refresh_windows();
        return;
    }

    let (mode, picked) = {
        let wizard = model.borrow();
        (wizard.definition_mode, wizard.definition.clone())
    };
    let settings = super::configuration_step::choices(model, cx)
        .values()
        .filter_map(Clone::clone)
        .collect::<Vec<String>>();

    let status = CoreService::status(cx);
    let detected = status.detected_firmware.clone();
    let (port, baud) = effective_connection(model, cx);
    let definition = match mode {
        DefinitionMode::Auto => detected
            .as_ref()
            .and_then(|detected| detected.definition.clone()),
        DefinitionMode::Manual => picked,
    };
    let Some(definition) = definition else {
        // The review gates Create on a definition, so this only fires when
        // the definition vanished between the frame and the click.
        tracing::warn!("create pressed without a definition; the click is dropped");
        return;
    };

    // A Test run on this connection outranks the detection: it is the
    // freshest proof of what the ECU reports. A test that ran on other pickers
    // proves nothing about this connection, so it is ignored like the step
    // ignores its stale result.
    let test_matches = test_matches_pickers(model, cx);
    let expected_signature = if test_matches {
        status
            .test
            .as_ref()
            .and_then(|test| test.signature_for(port.as_deref()))
    } else {
        None
    }
    .or_else(|| {
        detected
            .as_ref()
            .filter(|detected| {
                // The signature only vouches for the definition on the port it was
                // probed on. A different pick means an unverified pairing: drop
                // the expectation here and let first connect verify it instead.
                detected.definition.as_deref() == Some(definition.as_path())
                    && Some(detected.port.as_str()) == port.as_deref()
            })
            .map(|detected| detected.signature.clone())
    });
    let failure_serial = status.failure_serial;
    let link = port.clone().map(|port| (port, baud));

    let request = CreateRequest {
        parent: parent.to_path_buf(),
        name: name.to_owned(),
        ini_path: definition,
        expected_signature,
        settings,
        connection: port.map(|port| ConnectionSeed { port, baud }),
        layout: cx.global::<AppState>().data().layout.to_project_layout(),
        write_stamp: None,
    };

    {
        let mut wizard = model.borrow_mut();
        wizard.target = Some(request.root());
        wizard.failure_serial = failure_serial;
    }

    // Auto-detect means the tune is the controller's, not the definition's
    // defaults. A missing port is a failed create, never a silent fallback.
    if mode == DefinitionMode::Auto {
        if let Some((port, baud)) = link {
            core::create_project_from_ecu(request, port, baud, cx);
        } else {
            tracing::warn!("create from the ECU pressed without a port");
            core::report_failure(
                cx,
                oxtuner_core::FailureCode::ProjectRejected,
                t!("app.new_project.connect_required").to_string(),
            );
        }
    } else {
        core::create_project(request, cx);
    }
}
