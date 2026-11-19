use super::*;

#[test]
fn a_burn_follows_a_verified_write_and_is_journaled() {
    let dir = TempDir::new("burn");
    // The virtual board keeps reporting a queued EEPROM write for two reads,
    // the way a real one answers a burn before it touches flash. The burn may
    // only be reported verified once that bit clears.
    let config = SimConfig {
        burn_pending_samples: 2,
        ..SimConfig::default()
    };
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(config);
    core.read_all();
    core.keep_working();
    core.edit_cell(66.0);

    let event = core.write();
    assert!(matches!(event, Event::TuneWritten { .. }), "{event:?}");

    let started = Instant::now();
    let event = core.burn();
    let Event::TuneBurned {
        report: summary, ..
    } = event
    else {
        panic!("expected a verified burn, got {event:?}");
    };
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "the burn waited for the queued EEPROM write, not just for the reply"
    );
    assert!(summary.verified);
    assert!(summary.snapshot.as_ref().is_some_and(|path| path.exists()));
    assert_eq!(
        core.status().unsaved.cells,
        0,
        "a burn saves the project first"
    );

    let (_, entries) = core.journal();
    let burns: Vec<&JournalEntry> = entries
        .iter()
        .filter(|entry| entry.operation == "burn")
        .collect();
    assert_eq!(burns.len(), 2, "burn begin and result");
    assert_eq!(burns[1].result, "ok");

    assert!(
        fs::read_dir(dir.snapshots()).expect("snapshots").count() >= 2,
        "the write and the burn each snapshot first"
    );

    core.close();
}

#[test]
fn burning_before_writing_the_edit_is_refused() {
    let dir = TempDir::new("burn-order");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.keep_working();
    core.edit_cell(55.0);

    let (kind, message) = failure(&core.burn());
    assert_eq!(kind, FailureCode::BurnRefused);
    assert!(message.contains("not in ECU RAM"), "{message}");

    // No burn reached the wire, so the journal has no burn entry.
    let (_, entries) = core.journal();
    assert!(entries.iter().all(|entry| entry.operation != "burn"));

    core.close();
}

#[test]
fn no_edit_or_read_ever_burns() {
    let dir = TempDir::new("no-auto-burn");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.keep_working();
    core.edit_cell(22.0);
    let _ = core.write();
    let _ = core.table();

    let (_, entries) = core.journal();
    assert!(
        entries
            .iter()
            .all(|entry| matches!(entry.operation.as_str(), "write" | "conflict"))
    );
    assert!(
        entries.iter().all(|entry| entry.operation != "burn"),
        "nothing but an explicit burn command may burn (S5)"
    );

    core.close();
}

#[test]
fn burn_without_a_connection_is_refused() {
    let dir = TempDir::new("not-connected");
    let core = Harness::start(true);
    core.open(dir.path());

    let (kind, _) = failure(&core.burn());
    assert_eq!(kind, FailureCode::BurnRefused);

    core.close();
}
