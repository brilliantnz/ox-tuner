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
    BlockingEventStream, CellEdit, CoreConfig, CoreHandle, CoreThread, DialogFieldKind,
    DialogFieldTone, DialogPresetAssignment, DialogSummary, DialogWidgetKind, Event, FailureCode,
    OpId, TableSummary, start,
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

dialog = southDialog, "South"
  field = "Req. Fuel", reqFuel
  field = "!Values must be sane"

dialog = boardDialog, "Board"
  panel = southDialog, Center
  panel = std_injection, North
  panel = veTableTbl, South

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
        Self::with_definition(label, DEFINITION, tune)
    }

    fn with_definition(label: &str, definition: &str, tune: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "oxtuner-core-table-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("projectCfg")).expect("temporary project is creatable");
        fs::write(path.join("projectCfg/mainController.ini"), definition)
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
        let parts = start(&CoreConfig::default()).expect("the core starts");
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
                assert_eq!(answered, op, "the reply belongs to the queued read");
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
    assert_eq!(table.cell_minimum, Some(0.0));
    assert_eq!(table.cell_maximum, Some(255.0));
    assert_eq!(table.curve, None);
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
fn reverting_one_target_keeps_the_other_edits() {
    let dir = TempDir::new("revert-target");
    let core = Harness::start();
    core.open(dir.path());

    let _ = core
        .handle
        .edit_table(
            "veTable",
            vec![CellEdit {
                row: 0,
                column: 1,
                value: 25.0,
            }],
        )
        .expect("the table edit is queued");
    let Event::TableEdited { .. } = core.next() else {
        panic!("expected the table echo");
    };

    let _ = core
        .handle
        .edit_table(
            "curveVals",
            vec![CellEdit {
                row: 0,
                column: 0,
                value: 42.0,
            }],
        )
        .expect("the curve edit is queued");
    let Event::TableEdited { .. } = core.next() else {
        panic!("expected the curve echo");
    };

    let op = core
        .handle
        .revert_target("veTableTbl")
        .expect("the revert is queued");

    let Event::TableEdited {
        op: answered,
        table,
    } = core.next()
    else {
        panic!("expected the table after the revert");
    };
    assert_eq!(answered, op);
    assert_eq!(table.cells, vec![10.0, 20.0, 30.0, 40.0]);
    assert!(
        table.changed.iter().all(|changed| !changed),
        "the table matches the file baseline again"
    );
    let Event::TuneStatusRead { .. } = core.next() else {
        panic!("expected the status after the revert");
    };

    let (_, curve) = core.read("idleCurve");
    assert_eq!(
        curve.cells[0], 42.0,
        "a revert of one target leaves every other target alone"
    );
    assert!(curve.changed[0]);
}

#[test]
fn reverting_a_dialog_returns_only_its_constants() {
    let dir = TempDir::new("revert-dialog");
    let core = Harness::start();
    core.open(dir.path());

    let _ = core
        .handle
        .edit_scalar("engineDialog", "reqFuel", 30.0)
        .expect("the scalar edit is queued");
    let Event::DialogEdited { dialog, .. } = core.next() else {
        panic!("expected the dialog echo");
    };
    assert_eq!(dialog.changed, 1);

    let op = core
        .handle
        .revert_target("engineDialog")
        .expect("the revert is queued");
    let Event::DialogEdited {
        op: answered,
        dialog,
    } = core.next()
    else {
        panic!("expected the dialog after the revert");
    };
    assert_eq!(answered, op);
    assert_eq!(dialog.changed, 0);
    assert_eq!(
        dialog
            .fields
            .iter()
            .find(|field| field.variable == "reqFuel")
            .and_then(|field| field.value),
        Some(12.0),
        "the dialog's row is back to the file baseline"
    );
}

#[test]
fn reverting_a_dialog_also_reverts_the_map_it_embeds() {
    let dir = TempDir::new("revert-board");
    let core = Harness::start();
    core.open(dir.path());

    let _ = core
        .handle
        .edit_table(
            "veTable",
            vec![CellEdit {
                row: 0,
                column: 0,
                value: 66.0,
            }],
        )
        .expect("the table edit is queued");
    let Event::TableEdited { .. } = core.next() else {
        panic!("expected the table echo");
    };

    let _ = core
        .handle
        .revert_target("boardDialog")
        .expect("the revert is queued");

    let Event::TableEdited { table, .. } = core.next() else {
        panic!("expected the map after the revert");
    };
    assert_eq!(table.cells[0], 10.0, "the embedded map is back");
    assert!(table.changed.iter().all(|changed| !changed));
}

#[test]
fn reverting_a_dialog_reverts_its_constants_and_its_map() {
    let dir = TempDir::new("revert-board-both");
    let core = Harness::start();
    core.open(dir.path());

    let _ = core
        .handle
        .edit_scalar("boardDialog", "reqFuel", 30.0)
        .expect("the scalar edit is queued");
    let Event::DialogEdited { dialog, .. } = core.next() else {
        panic!("expected the dialog echo");
    };
    assert_eq!(
        dialog.changed, 2,
        "reqFuel shows twice: directly and in the composed injection panel"
    );

    let _ = core
        .handle
        .edit_table(
            "veTable",
            vec![CellEdit {
                row: 0,
                column: 0,
                value: 66.0,
            }],
        )
        .expect("the table edit is queued");
    let Event::TableEdited { .. } = core.next() else {
        panic!("expected the table echo");
    };

    let _ = core
        .handle
        .revert_target("boardDialog")
        .expect("the revert is queued");

    let mut table = None;
    let mut dialog = None;
    loop {
        match core.next() {
            Event::TableEdited { table: echo, .. } => table = Some(echo),
            Event::DialogEdited { dialog: echo, .. } => dialog = Some(echo),
            Event::TuneStatusRead { .. } if table.is_some() && dialog.is_some() => break,
            Event::TuneStatusRead { .. } => {}
            event => panic!("unexpected event {event:?}"),
        }
    }

    let table = table.expect("the embedded map is echoed");
    assert_eq!(table.cells[0], 10.0, "the embedded map is back");
    assert!(table.changed.iter().all(|changed| !changed));

    let dialog = dialog.expect("the dialog is echoed");
    assert_eq!(dialog.changed, 0, "the constants are back");
    assert_eq!(
        dialog
            .fields
            .iter()
            .find(|field| field.variable == "reqFuel")
            .and_then(|field| field.value),
        Some(12.0),
        "the constant row is back to the file baseline"
    );
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
    assert_eq!(table.cell_minimum, Some(0.0));
    assert_eq!(table.cell_maximum, Some(255.0));
    assert!(table.writable);

    let axes = table.curve.expect("a curve frame");
    assert_eq!((axes.x_min, axes.x_max), (1000.0, 2000.0));
    assert_eq!((axes.y_min, axes.y_max), (5.0, 6.0));
    assert_eq!(axes.divisions, 4);
}

#[test]
fn a_curve_axis_edit_echoes_through_its_bin_array() {
    use oxtuner_core::{AxisEdit, AxisKind};

    let dir = TempDir::new("curve-axis-edit");
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .edit_axis(
            "idleCurve",
            AxisKind::X,
            vec![AxisEdit {
                index: 0,
                value: 900.0,
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
    assert_eq!(table.x_axis.values, vec![900.0, 2000.0]);
    assert_eq!(table.cells, vec![5.0, 6.0]);
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
fn a_dialog_flattens_the_panels_it_embeds() {
    let dir = TempDir::new("dialog-panels");
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .read_dialog("boardDialog")
        .expect("the read is queued");
    let Event::DialogRead {
        op: answered,
        dialog,
    } = core.next()
    else {
        panic!("expected a dialog read");
    };

    assert_eq!(answered, op);
    assert_eq!(dialog.title, "Board");
    let labels: Vec<&str> = dialog
        .fields
        .iter()
        .map(|field| field.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["Req. Fuel", "Values must be sane", "Required Fuel"],
        "the panel's rows are the dialog's rows, plus the composed panel"
    );
    assert_eq!(dialog.fields[0].section, "South");
    assert_eq!(dialog.fields[0].value, Some(12.0));
    assert!(dialog.fields[0].writable);
    assert_eq!(dialog.fields[1].kind, DialogFieldKind::Note);
    assert_eq!(
        dialog.fields[1].tone,
        DialogFieldTone::Danger,
        "the `!` marker is a danger label"
    );
    assert_eq!(
        dialog.fields[2].section, "Injection Setup",
        "a composed `std_*` panel keeps its own title"
    );
    assert!(
        dialog.unavailable.is_empty(),
        "a composable panel is rows, not a missing name: {:?}",
        dialog.unavailable,
    );
}

#[test]
fn a_dialog_id_opens_the_table_it_embeds() {
    let dir = TempDir::new("dialog-table");
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .read_table("boardDialog")
        .expect("the read is queued");
    let Event::TableRead {
        op: answered,
        table,
    } = core.next()
    else {
        panic!("expected a table read");
    };

    assert_eq!(answered, op);
    assert_eq!(table.id, "veTableTbl");
    assert_eq!((table.cols, table.rows), (2, 2));
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

const PRESENTATION_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 20
page = 1
algo = bits, U08, 0, [0:2], "MAP", "TPS", "INVALID", "IMAP/EMAP"
blank = bits, U08, 16, [0:1], "", ""
limit = scalar, U08, 1, "units", 1, 0, 0, 100, 0
gated = scalar, U08, 2, "units", 1, 0, 0, 100, 0
both = scalar, U08, 3, "units", 1, 0, 0, 100, 0
arr = array, U08, 4, [4], "units", 1, 0, 0, 100, 0
xb = array, U08, 8, [2], "rpm", 100, 0, 0, 10000, 0
yb = array, U08, 10, [2], "kPa", 1, 0, 0, 255, 0
zc = array, U08, 12, [2x2], "%", 1, 0, 0, 255, 0

[SettingContextHelp]
algo = "Pick the load source"

[ConstantExtensions]
requiresPowerCycle = limit
requiresPowerCycle = xb
requiresPowerCycle = zc

[ControllerCommands]
cmdPing = "p"

[TableEditor]
table = embTbl, embMap, "Embedded", 1
  xBins = xb, rpm
  yBins = yb, map
  zBins = zc

[UserDefined]
dialog = presDialog, "Presentation", border
  topicHelp = "https://example.com/engine-constants"
  field = "Load source", algo
  field = "Gated", gated, { gated > 10 }
  field = "Both", both, { nCyl == 4 }, { gated > 1 }
  field = "Shown only", limit, {}, { limit > 5 }
  slider = "Limit slider", limit
  help = "A helper note"
  field = "Plain", limit
  field = "Glued gate", gated { gated > 5 }
  field = "Bare gate", both, !gated
  field = "Condvar gate", limit, gated
  field = "!Conditional warning", {}, { limit > 5 }
  field = "Never", limit, { 0 }
  field = "Visible after zero", limit, 0, { limit > 2 }
  field = "Element two", arr[2], { limit > 1 }
  checkbox = "Flipped", algo, flipValue
  runtimeValue = "Live label", rpm, { gated > 1 }
  indicator = { gated > 1 }, "Gated off", { Dynamic label }
  field = !"Outside marker", limit
  displayOnlyField = !"Blank row", blank
  slider = "Gated slider", limit, horizontal, { limit > 0 }
  checkbox = "Gated check", algo, {}, { limit > 0 }
  help = helpGeneral, "General help"
  webHelp = "https://example.com/help"
  settingSelector = "Common sensors", { limit > 0 }
  settingOption = "Low", limit=5, gated=6
  panel = presetInner
  settingOption = "High", limit=50
  commandButton = "Ping", cmdPing, { limit > 0 }
  commandButton = "Ghost", cmdMissing
  gauge = someGauge
  indicator = { gated > 1 }, "Gated off", "Gated on"
  runtimeValue = "Live", rpm
  graphLine = rpm, "RPM", 0, 8000
  readout = gated, "Gated weight", "%", 0, 100
  radio = vertical, "Radio algo", algo
  field = "Hex limit", limit, {}, {}, displayInHex
  commandButton = "Reboot", cmdPing, { limit > 0 }, closeDialogOnClick
  commandButton = "Notify", cmdPing, showMessageOnClick, "Done"
  mysteryRow = nine, ten
  panel = gatedInner { show_inner }
  panel = gatedPanel, North, { limit > 0 }
  panel = embTbl
  panel = std_ms3Rtc {rtc_mode}

dialog = gatedInner, "Inner"
  field = "Inner plain", limit

dialog = presetInner, "Preset Inner"
  field = "Between options"

dialog = gatedPanel, "Gated"
  field = "Gated plain", gated

dialog = helpInner, "Help Inner"
  topicHelp = "https://example.com/inner"
  field = "Help inner value", limit

dialog = helpParent, "Help Parent"
  panel = helpInner

[Menu]
menuDialog = main
menu = "Tuning"
  subMenu = presDialog, "Presentation"
"#;

const PRESENTATION_TUNE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <bibliography author="TunerStudio" tuneComment="fixture"/>
    <versionInfo fileFormat="5.0" firmwareInfo="Speeduino+2025.01.3" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="20">
        <constant name="algo">1.0</constant>
        <constant name="blank">0.0</constant>
        <constant name="limit">10.0</constant>
        <constant name="gated">20.0</constant>
        <constant name="both">30.0</constant>
        <constant name="arr">1.0 2.0 3.0 4.0</constant>
        <constant name="xb">1000.0 2000.0</constant>
        <constant name="yb">50.0 100.0</constant>
        <constant name="zc">10.0 20.0 30.0 40.0</constant>
    </page>
</msq>
"#;

const STD_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 4
page = 1
reqFuel = scalar, U08, 0, "ms", 1, 0, 0, 255, 0
nCylinders = scalar, U08, 1, "", 1, 0, 0, 8, 0

[UserDefined]
dialog = stdTest, "Std"
  panel = std_injection
  panel = std_bogus

[Menu]
menuDialog = main
menu = "Tuning"
  subMenu = stdTest, "Std"
"#;

const STD_TUNE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <bibliography author="TunerStudio" tuneComment="fixture"/>
    <versionInfo fileFormat="5.0" firmwareInfo="Speeduino+2025.01.3" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="4">
        <constant name="reqFuel">12.0</constant>
        <constant name="nCylinders">4.0</constant>
    </page>
</msq>
"#;

#[test]
fn a_known_std_panel_composes_while_an_unknown_one_is_named() {
    let dir = TempDir::with_definition("dialog-std", STD_DEFINITION, STD_TUNE);
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .read_dialog("stdTest")
        .expect("the read is queued");
    let Event::DialogRead {
        op: answered,
        dialog,
    } = core.next()
    else {
        panic!("expected a dialog read");
    };

    assert_eq!(answered, op);

    let labels: Vec<&str> = dialog
        .fields
        .iter()
        .map(|field| field.label.as_str())
        .collect();
    assert_eq!(labels, vec!["Required Fuel", "Cylinder Count"]);
    assert!(
        dialog
            .fields
            .iter()
            .all(|field| field.section == "Injection Setup"),
        "composed rows keep the panel title as their section"
    );
    assert_eq!(
        dialog.unavailable,
        vec!["std_bogus"],
        "only the uncomposable panel is named"
    );
}

#[test]
fn a_dialog_carries_options_conditions_help_and_topic_help() {
    let (_dir, _core, dialog) = read_pres_dialog("dialog-presentation");
    assert_eq!(
        dialog.topic_help.as_deref(),
        Some("https://example.com/engine-constants")
    );
    assert_eq!(dialog.layout.as_deref(), Some("border"));

    let field = dialog
        .fields
        .iter()
        .find(|field| field.variable == "algo")
        .expect("the bits row is present");
    assert_eq!(field.label, "Load source");
    assert_eq!(field.value, Some(1.0));

    let labels: Vec<&str> = field
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, vec!["MAP", "TPS", "IMAP/EMAP"]);
    assert_eq!(field.help.as_deref(), Some("Pick the load source"));

    let gated = dialog
        .fields
        .iter()
        .find(|field| field.variable == "gated")
        .expect("the gated row is present");
    assert_eq!(gated.visibility, None);
    assert_eq!(gated.enabled.as_deref(), Some("gated > 10"));

    let both = dialog
        .fields
        .iter()
        .find(|field| field.variable == "both")
        .expect("the two-condition row is present");
    assert_eq!(both.enabled.as_deref(), Some("nCyl == 4"));
    assert_eq!(both.visibility.as_deref(), Some("gated > 1"));

    let shown = dialog
        .fields
        .iter()
        .find(|field| field.label == "Shown only")
        .expect("the empty-slot row is present");
    assert_eq!(
        shown.visibility.as_deref(),
        Some("limit > 5"),
        "an empty enable slot keeps the visible expression in the visible slot"
    );
    assert_eq!(shown.enabled, None);

    let slider = dialog
        .fields
        .iter()
        .find(|field| field.label == "Limit slider")
        .expect("the slider row is present");
    assert_eq!(slider.minimum, Some(0.0));
    assert_eq!(slider.maximum, Some(100.0));
    assert!(
        slider.power_cycle,
        "`requiresPowerCycle = limit` reaches the row"
    );

    assert!(
        dialog
            .fields
            .iter()
            .any(|field| field.text.as_deref() == Some("A helper note")),
        "a `help` row is a note, not dropped"
    );
}

fn read_pres_dialog(label: &str) -> (TempDir, Harness, DialogSummary) {
    let dir = TempDir::with_definition(label, PRESENTATION_DEFINITION, PRESENTATION_TUNE);
    let core = Harness::start();
    core.open(dir.path());

    let op = core
        .handle
        .read_dialog("presDialog")
        .expect("the read is queued");
    let Event::DialogRead {
        op: answered,
        dialog,
    } = core.next()
    else {
        panic!("expected a dialog read");
    };

    assert_eq!(answered, op, "the reply belongs to the queued read");

    (dir, core, dialog)
}

#[test]
fn power_cycle_flags_reach_tables_and_their_axes() {
    let (_dir, core, _dialog) = read_pres_dialog("dialog-power-cycle");

    let (_, table) = core.read("embTbl");
    assert!(
        table.power_cycle,
        "`requiresPowerCycle = zc` marks the table cells"
    );
    assert!(
        table.x_axis.power_cycle,
        "`requiresPowerCycle = xb` marks the column axis"
    );
    assert!(
        !table.y_axis.power_cycle,
        "an unflagged axis stays unflagged"
    );
}

#[test]
fn a_nested_panel_keeps_its_topic_help_to_itself() {
    let (_dir, core, _dialog) = read_pres_dialog("dialog-topic-help");

    core.handle
        .read_dialog("helpParent")
        .expect("the parent read is queued");
    let Event::DialogRead { dialog, .. } = core.next() else {
        panic!("expected a parent dialog read");
    };
    assert_eq!(
        dialog.topic_help, None,
        "the embedded panel's topicHelp does not become the parent's"
    );

    core.handle
        .read_dialog("helpInner")
        .expect("the inner read is queued");
    let Event::DialogRead { dialog, .. } = core.next() else {
        panic!("expected an inner dialog read");
    };
    assert_eq!(
        dialog.topic_help.as_deref(),
        Some("https://example.com/inner"),
        "the dialog that declares topicHelp keeps it"
    );
}

#[test]
fn a_dialog_names_gated_panels_by_their_bare_name() {
    let (_dir, _core, dialog) = read_pres_dialog("dialog-gated-panels");

    assert!(
        dialog.unavailable.iter().any(|name| name == "std_ms3Rtc"),
        "a gated `std_*` panel is reported by its bare name: {:?}",
        dialog.unavailable,
    );

    let inner = dialog
        .fields
        .iter()
        .find(|field| field.label == "Inner plain")
        .expect("a gated panel still flattens");
    assert_eq!(inner.section, "Inner");
}

#[test]
fn a_dialog_resolves_glued_bare_and_conditional_gates() {
    let (_dir, _core, dialog) = read_pres_dialog("dialog-gates");
    let glued = dialog
        .fields
        .iter()
        .find(|field| field.label == "Glued gate")
        .expect("a space-joined gate resolves");
    assert_eq!(glued.variable, "gated");
    assert_eq!(glued.enabled.as_deref(), Some("gated > 5"));

    let bare = dialog
        .fields
        .iter()
        .find(|field| field.label == "Bare gate")
        .expect("a bare `!` gate resolves");
    assert_eq!(bare.variable, "both");
    assert_eq!(bare.enabled.as_deref(), Some("!gated"));

    let condvar = dialog
        .fields
        .iter()
        .find(|field| field.label == "Condvar gate")
        .expect("a bare variable gate resolves");
    assert_eq!(condvar.variable, "limit");
    assert_eq!(condvar.enabled.as_deref(), Some("gated"));

    let warning = dialog
        .fields
        .iter()
        .find(|field| field.label == "Conditional warning")
        .expect("a conditional note is carried");
    assert_eq!(warning.tone, DialogFieldTone::Danger);
    assert_eq!(warning.visibility.as_deref(), Some("limit > 5"));
    assert_eq!(warning.enabled, None);

    let never = dialog
        .fields
        .iter()
        .find(|field| field.label == "Never")
        .expect("a `{ 0 }` row is carried");
    assert_eq!(
        never.enabled.as_deref(),
        Some("0"),
        "zero is a real disabled gate, not a placeholder"
    );

    let zero_placeholder = dialog
        .fields
        .iter()
        .find(|field| field.label == "Visible after zero")
        .expect("a zero-placeholder row is carried");
    assert_eq!(
        zero_placeholder.visibility.as_deref(),
        Some("limit > 2"),
        "a bare zero keeps the enable slot empty"
    );
    assert_eq!(zero_placeholder.enabled, None);

    let element = dialog
        .fields
        .iter()
        .find(|field| field.label == "Element two")
        .expect("an array element resolves");
    assert_eq!(element.variable, "arr");
    assert_eq!(element.index, Some(2));
    assert_eq!(element.value, Some(3.0));
    assert!(
        !element.writable,
        "`EditScalar` cannot store an array element yet"
    );

    let slider = dialog
        .fields
        .iter()
        .find(|field| field.label == "Gated slider")
        .expect("a slider with a gate is carried");
    assert_eq!(
        slider.enabled.as_deref(),
        Some("limit > 0"),
        "the orientation is not a gate slot"
    );
    assert_eq!(slider.visibility, None);

    let check = dialog
        .fields
        .iter()
        .find(|field| field.label == "Gated check")
        .expect("a checkbox with a visibility gate is carried");
    assert_eq!(check.visibility.as_deref(), Some("limit > 0"));
    assert_eq!(check.enabled, None);
}

#[test]
fn a_checkbox_and_a_runtime_value_keep_their_flags_and_labels() {
    let (_dir, _core, dialog) = read_pres_dialog("dialog-flags");

    let checkbox = dialog
        .fields
        .iter()
        .find(|field| field.label == "Flipped")
        .expect("the checkbox row is present");
    assert_eq!(checkbox.kind, DialogFieldKind::Toggle);
    assert!(checkbox.flip, "`flipValue` reverses the checked state");

    let runtime = dialog
        .widgets
        .iter()
        .find(|widget| widget.kind == DialogWidgetKind::RuntimeValue)
        .expect("the runtime value is carried");
    assert_eq!(
        runtime.label, "Live label",
        "a runtime value labels its row from the first field"
    );
    assert_eq!(runtime.target, "rpm");
    assert_eq!(runtime.enabled.as_deref(), Some("gated > 1"));

    let indicator = dialog
        .widgets
        .iter()
        .find(|widget| widget.kind == DialogWidgetKind::Indicator)
        .expect("the indicator is carried");
    assert_eq!(
        indicator.label, "Dynamic label",
        "a label expression is text, not a gate"
    );
    assert_eq!(indicator.enabled, None);
    assert_eq!(indicator.visibility, None);

    let outside = dialog
        .fields
        .iter()
        .find(|field| field.label == "Outside marker")
        .expect("a marker outside the quotes is split");
    assert_eq!(outside.tone, DialogFieldTone::Danger);
    assert_eq!(outside.variable, "limit");

    let blank = dialog
        .fields
        .iter()
        .find(|field| field.label == "Blank row")
        .expect("a blank display-only constant reads as a note");
    assert_eq!(blank.kind, DialogFieldKind::Note);
    assert_eq!(blank.text.as_deref(), Some("Blank row"));
    assert_eq!(blank.tone, DialogFieldTone::Danger);
}

#[test]
fn a_dialog_carries_help_links_presets_commands_widgets_and_embeds() {
    let (_dir, _core, dialog) = read_pres_dialog("dialog-collections");

    assert!(
        dialog
            .help_links
            .iter()
            .any(|link| link.topic == "helpGeneral" && link.title == "General help"),
        "a two-field help row is a link: {:?}",
        dialog.help_links,
    );
    assert!(
        dialog
            .help_links
            .iter()
            .any(|link| link.topic == "https://example.com/help"),
        "a webHelp row is a link: {:?}",
        dialog.help_links,
    );
    assert!(
        !dialog
            .fields
            .iter()
            .any(|field| field.label == "helpGeneral"),
        "a help link is not note text"
    );

    let preset = dialog
        .presets
        .iter()
        .find(|preset| preset.label == "Common sensors")
        .expect("a setting selector is a preset");
    assert_eq!(preset.enabled.as_deref(), Some("limit > 0"));
    assert_eq!(preset.options.len(), 2);
    assert!(
        dialog
            .fields
            .iter()
            .any(|field| field.label == "Between options"),
        "a panel between selector and options still flattens"
    );
    assert_eq!(
        preset.options[0].assignments,
        vec![
            DialogPresetAssignment {
                variable: "limit".to_owned(),
                value: "5".to_owned(),
                power_cycle: true,
            },
            DialogPresetAssignment {
                variable: "gated".to_owned(),
                value: "6".to_owned(),
                power_cycle: false,
            },
        ],
    );
    assert!(
        preset.options[0].power_cycle,
        "one flagged assignment marks the whole option"
    );

    let ping = dialog
        .commands
        .iter()
        .find(|command| command.label == "Ping")
        .expect("a command button is carried");
    assert_eq!(ping.command, "cmdPing");
    assert!(ping.known);
    assert_eq!(ping.enabled.as_deref(), Some("limit > 0"));
    let ghost = dialog
        .commands
        .iter()
        .find(|command| command.label == "Ghost")
        .expect("an unknown command is carried");
    assert!(!ghost.known);
}

#[test]
fn a_dialog_carries_widgets_embeds_kinds_and_flags() {
    let (_dir, _core, dialog) = read_pres_dialog("dialog-widgets");

    assert!(
        dialog
            .widgets
            .iter()
            .any(|widget| widget.kind == DialogWidgetKind::Gauge && widget.target == "someGauge"),
        "a gauge row is carried: {:?}",
        dialog.widgets,
    );
    let indicator = dialog
        .widgets
        .iter()
        .find(|widget| widget.kind == DialogWidgetKind::Indicator && widget.label == "Gated on")
        .expect("an indicator row is carried");
    assert_eq!(indicator.target, "gated > 1");
    assert!(
        dialog
            .widgets
            .iter()
            .any(|widget| widget.kind == DialogWidgetKind::RuntimeValue && widget.target == "rpm"),
        "a runtime value is carried: {:?}",
        dialog.widgets,
    );

    assert_eq!(dialog.tables, vec!["embTbl"]);

    let algo = dialog
        .fields
        .iter()
        .find(|field| field.variable == "algo")
        .expect("the bits row is present");
    assert_eq!(algo.kind, DialogFieldKind::Select);

    let slider = dialog
        .fields
        .iter()
        .find(|field| field.label == "Limit slider")
        .expect("the slider row is present");
    assert_eq!(slider.kind, DialogFieldKind::Slider);

    let radio = dialog
        .fields
        .iter()
        .find(|field| field.label == "Radio algo")
        .expect("an orientation-first radio resolves");
    assert_eq!(radio.variable, "algo");
    assert_eq!(radio.kind, DialogFieldKind::Toggle);

    let hex = dialog
        .fields
        .iter()
        .find(|field| field.label == "Hex limit")
        .expect("a hex row is present");
    assert!(hex.hex);

    let reboot = dialog
        .commands
        .iter()
        .find(|command| command.label == "Reboot")
        .expect("a flagged button is carried");
    assert_eq!(reboot.flags, vec!["closeDialogOnClick"]);
    let notify = dialog
        .commands
        .iter()
        .find(|command| command.label == "Notify")
        .expect("a message button is carried");
    assert_eq!(notify.flags, vec!["showMessageOnClick", "Done"]);

    let trace = dialog
        .widgets
        .iter()
        .find(|widget| widget.kind == DialogWidgetKind::GraphLine)
        .expect("a graph line is carried");
    assert_eq!(trace.units, "RPM");
    assert_eq!(trace.minimum, Some(0.0));
    assert_eq!(trace.maximum, Some(8000.0));

    let readout = dialog
        .widgets
        .iter()
        .find(|widget| widget.kind == DialogWidgetKind::Readout)
        .expect("a full readout is carried");
    assert_eq!(readout.label, "Gated weight");
    assert_eq!(readout.units, "%");
    assert_eq!(readout.minimum, Some(0.0));
    assert_eq!(readout.maximum, Some(100.0));

    let gated = dialog
        .fields
        .iter()
        .find(|field| field.label == "Gated plain")
        .expect("a gated panel flattens");
    assert_eq!(gated.section, "Gated");
    assert_eq!(gated.enabled.as_deref(), Some("limit > 0"));

    assert_eq!(dialog.unmodeled.len(), 1);
    assert_eq!(dialog.unmodeled[0].keyword, "mysteryRow");
    assert_eq!(dialog.unmodeled[0].fields, vec!["nine", "ten"]);
}
