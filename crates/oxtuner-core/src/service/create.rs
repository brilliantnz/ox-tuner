//! A project created from a controller's tune.
//!
//! The manual path scaffolds from the definition's defaults. This path is the
//! other one: parse the definition, connect on the wizard's port, verify the
//! signature, read every page, seed the scaffold with that image, write the
//! project and open it. The read runs through the same [`Live`] connection and
//! [`PendingKind`] table as an ordinary connect, so the core's one-owner-of-the-
//! port rule holds and the UI can cancel by closing the session.
//!
//! A failure never falls back to the definition's defaults: a project that
//! claims to be created from an ECU must hold the ECU's values.

#![allow(
    clippy::wildcard_imports,
    reason = "the service modules are one implementation split across files"
)]

use std::path::Path;

use oxtuner_project::{CreateRequest, PreparedProject, Project};

use super::dispatch::{CacheSource, adopt_project, reject_project};
use super::ecu::default_framing;
use super::*;

/// What stage the create read is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    /// The signature reply has not arrived yet.
    Signature,
    /// The signature matched and every page was requested.
    Pages,
    /// The pages decoded, so the project can be written.
    Ready,
}

/// A create from a controller that is in flight.
#[derive(Debug)]
pub(super) struct PendingCreate {
    op: OpId,
    prepared: PreparedProject,
    stage: Stage,
}

impl PendingCreate {
    /// Whether the read finished and the project can be written.
    pub(super) fn is_ready(&self) -> bool {
        self.stage == Stage::Ready
    }
}

/// Validates the request, starts the link and asks for the signature.
///
/// Returns the pending create when the handshake was queued. A refused request
/// answers its op with `ProjectRejected` and leaves the session untouched.
pub(super) fn begin(
    op: OpId,
    mut request: CreateRequest,
    link: &PortChoice,
    live: &mut Option<Live>,
    events: &EventTx,
    report: &mut CoreReport,
) -> Option<PendingCreate> {
    if live.is_some() {
        reject(
            op,
            "a connection is already open; disconnect before creating a project from the ECU",
            report,
            events,
        );
        return None;
    }

    // The wizard's picked port is the one whose page image is read. Remember
    // it before preparing: `PreparedProject` clones the request, and the
    // write step reads that clone, so injecting it later would be lost.
    remember_connection(&mut request, link);

    let prepared = match Project::prepare(&request) {
        Ok(prepared) => prepared,
        Err(error) => {
            reject_project(&request.root(), &error, op, report, events);
            return None;
        }
    };

    let definition = prepared.definition();
    let Some(behavior) = BehaviorKind::select(definition) else {
        reject(
            op,
            &format!(
                "no ECU behavior claims `{}`; its message set is not one this build knows",
                definition.signature
            ),
            report,
            events,
        );
        return None;
    };
    let framing = default_framing(definition);
    let plan = IoPlan::definition(Box::new(definition.clone()), behavior, None, link.clone());
    let (thread, replies) = match IoThread::start(&plan) {
        Ok(pair) => pair,
        Err(error) => {
            reject(op, &error.to_string(), report, events);
            return None;
        }
    };

    let mut connection = Live::new(
        thread,
        replies,
        link,
        definition,
        behavior,
        framing,
        Some(Box::new(prepared.new_image())),
    );
    // Creation reads one image and writes a file; polling would interleave
    // realtime replies with the pages on the wire.
    connection.connection.auto_poll = false;

    if let Err(error) = connection.request(
        op,
        PendingKind::CreateSignature,
        IoOp::Signature { timeout: None },
    ) {
        reject(op, &error.to_string(), report, events);
        return None;
    }

    *live = Some(connection);

    Some(PendingCreate {
        op,
        prepared,
        stage: Stage::Signature,
    })
}

/// Records the signature reply and, when it matches, queues the page read.
///
/// A mismatch is a create failure, not a read-only session: the definition the
/// wizard resolved does not describe this controller.
pub(super) fn signature_reached(
    create: &mut PendingCreate,
    live: &mut Live,
    signature: String,
) -> Result<(), String> {
    live.set_signature(signature);
    if live.connection.verdict != SignatureVerdict::Match {
        return Err(format!(
            "the ECU reports `{}`, but the definition declares `{}`",
            live.connection.signature.trim(),
            live.connection.definition_signature.trim()
        ));
    }

    create.stage = Stage::Pages;
    live.request(create.op, PendingKind::CreateRead, IoOp::ReadAll)
        .map_err(|error| error.to_string())
}

/// Decodes every page of a create read into the connection's ECU image.
pub(super) fn pages_reached(
    create: &mut PendingCreate,
    live: &mut Live,
    pages: &[PageRead],
) -> Result<(), String> {
    let tune = live
        .images
        .standalone
        .as_mut()
        .ok_or_else(|| "the create connection has no ECU image".to_owned())?;
    let layout = Arc::clone(tune.layout());

    for read in pages {
        let page = IniPage::new(read.page)
            .ok_or_else(|| format!("page {} is not a valid page number", read.page))?;
        decode_page(&layout, page, &read.bytes, tune).map_err(|error| error.to_string())?;
    }

    create.stage = Stage::Ready;
    Ok(())
}

/// Answers a failed create read and drops the pending state.
///
/// The caller tears the connection down; the op is answered first so a stuck
/// port cannot swallow the reason.
pub(super) fn fail(
    pending_create: &mut Option<PendingCreate>,
    message: &str,
    events: &EventTx,
    report: &mut CoreReport,
) {
    let Some(create) = pending_create.take() else {
        return;
    };

    reject(create.op, message, report, events);
}

/// Writes the project from the page image and opens it.
///
/// The connection is torn down by [`adopt_project`] on success and explicitly
/// on failure, so the port is released either way.
#[expect(
    clippy::too_many_arguments,
    reason = "the write completes the command that owns every session resource"
)]
pub(super) fn finish(
    create: PendingCreate,
    live: &mut Option<Live>,
    session: &mut Session,
    safety: &mut Safety,
    journal_dir: Option<&Path>,
    events: &EventTx,
    latest: &LatestFrame,
    history: &ChartHistory,
    log: &LogStatus,
    report: &mut CoreReport,
    last_menu: &mut Vec<MenuNode>,
) {
    let seed = live
        .as_ref()
        .and_then(|connection| connection.images.standalone.as_deref());
    let root = create.prepared.root().to_path_buf();

    match create
        .prepared
        .create(seed, oxtuner_project::WritePolicy::Full)
    {
        Ok(project) => adopt_project(
            project,
            create.op,
            CacheSource::Fresh,
            session,
            live,
            safety,
            journal_dir,
            events,
            latest,
            history,
            log,
            report,
            last_menu,
        ),
        Err(error) => {
            teardown(
                live,
                session,
                safety,
                journal_dir,
                latest,
                events,
                history,
                log,
            );
            reject_project(&root, &error, create.op, report, events);
        }
    }
}

/// Answers a create with a readable reason and counts the failure.
fn reject(op: OpId, message: &str, report: &mut CoreReport, events: &EventTx) {
    tracing::warn!(%op, %message, "project creation from the ECU failed");
    report.failures += 1;
    send(
        events,
        Event::Failed {
            op,
            failure: Failure {
                code: FailureCode::ProjectRejected,
                message: message.to_owned(),
            },
        },
    );
}

/// Fills in the serial port a create-from-ECU should remember.
///
/// The wizard normally supplies it, but a caller that passes only the link
/// should still get a project that reconnects to the same controller. A custom
/// link has no serial port to store.
fn remember_connection(request: &mut CreateRequest, link: &PortChoice) {
    if request.connection.is_none()
        && let PortChoice::Serial { port, baud } = link
    {
        request.connection = Some(oxtuner_project::ConnectionSeed {
            port: port.clone(),
            baud: *baud,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use oxtuner_project::ProjectLayout;

    use super::*;

    fn request() -> CreateRequest {
        CreateRequest {
            parent: PathBuf::from("/tmp/oxtuner-create-test"),
            name: "Roadster".to_owned(),
            ini_path: PathBuf::from("speeduino.ini"),
            expected_signature: None,
            settings: Vec::new(),
            connection: None,
            layout: ProjectLayout::default(),
        }
    }

    #[test]
    fn a_serial_create_remembers_the_picked_port() {
        let mut request = request();
        remember_connection(&mut request, &PortChoice::serial("/dev/cu.test", 115_200));

        let connection = request.connection.expect("the port is remembered");
        assert_eq!(connection.port, "/dev/cu.test");
        assert_eq!(connection.baud, 115_200);
    }

    #[test]
    fn an_explicit_connection_is_not_overwritten() {
        let mut request = request();
        request.connection = Some(oxtuner_project::ConnectionSeed {
            port: "/dev/cu.explicit".to_owned(),
            baud: 57_600,
        });

        remember_connection(&mut request, &PortChoice::serial("/dev/cu.other", 115_200));

        let connection = request.connection.expect("the port is kept");
        assert_eq!(connection.port, "/dev/cu.explicit");
        assert_eq!(connection.baud, 57_600);
    }
}
