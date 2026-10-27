//! Table snapshots and edits through the core.
//!
//! The fixture is a tiny project with a 2×2 VE table and two axes. Every test
//! drives the real command loop, so the message contract is covered end to end,
//! not just the summary builder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use oxtuner_core::{
    BlockingEventStream, CellEdit, CoreConfig, CoreHandle, CoreThread, Event, FailureCode, OpId,
    TableSummary, start,
};
use oxtuner_project::TUNE_FILE;

const WAIT: Duration = Duration::from_secs(10);

const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
veTable = array, U08, 0, [2x2], "%", 1, 0, 0, 255, 0
rpmBins = array, U16, 4, [2], "rpm", 1, 0, 0, 10000, 0
mapBins = array, U08, 8, [2], "kPa", 1, 0, 0, 255, 0
curveVals = array, U08, 10, [2], "%", 1, 0, 0, 255, 0
reqFuel = scalar, U08, 12, "ms", 1, 0, 0, 255, 0

[TableEditor]
table = veTableTbl, veMap, "VE Table", 1
  xBins = rpmBins, rpm
  yBins = mapBins, map
  zBins = veTable
  xyLabels = "RPM", "MAP"

[CurveEditor]
curve = idleCurve, "Idle Curve"
  xBins = rpmBins, rpm
  yBins = curveVals

[UserDefined]
dialog = engineDialog, "Engine"
  field = "Req. Fuel", reqFuel

[Menu]
menuDialog = main
menu = "Tuning"
  subMenu = veTableTbl, "VE Table"
  subMenu = idleCurve, "Idle Curve"
  subMenu = engineDialog, "Engine"
"#;

const TUNE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <bibliography author="TunerStudio" tuneComment="fixture"/>
    <versionInfo fileFormat="5.0" firmwareInfo="Speeduino+2025.01.3" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="16">
        <constant cols="2" digits="0" name="veTable" rows="2" units="%">10.0 20.0 30.0 40.0</constant>
        <constant name="rpmBins">1000.0 2000.0</constant>
        <constant name="mapBins">50.0 100.0</constant>
        <constant name="curveVals">5.0 6.0</constant>
        <constant name="reqFuel">12.0</constant>
    </page>
</msq>
"#;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        Self::with_tune(label, TUNE)
    }

    fn with_tune(label: &str, tune: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "oxtuner-core-table-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("projectCfg")).expect("temporary project is creatable");
        fs::write(path.join("projectCfg/mainController.ini"), DEFINITION)
            .expect("the definition is writable");
        fs::write(path.join(TUNE_FILE), tune).expect("the tune is writable");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Harness {
    handle: CoreHandle,
    _thread: CoreThread,
    events: BlockingEventStream,
}

impl Harness {
    fn start() -> Self {
        let parts = start(CoreConfig::default()).expect("the core starts");
        Self {
            handle: parts.handle,
            _thread: parts.thread,
            events: parts.events.into_blocking(),
        }
    }

    fn next(&self) -> Event {
        self.events.recv_timeout(WAIT).expect("an event arrives")
    }

    fn open(&self, path: &Path) -> OpId {
        let op = self.handle.open_project(path).expect("the open is queued");
        let event = self.next();
        assert!(matches!(event, Event::ProjectOpened { .. }), "{event:?}");
        op
    }

    fn read(&self, name: &str) -> (OpId, TableSummary) {
        let op = self.handle.read_table(name).expect("the read is queued");
        match self.next() {
            Event::TableRead {
                op: answered,
                table,
            } => {
                assert_eq!(answered, op);
                (op, table)
            }
            other => panic!("expected a table read, got {other:?}"),
        }
    }
}

#[test]
fn a_table_arrives_with_cells_axes_and_units() {
    let dir = TempDir::new("read");
    let core = Harness::start();
    core.open(dir.path());

    let (_, table) = core.read("veTableTbl");

    assert_eq!(table.id, "veTableTbl");
    assert_eq!(table.name, "veTable");
    assert_eq!(table.title, "VE Table");
    assert_eq!((table.rows, table.cols), (2, 2));
    assert_eq!(table.cells, vec![10.0, 20.0, 30.0, 40.0]);
    assert_eq!((table.minimum, table.maximum), (10.0, 40.0));
    assert_eq!(table.x_axis.label, "RPM");
    assert_eq!(table.x_axis.values, vec![1000.0, 2000.0]);
    assert_eq!(table.y_axis.label, "MAP");
    assert_eq!(table.y_axis.values, vec![50.0, 100.0]);
    assert_eq!(table.y_axis.units, "kPa");
    assert!(table.writable);
    assert!(table.changed.iter().all(|changed| !changed));
}

#[test]
fn the_map_id_opens_the_same_table() {
    let dir = TempDir::new("map-id");
    let core = Harness::start();
    core.open(dir.path());

    let (_, table) = core.read("veMap");

    assert_eq!(table.id, "veTableTbl");
}

#[test]
fn an_edit_is_echoed_as_the_next_snapshot() {
    let dir = TempDir::new("edit");
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .edit_table(
            "veTable",
            vec![
                CellEdit {
                    row: 0,
                    column: 1,
                    value: 25.0,
                },
                CellEdit {
                    row: 1,
                    column: 1,
                    value: 45.0,
                },
            ],
        )
        .expect("the edit is queued");

    let Event::TableEdited {
        op: answered,
        table,
    } = core.next()
    else {
        panic!("expected a table edit");
    };
    assert_eq!(answered, op);
    assert_eq!(table.cells, vec![10.0, 25.0, 30.0, 45.0]);
    assert_eq!(
        table.changed,
        vec![false, true, false, true],
        "the edited cells differ from the file baseline"
    );
    assert_eq!(table.changed_cells(), 2);
}

#[test]
fn an_out_of_range_edit_rejects_the_whole_request() {
    let dir = TempDir::new("range");
    let core = Harness::start();
    core.open(dir.path());

    let _ = core
        .handle
        .edit_table(
            "veTable",
            vec![
                CellEdit {
                    row: 0,
                    column: 0,
                    value: 99.0,
                },
                CellEdit {
                    row: 9,
                    column: 9,
                    value: 1.0,
                },
            ],
        )
        .expect("the edit is queued");

    let Event::Failed { failure, .. } = core.next() else {
        panic!("expected a rejection");
    };
    assert_eq!(failure.code, FailureCode::TableEditRejected);

    let (_, table) = core.read("veTable");
    assert_eq!(
        table.cells,
        vec![10.0, 20.0, 30.0, 40.0],
        "the valid part of a rejected request is not applied"
    );
}

#[test]
fn an_axis_edit_is_echoed_as_the_next_snapshot() {
    use oxtuner_core::{AxisEdit, AxisKind};

    let dir = TempDir::new("axis-edit");
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .edit_axis(
            "veTableTbl",
            AxisKind::X,
            vec![AxisEdit {
                index: 1,
                value: 2500.0,
            }],
        )
        .expect("the edit is queued");

    let Event::TableEdited {
        op: answered,
        table,
    } = core.next()
    else {
        panic!("expected a table edit");
    };
    assert_eq!(answered, op);
    assert_eq!(table.x_axis.values, vec![1000.0, 2500.0]);
    assert_eq!(table.cells, vec![10.0, 20.0, 30.0, 40.0]);
}

#[test]
fn an_out_of_range_axis_edit_rejects_the_whole_request() {
    use oxtuner_core::{AxisEdit, AxisKind};

    let dir = TempDir::new("axis-range");
    let core = Harness::start();
    core.open(dir.path());

    let _ = core
        .handle
        .edit_axis(
            "veTableTbl",
            AxisKind::Y,
            vec![
                AxisEdit {
                    index: 0,
                    value: 60.0,
                },
                AxisEdit {
                    index: 9,
                    value: 1.0,
                },
            ],
        )
        .expect("the edit is queued");

    let Event::Failed { failure, .. } = core.next() else {
        panic!("expected a rejection");
    };
    assert_eq!(failure.code, FailureCode::TableEditRejected);

    let (_, table) = core.read("veTableTbl");
    assert_eq!(
        table.y_axis.values,
        vec![50.0, 100.0],
        "the valid part of a rejected request is not applied"
    );
}

#[test]
fn a_read_only_project_refuses_axis_edits() {
    use oxtuner_core::{AxisEdit, AxisKind};

    let tune = TUNE.replace(
        "signature=\"speeduino 202501\"",
        "signature=\"another firmware\"",
    );
    let dir = TempDir::with_tune("axis-read-only", &tune);
    let core = Harness::start();
    core.open(dir.path());

    let _ = core
        .handle
        .edit_axis(
            "veTableTbl",
            AxisKind::X,
            vec![AxisEdit {
                index: 0,
                value: 900.0,
            }],
        )
        .expect("the edit is queued");
    let Event::Failed { failure, .. } = core.next() else {
        panic!("expected a rejection");
    };
    assert_eq!(failure.code, FailureCode::TableEditRejected);
}

#[test]
fn a_read_only_project_refuses_edits() {
    let tune = TUNE.replace(
        "signature=\"speeduino 202501\"",
        "signature=\"another firmware\"",
    );
    let dir = TempDir::with_tune("read-only", &tune);
    let core = Harness::start();
    core.open(dir.path());

    let (_, table) = core.read("veTableTbl");
    assert!(!table.writable, "a mismatched signature opens read-only");

    let _ = core
        .handle
        .edit_table(
            "veTable",
            vec![CellEdit {
                row: 0,
                column: 0,
                value: 1.0,
            }],
        )
        .expect("the edit is queued");
    let Event::Failed { failure, .. } = core.next() else {
        panic!("expected a rejection");
    };
    assert_eq!(failure.code, FailureCode::TableEditRejected);
}

#[test]
fn a_curve_reads_as_a_one_row_table() {
    let dir = TempDir::new("curve-read");
    let core = Harness::start();
    core.open(dir.path());

    let (_, table) = core.read("idleCurve");

    assert_eq!(table.id, "idleCurve");
    assert_eq!((table.rows, table.cols), (1, 2));
    assert_eq!(table.cells, vec![5.0, 6.0]);
    assert_eq!(table.x_axis.values, vec![1000.0, 2000.0]);
    assert!(table.writable);
}

#[test]
fn a_curve_cell_edit_echoes_through_its_value_array() {
    let dir = TempDir::new("curve-edit");
    let core = Harness::start();
    core.open(dir.path());

    // The grid sends the value array's name, not the curve's: the echo must
    // still resolve back to the curve.
    let op = core
        .handle
        .edit_table(
            "curveVals",
            vec![CellEdit {
                row: 0,
                column: 1,
                value: 9.0,
            }],
        )
        .expect("the edit is queued");

    let Event::TableEdited {
        op: answered,
        table,
    } = core.next()
    else {
        panic!("expected a table edit");
    };
    assert_eq!(answered, op);
    assert_eq!(table.id, "idleCurve");
    assert_eq!(table.cells, vec![5.0, 9.0]);
}

#[test]
fn a_dialog_reads_its_scalar_rows() {
    let dir = TempDir::new("dialog-read");
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .read_dialog("engineDialog")
        .expect("the read is queued");
    let Event::DialogRead {
        op: answered,
        dialog,
    } = core.next()
    else {
        panic!("expected a dialog read");
    };

    assert_eq!(answered, op);
    assert_eq!(dialog.id, "engineDialog");
    assert_eq!(dialog.title, "Engine");
    assert_eq!(dialog.fields.len(), 1);
    assert_eq!(dialog.fields[0].label, "Req. Fuel");
    assert_eq!(dialog.fields[0].value, Some(12.0));
    assert_eq!(dialog.fields[0].units, "ms");
    assert!(dialog.fields[0].writable);
}

#[test]
fn a_scalar_edit_is_echoed_as_the_next_snapshot() {
    let dir = TempDir::new("dialog-edit");
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .edit_scalar("engineDialog", "reqFuel", 15.0)
        .expect("the edit is queued");
    let Event::DialogEdited {
        op: answered,
        dialog,
    } = core.next()
    else {
        panic!("expected a dialog edit");
    };

    assert_eq!(answered, op);
    assert_eq!(dialog.fields[0].value, Some(15.0));
    assert_eq!(dialog.changed, 1);
}

#[test]
fn a_scalar_edit_rejects_an_array_variable() {
    let dir = TempDir::new("dialog-range");
    let core = Harness::start();
    core.open(dir.path());

    let _ = core
        .handle
        .edit_scalar("engineDialog", "veTable", 1.0)
        .expect("the edit is queued");
    let Event::Failed { failure, .. } = core.next() else {
        panic!("expected a rejection");
    };
    assert_eq!(failure.code, FailureCode::DialogEditRejected);
}

#[test]
fn a_table_read_without_a_project_is_a_failure() {
    let core = Harness::start();

    let _ = core.handle.read_table("veTable").expect("queued");
    let Event::Failed { failure, .. } = core.next() else {
        panic!("expected a failure");
    };

    assert_eq!(failure.code, FailureCode::TableUnavailable);
}

#[test]
fn a_definition_only_session_cannot_read_a_table() {
    let core = Harness::start();
    let _ = core
        .handle
        .load_definition("speeduino.ini", DEFINITION.as_bytes())
        .expect("queued");
    assert!(matches!(core.next(), Event::DefinitionLoaded { .. }));

    let _ = core.handle.read_table("veTableTbl").expect("queued");
    let Event::Failed { failure, .. } = core.next() else {
        panic!("expected a failure");
    };

    assert_eq!(failure.code, FailureCode::TableUnavailable);
}
