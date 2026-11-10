use super::*;

#[test]
fn a_controller_command_is_refused_while_writes_are_off() {
    let dir = TempDir::new("command-writes-off");
    let core = Harness::start(false);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();

    let (kind, message) = failure(&core.run_command("cmdPing"));

    assert_eq!(kind, FailureCode::ControllerActionRefused);
    assert!(message.contains("writes are disabled"), "{message}");
    assert!(
        core.journal().1.is_empty(),
        "a refused command must not reach the journal"
    );

    core.close();
}

#[test]
fn a_controller_command_marks_the_ecu_image_stale() {
    let dir = TempDir::new("command-stale");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    assert_ne!(core.table().states[0], oxtuner_core::CellState::Unknown);

    let event = core.run_command("cmdPing");

    let Event::ControllerActionRan {
        report: summary, ..
    } = event
    else {
        panic!("expected a controller command, got {event:?}");
    };
    assert_eq!(summary.command, "cmdPing");
    assert_eq!(summary.dialog, "engineDialog");
    assert_eq!(summary.commands, 1);
    assert_eq!(summary.bytes, 1);

    assert_eq!(
        core.table().states[0],
        oxtuner_core::CellState::Unknown,
        "a raw controller action bypasses memory synchronization"
    );

    let (_, entries) = core.journal();
    assert_eq!(entries.len(), 2, "begin and result");
    assert_eq!(entries[0].operation, "controller-command");
    assert_eq!(entries[0].phase, "begin");
    assert_eq!(entries[1].phase, "result");
    assert_eq!(entries[1].result, "ok");

    // A fresh read makes the ECU image authoritative again.
    core.read_all();
    assert_ne!(core.table().states[0], oxtuner_core::CellState::Unknown);

    core.close();
}

#[test]
fn a_controller_command_sends_every_string_in_order() {
    let dir = TempDir::new("command-chain");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();

    let event = core.run_command("cmdChain");

    let Event::ControllerActionRan {
        report: summary, ..
    } = event
    else {
        panic!("expected a controller command, got {event:?}");
    };
    assert_eq!(summary.commands, 2);
    assert_eq!(summary.bytes, 2, "each string is one byte on a legacy link");

    core.close();
}

#[test]
fn a_controller_command_runs_on_the_envelope_link() {
    let dir = TempDir::new("command-envelope");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect_with(SimConfig::default(), Some(oxtuner_core::LinkMode::Envelope));
    core.read_all();

    let event = core.run_command("cmdPing");

    let Event::ControllerActionRan {
        report: summary, ..
    } = event
    else {
        panic!("expected a controller command, got {event:?}");
    };
    assert_eq!(summary.command, "cmdPing");
    assert!(
        summary.bytes > 1,
        "an envelope command carries its frame on the wire"
    );

    core.close();
}

#[test]
fn an_unknown_controller_command_is_refused() {
    let dir = TempDir::new("command-unknown");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();

    let (kind, message) = failure(&core.run_command("cmdMissing"));

    assert_eq!(kind, FailureCode::ControllerActionRefused);
    assert!(message.contains("not declared"), "{message}");

    core.close();
}

#[test]
fn a_controller_command_is_refused_on_a_signature_mismatch() {
    let dir = TempDir::new("command-read-only");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig {
        signature: Some("another firmware".to_owned()),
        ..SimConfig::default()
    });
    core.read_all();

    let (kind, message) = failure(&core.run_command("cmdPing"));

    assert_eq!(kind, FailureCode::ControllerActionRefused);
    assert!(message.contains("read-only"), "{message}");

    core.close();
}
