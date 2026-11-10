use super::*;

#[test]
fn a_re_read_after_a_local_edit_reports_a_conflict() {
    let dir = TempDir::new("conflict");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.adopt();
    core.edit_cell(77.0);

    let report = core.read_all();
    assert_eq!(report.conflicts.len(), 1, "{:?}", report.conflicts);
    assert_eq!(report.conflicts[0].name, "veTable");

    let event = core.resolve_conflicts(oxtuner_core::ConflictResolution::KeepWorking);
    assert!(
        matches!(event, Event::ConflictsResolved { .. }),
        "{event:?}"
    );
    assert_eq!(core.table().cells[0], 77.0);

    core.close();
}

#[test]
fn resolving_with_the_ecu_values_restores_the_read_back() {
    let dir = TempDir::new("conflict-use-ecu");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.adopt();
    core.edit_cell(77.0);
    core.read_all();

    let event = core.resolve_conflicts(oxtuner_core::ConflictResolution::UseEcu);
    assert!(
        matches!(event, Event::ConflictsResolved { .. }),
        "{event:?}"
    );
    assert_ne!(core.table().cells[0], 77.0);

    core.close();
}
