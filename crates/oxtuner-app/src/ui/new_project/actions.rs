//! Sends the create once the wizard has everything it needs.

use std::path::Path;

use gpui_kit::App;
use oxtuner_core::CreateRequest;

use super::connection_step::effective_connection;
use super::state::{DefinitionMode, SharedWizard};
use crate::core::{self, CoreService};

/// Sends the create and remembers what the wizard is waiting for.
///
/// Only the active source counts: detection's answer in Auto, the picked file
/// in Manual. The port and baud always come from the connection step's
/// pickers, which default to the detection while the user has not chosen.
pub(super) fn create(model: &SharedWizard, parent: &Path, name: &str, cx: &mut App) {
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
        return;
    };

    let expected_signature = detected
        .as_ref()
        .filter(|detected| {
            // The signature only vouches for the definition on the port it was
            // probed on. A different pick means an unverified pairing: drop
            // the expectation here and let first connect verify it instead.
            detected.definition.as_deref() == Some(definition.as_path())
                && Some(detected.port.as_str()) == port.as_deref()
        })
        .map(|detected| detected.signature.clone());
    let failure_serial = status.failure_serial;

    let request = CreateRequest {
        parent: parent.to_path_buf(),
        name: name.to_owned(),
        ini_path: definition,
        expected_signature,
        settings,
        port,
        baud,
    };

    {
        let mut wizard = model.borrow_mut();
        wizard.target = Some(request.root());
        wizard.failure_serial = failure_serial;
    }

    core::create_project(request, cx);
}
