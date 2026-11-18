//! Sends the create once the wizard has everything it needs.

use std::path::Path;

use gpui_kit::App;
use oxtuner_core::{ConnectionSeed, CreateRequest};

use super::connection_step::effective_connection;
use super::name_step::name_target_problem;
use super::state::{DefinitionMode, SharedWizard};
use crate::core::{self, CoreService};
use crate::state::session::AppState;

/// Sends the create and remembers what the wizard is waiting for.
///
/// Only the active source counts: detection's answer in Auto, the picked file
/// in Manual. The port and baud always come from the connection step's
/// pickers, which default to the detection while the user has not chosen.
pub(super) fn create(model: &SharedWizard, parent: &Path, name: &str, cx: &mut App) {
    // The target is checked again here: the folder can appear between the
    // Next click that validated it and this one.
    if let Some(problem) = name_target_problem(name, parent) {
        model.borrow_mut().name_error = Some(problem);
        cx.refresh_windows();
        return;
    }

    let (mode, settings, picked) = {
        let wizard = model.borrow();
        (
            wizard.definition_mode,
            wizard
                .choices
                .values()
                .filter_map(Clone::clone)
                .collect::<Vec<String>>(),
            wizard.definition.clone(),
        )
    };

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
    // freshest proof of what the ECU reports.
    let expected_signature = status
        .test
        .as_ref()
        .and_then(|test| test.signature_for(port.as_deref()))
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

    let request = CreateRequest {
        parent: parent.to_path_buf(),
        name: name.to_owned(),
        ini_path: definition,
        expected_signature,
        settings,
        connection: port.map(|port| ConnectionSeed { port, baud }),
        layout: cx.global::<AppState>().data().layout.to_project_layout(),
    };

    {
        let mut wizard = model.borrow_mut();
        wizard.target = Some(request.root());
        wizard.failure_serial = failure_serial;
    }

    core::create_project(request, cx);
}
