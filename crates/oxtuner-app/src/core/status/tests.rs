#![expect(
    clippy::unwrap_used,
    reason = "unit tests may unwrap; a failed call is a failed test"
)]

use super::*;
use oxtuner_core::ProjectConnection;

fn summary() -> DefinitionView {
    DefinitionView {
        label: "speeduino.ini".to_owned(),
        signature: "speeduino 202501".to_owned(),
        spec_version: Some(3.78),
        pages: 15,
        constants: 817,
        pc_variables: 50,
        channels: 229,
        tables: 21,
        curves: 34,
        gauges: Vec::new(),
        front_page_gauges: Vec::new(),
        key_actions: Vec::new(),
        reference_tables: Vec::new(),
        unsupported: Vec::new(),
        dialogs: 239,
        indicators: Vec::new(),
        menu: Vec::new(),
        coverage_percent: 97.95,
        errors: 0,
        warnings: 0,
        active_flags: Vec::new(),
    }
}

fn project() -> ProjectView {
    ProjectView {
        root: std::path::PathBuf::from("/projects/roadster"),
        tune_path: std::path::PathBuf::from("/projects/roadster/CurrentTune.msq"),
        definition_path: std::path::PathBuf::from(
            "/projects/roadster/projectCfg/mainController.ini",
        ),
        in_project: true,
        definition: summary(),
        read_only: false,
        read_only_reason: None,
        read_only_reasons: Vec::new(),
        loaded: 817,
        skipped: 0,
        warnings: Vec::new(),
        layout: oxtuner_core::ProjectLayout::default(),
        connection: ProjectConnection::default(),
    }
}

fn failure(code: FailureCode) -> Failure {
    Failure {
        code,
        message: "boom".to_owned(),
    }
}

fn dialog() -> DialogView {
    DialogView {
        id: "engine".to_owned(),
        title: "Engine".to_owned(),
        fields: Vec::new(),
        changed: 0,
        writable: true,
        unavailable: Vec::new(),
        topic_help: None,
        layout: None,
        help_links: Vec::new(),
        presets: Vec::new(),
        actions: Vec::new(),
        widgets: Vec::new(),
        tables: Vec::new(),
        curves: Vec::new(),
        unmodeled: Vec::new(),
    }
}

#[test]
fn only_an_edit_or_a_save_moves_the_tune_revision() {
    let mut status = CoreStatus::default();

    status.apply(Update::Dialog {
        dialog: dialog(),
        edited: false,
    });
    assert_eq!(status.tune_revision, 0, "a read is not a change");

    status.apply(Update::Dialog {
        dialog: dialog(),
        edited: true,
    });
    assert_eq!(status.tune_revision, 1);

    status.apply(Update::Saved(SaveReport {
        path: "/projects/roadster/CurrentTune.msq".into(),
        cells: 3,
    }));
    assert_eq!(status.tune_revision, 2);
}

#[test]
fn a_snapshot_marks_and_clears_its_target() {
    let mut status = CoreStatus::default();
    let mut changed = dialog();
    changed.changed = 3;

    status.apply(Update::Dialog {
        dialog: changed,
        edited: true,
    });
    assert!(status.dirty.contains("engine"));

    // Reading the same target again with no changes clears its mark: the
    // file baseline now matches what the definition shows.
    status.apply(Update::Dialog {
        dialog: dialog(),
        edited: true,
    });
    assert!(!status.dirty.contains("engine"));

    status.apply(Update::Dialog {
        dialog: {
            let mut other = dialog();
            other.id = "startup".to_owned();
            other.changed = 1;
            other
        },
        edited: true,
    });
    assert_eq!(status.dirty.len(), 1);

    // Saving rebases the whole tune, so every mark goes.
    status.apply(Update::Saved(SaveReport {
        path: "/projects/roadster/CurrentTune.msq".into(),
        cells: 4,
    }));
    assert!(status.dirty.is_empty());
}

#[test]
fn a_load_replaces_any_previous_failure() {
    let mut status = CoreStatus {
        failure: Some(failure(FailureCode::DefinitionRejected)),
        ..CoreStatus::default()
    };

    status.apply(Update::Loaded(summary()));

    assert!(status.definition.is_some());
    assert_eq!(status.failure, None);
}

#[test]
fn a_project_and_a_definition_are_mutually_exclusive() {
    let mut status = CoreStatus::default();
    status.apply(Update::Loaded(summary()));

    status.apply(Update::Opened(Box::new(project())));

    assert!(status.project.is_some());
    assert!(
        status.definition.is_none(),
        "the project owns the definition now"
    );
    assert_eq!(status.definition().map(|summary| summary.pages), Some(15));
}

#[test]
fn a_save_is_shown_until_the_next_failure() {
    let mut status = CoreStatus::default();
    status.apply(Update::Opened(Box::new(project())));

    status.apply(Update::Saved(SaveReport {
        path: std::path::PathBuf::from("/projects/roadster/CurrentTune.msq"),
        cells: 3,
    }));

    assert_eq!(status.last_save.as_ref().map(|save| save.cells), Some(3));
    assert_eq!(status.failure, None);

    status.apply(Update::Failed(failure(FailureCode::TuneSaveFailed)));

    assert_eq!(
        status.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::TuneSaveFailed)
    );
}

#[test]
fn a_failure_keeps_the_project_that_is_still_open() {
    let mut status = CoreStatus::default();
    status.apply(Update::Opened(Box::new(project())));

    status.apply(Update::Failed(failure(FailureCode::ProjectRejected)));

    assert!(status.project.is_some(), "the old project is still owned");
    assert_eq!(
        status.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::ProjectRejected)
    );
}

fn connection() -> ConnectionView {
    ConnectionView {
        port: "/dev/cu.usbmodem21201".to_owned(),
        custom: false,
        signature: "speeduino 202501".to_owned(),
        definition_signature: "speeduino 202501".to_owned(),
        verdict: oxtuner_core::SignatureVerdict::Match,
        read_only: false,
        read_only_reason: None,
        writes_enabled: true,
        behavior: "speeduino".to_owned(),
        framing: "envelope",
        pages: 15,
        realtime_block: Some(130),
    }
}

#[test]
fn a_connection_replaces_the_previous_one_and_clears_a_loss() {
    let mut status = CoreStatus {
        link_lost: Some("old".to_owned()),
        ..CoreStatus::default()
    };

    status.apply(Update::Connected(connection()));

    assert!(status.connection.is_some());
    assert_eq!(status.link_lost, None);
    assert_eq!(status.failure, None);
}

#[test]
fn a_lost_link_keeps_the_summary_but_stops_polling() {
    let mut status = CoreStatus::default();
    status.apply(Update::Connected(connection()));
    status.apply(Update::Realtime(true));

    status.apply(Update::LinkLost("unplugged".to_owned()));

    assert!(status.connection.is_some(), "the session is still known");
    assert_eq!(status.link_lost.as_deref(), Some("unplugged"));
    assert!(!status.realtime);
}

#[test]
fn a_page_read_moves_the_tune_revision_but_a_disconnect_keeps_the_read() {
    let mut status = CoreStatus::default();
    status.apply(Update::Connected(connection()));

    status.apply(Update::PagesRead(ReadReport {
        pages: 15,
        bytes: 3408,
        constants: 817,
        skipped: 0,
        signature_verified: true,
        conflicts: Vec::new(),
    }));
    assert_eq!(status.tune_revision, 1);
    assert_eq!(status.pages_read.as_ref().map(|read| read.pages), Some(15));

    status.apply(Update::Disconnected);
    assert!(status.connection.is_none());
    assert!(status.pages_read.is_some(), "the read still happened");
}

#[test]
fn a_failed_write_halts_the_path_until_it_is_acknowledged() {
    let mut status = CoreStatus::default();

    status.apply(Update::Failed(failure(FailureCode::WriteFailed)));
    assert!(status.write_halted.is_some());

    status.apply(Update::Acknowledged);
    assert!(status.write_halted.is_none());
}

#[test]
fn the_write_switch_is_tracked_independently_of_the_connection() {
    let mut status = CoreStatus::default();
    assert!(!status.writes_enabled);

    status.apply(Update::WriteAccess {
        enabled: true,
        connection: None,
    });

    assert!(status.writes_enabled);
}

#[test]
fn unloading_keeps_the_write_switch() {
    let mut status = CoreStatus::default();
    status.apply(Update::WriteAccess {
        enabled: true,
        connection: None,
    });

    status.apply(Update::Unloaded);

    assert!(
        status.writes_enabled,
        "the global switch is not project state"
    );
}

#[test]
fn a_bad_import_is_not_halted() {
    let mut status = CoreStatus::default();

    status.apply(Update::Failed(failure(FailureCode::ImportRejected)));

    assert!(status.write_halted.is_none());
}

#[test]
fn unloading_drops_the_project_but_keeps_machine_state() {
    let mut status = CoreStatus {
        ports: vec![PortView {
            name: "/dev/cu.usbmodem".to_owned(),
            label: "/dev/cu.usbmodem".to_owned(),
            vid: None,
            pid: None,
            serial: None,
        }],
        ports_revision: 3,
        ..CoreStatus::default()
    };
    status.apply(Update::Opened(Box::new(project())));
    status.apply(Update::Failed(failure(FailureCode::ProjectRejected)));

    status.apply(Update::Unloaded);

    assert!(status.project.is_none());
    assert!(status.definition.is_none());
    assert!(status.last_save.is_none());
    assert!(status.failure.is_none());
    assert_eq!(status.ports_revision, 3);
    assert_eq!(
        status
            .ports
            .iter()
            .map(|port| port.name.as_str())
            .collect::<Vec<_>>(),
        ["/dev/cu.usbmodem"],
        "the sampled port list is machine state, not project state"
    );
}

#[test]
fn creating_marks_the_folder_pending_with_its_own_kind() {
    let mut status = CoreStatus::default();

    status.begin_pending_creation(Path::new("/projects/roadster"));
    let pending = status.pending_project.as_ref().expect("pending");
    assert_eq!(pending.kind, PendingProjectKind::Create);
    assert_eq!(pending.name.as_str(), "roadster");

    // An open afterwards replaces it, and the splash word follows the kind.
    status.begin_pending_project(Path::new("/projects/other"));
    assert_eq!(
        status.pending_project.as_ref().map(|pending| pending.kind),
        Some(PendingProjectKind::Open)
    );
}

#[test]
fn opening_marks_the_folder_pending_until_its_own_answer_arrives() {
    let mut status = CoreStatus::default();

    status.begin_pending_project(Path::new("/projects/roadster"));
    assert_eq!(
        status
            .pending_project
            .as_ref()
            .map(|pending| pending.name.as_str()),
        Some("roadster")
    );
    assert_eq!(
        status
            .pending_project
            .as_ref()
            .and_then(|pending| pending.op),
        None
    );

    status.pending_project.as_mut().unwrap().op = Some(7);
    assert_eq!(
        status
            .pending_project
            .as_ref()
            .and_then(|pending| pending.op),
        Some(7)
    );

    status.apply(Update::Opened(Box::new(project())));
    assert!(
        status.finish_pending_project_at(Some(7)),
        "the request's own answer ends the loading state"
    );
    assert!(status.pending_project.is_none());
    assert!(!status.project_missing);
}

#[test]
fn a_stale_answer_does_not_clear_a_newer_request() {
    let mut status = CoreStatus::default();
    status.begin_pending_project(Path::new("/projects/old"));
    status.pending_project.as_mut().unwrap().op = Some(7);
    status.begin_pending_project(Path::new("/projects/new"));
    status.pending_project.as_mut().unwrap().op = Some(8);

    assert!(
        !status.finish_pending_project_at(Some(7)),
        "the older request finishing must not clear the newer one"
    );
    assert_eq!(
        status
            .pending_project
            .as_ref()
            .and_then(|pending| pending.op),
        Some(8)
    );

    assert!(status.finish_pending_project_at(Some(8)));
    assert!(status.pending_project.is_none());
}

#[test]
fn a_refused_command_cancels_the_loading_state() {
    let mut status = CoreStatus::default();
    status.begin_pending_project(Path::new("/projects/roadster"));

    status.cancel_pending_project();

    assert!(
        status.pending_project.is_none(),
        "a send failure must never leave the splash up"
    );
}

#[test]
fn a_failed_open_clears_the_loading_state_but_keeps_the_old_project() {
    let mut status = CoreStatus::default();
    status.apply(Update::Opened(Box::new(project())));
    status.begin_pending_project(Path::new("/projects/other"));
    status.pending_project.as_mut().unwrap().op = Some(9);
    status.apply(Update::Failed(failure(FailureCode::ProjectRejected)));

    assert!(status.finish_pending_project_at(Some(9)));
    assert!(status.pending_project.is_none());
    assert!(
        status.project.is_some(),
        "the project that was already open is still owned"
    );
    assert!(status.failure.is_some());
}

#[test]
fn a_lost_open_answer_expires_only_its_own_request() {
    let mut status = CoreStatus::default();
    status.begin_pending_project(Path::new("/projects/roadster"));
    status.pending_project.as_mut().unwrap().op = Some(7);

    assert!(
        !status.expire_pending_project_at(8),
        "another request's timeout must not clear this one"
    );
    assert!(status.pending_project.is_some());

    assert!(status.expire_pending_project_at(7));
    assert!(status.pending_project.is_none());
}
