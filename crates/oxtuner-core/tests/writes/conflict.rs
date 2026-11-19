use super::*;

/// Sixteen bytes with `value` in the first VE cell, so the ECU's tune is known.
fn page_with_first_cell(value: u8) -> Vec<u8> {
    let mut page = vec![0u8; 16];
    page[0] = value;
    page
}

fn drifted() -> SimConfig {
    SimConfig {
        pages: vec![(1, page_with_first_cell(77))],
        ..SimConfig::default()
    }
}

#[test]
fn the_first_read_reports_the_difference_and_the_write_waits_for_a_decision() {
    let dir = TempDir::new("conflict-keep");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(drifted());

    let report = core.read_all();
    assert_ne!(report.conflicts.len(), 0, "{:?}", report.conflicts);
    assert!(
        report.conflicts.iter().any(|cell| cell.name == "veTable"),
        "{:?}",
        report.conflicts
    );
    assert_eq!(core.status().conflicts, report.conflicts.len());

    // The ECU's tune is not overwritten until the user chooses.
    let (kind, message) = failure(&core.write());
    assert_eq!(kind, FailureCode::WriteRefused);
    assert!(message.contains("choose which to keep"), "{message}");

    let (kind, message) = failure(&core.burn());
    assert_eq!(kind, FailureCode::BurnRefused);
    assert!(message.contains("choose which to keep"), "{message}");

    let event = core.resolve_conflicts(oxtuner_core::ConflictResolution::UseProject);
    assert!(
        matches!(event, Event::ConflictsResolved { .. }),
        "{event:?}"
    );
    assert_eq!(core.status().conflicts, 0);
    assert!(core.status().pending_write.cells >= 1);
    assert_eq!(
        core.status().unsaved.cells,
        0,
        "the chosen project tune was saved"
    );

    let Event::TuneWritten {
        report: summary, ..
    } = core.write()
    else {
        panic!("the write after the decision must succeed");
    };
    assert!(summary.verified);
    assert_eq!(core.table().cells[0], 10.0, "the project tune reached RAM");

    let report = core.read_all();
    assert_eq!(report.conflicts.len(), 0, "{:?}", report.conflicts);

    core.close();
}

#[test]
fn using_the_ecu_values_moves_them_into_the_project() {
    let dir = TempDir::new("conflict-use-ecu");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(drifted());
    let report = core.read_all();
    assert_ne!(report.conflicts.len(), 0);

    let event = core.resolve_conflicts(oxtuner_core::ConflictResolution::UseEcu);
    assert!(
        matches!(event, Event::ConflictsResolved { .. }),
        "{event:?}"
    );
    assert_eq!(core.table().cells[0], 77.0);
    assert_eq!(core.status().conflicts, 0);
    assert_eq!(core.status().pending_write.cells, 0);
    assert_eq!(
        core.status().unsaved.cells,
        0,
        "the adopted values were saved into the project"
    );

    core.close();
}

#[test]
fn an_edit_after_the_decision_is_not_a_new_conflict() {
    let dir = TempDir::new("conflict-edit");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(drifted());
    core.read_all();
    let event = core.resolve_conflicts(oxtuner_core::ConflictResolution::UseProject);
    assert!(
        matches!(event, Event::ConflictsResolved { .. }),
        "{event:?}"
    );

    core.edit_cell(50.0);
    let report = core.read_all();

    assert!(
        report.conflicts.is_empty(),
        "a local edit is an unsent change, not a conflict: {:?}",
        report.conflicts
    );
    assert!(core.status().pending_write.cells >= 1);

    core.close();
}
